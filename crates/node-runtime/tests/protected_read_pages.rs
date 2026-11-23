use async_trait::async_trait;
use biocompass_core_traits::{
    CatalogId, CompassError, CompassResult, DatasetGrant, FilterExpr, OrgId, RunRecord,
};
use biocompass_data_engine::{DuckLakeCatalogEngine, SealedSnapshot};
use biocompass_node_runtime::{LiveQueryAuthority, LiveQueryBinding, LiveQuerySession};
use biocompass_oci_runner::LiveQueryResponder;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Notify;

const ROSTER: &str = "sample_roster.v2";
const COUNTS: &str = "sample_observation_counts.v2";
const DECLARED: &[&str] = &[
    ROSTER,
    COUNTS,
    "sample_roster.v1",
    "sample_observation_counts.v1",
];

#[derive(Default)]
struct Authority {
    revoked: AtomicBool,
    audit: Mutex<Value>,
    checks: AtomicUsize,
    pause_after_query: Option<(Notify, Notify)>,
}

#[async_trait]
impl LiveQueryAuthority for Authority {
    async fn ensure_allowed(&self) -> CompassResult<()> {
        if let Some((queried, resume)) = &self.pause_after_query
            && self.checks.fetch_add(1, Ordering::SeqCst) == 1
        {
            queried.notify_one();
            resume.notified().await;
        }
        if self.revoked.load(Ordering::SeqCst) {
            return Err(CompassError::AccessDenied("grant revoked".into()));
        }
        Ok(())
    }

    async fn save_audit(&self, document: &[u8]) -> CompassResult<()> {
        *self.audit.lock().unwrap() = serde_json::from_slice(document).unwrap();
        Ok(())
    }
}

fn session(
    catalog: &DuckLakeCatalogEngine,
    snapshot: &SealedSnapshot,
    org: &str,
    sample_ids: Vec<String>,
    authority: Arc<Authority>,
    declared: &[&str],
) -> CompassResult<LiveQuerySession> {
    let mut grant =
        DatasetGrant::compute_only_partner(OrgId::from(org), catalog.summary().id.clone());
    grant.grant_id = format!("grant-{org}");
    grant.row_scope = Some(FilterExpr::in_values("original_sample_number", sample_ids));
    let run: RunRecord = serde_json::from_value(json!({
        "run_id": format!("run-{org}"), "org_id": org, "plugin_id": "synthetic/reader",
        "plugin_version": "1", "status": "running", "params": {}, "grant": grant,
        "created_at": "2026-01-01T00:00:00Z", "artifacts": [], "froglet_chain": [],
        "receipt_verified": false, "logs": [],
        "quote": {
            "run_id": format!("run-{org}"), "plugin_id": "synthetic/reader", "plugin_version": "1",
            "image_ref": "synthetic/reader", "target_node_id": "synthetic-node",
            "estimated_cost_act": 0, "workload_hash": "11".repeat(32),
            "froglet_quote": {}, "expires_at": "2026-01-01T00:05:00Z"
        }
    }))
    .unwrap();
    LiveQuerySession::new(
        &run,
        LiveQueryBinding {
            catalog: catalog.clone(),
            version: 1,
            snapshot: snapshot.snapshot_id,
            digest: snapshot.dataset_digest.clone(),
        },
        declared.iter().map(|name| (*name).to_owned()).collect(),
        8 * 1024 * 1024,
        authority,
    )
}

async fn read(
    session: &LiveQuerySession,
    request_id: &str,
    query: &str,
    params: Value,
) -> CompassResult<Value> {
    let frame =
        serde_json::to_vec(&json!({ "request_id": request_id, "query": query, "params": params }))
            .unwrap();
    let reply: Value = serde_json::from_slice(&session.call(&frame).await?).unwrap();
    assert_eq!(reply["request_id"], request_id);
    Ok(reply)
}

fn sample_id(i: usize) -> String {
    format!("S{i:04}")
}

#[tokio::test]
async fn protected_pages_are_complete_scoped_pinned_and_revocable() -> CompassResult<()> {
    let temp = tempfile::tempdir().unwrap();
    let mut catalog = DuckLakeCatalogEngine::create_local_catalog(
        temp.path(),
        CatalogId::from("synthetic/protected-pages"),
        "Synthetic roster",
        "1",
        "",
        "",
        Default::default(),
        "synthetic test data",
        |conn| {
            conn.execute_batch(
                "
            CREATE TABLE dl.obs_samples AS
            SELECT printf('S%04d', i) AS original_sample_number,
                   CASE WHEN i % 2 = 0 THEN 'even' ELSE 'odd' END AS sample_class
            FROM range(1004) r(i) ORDER BY i DESC;
            CREATE TABLE dl.obs_sample_peptide_mappings AS
            SELECT * FROM (VALUES ('S0000'), ('S0001'), ('S1001'), ('S1001'), ('S1003'))
              r(original_sample_number);",
            )
            .map_err(|error| CompassError::Storage(error.to_string()))
        },
    )?;
    let pin = catalog.load_sealed_snapshot(None)?;
    let authority_a = Arc::new(Authority::default());
    let authority_b = Arc::new(Authority::default());
    let authorized: Vec<_> = (1..=1002).map(sample_id).collect();
    let a = session(
        &catalog,
        &pin,
        "org-a",
        authorized.clone(),
        authority_a.clone(),
        DECLARED,
    )?;
    let b = session(
        &catalog,
        &pin,
        "org-b",
        vec![sample_id(0), sample_id(1003), sample_id(1003)],
        authority_b,
        DECLARED,
    )?;

    // Hold an active run after SQL has produced rows, then revoke its grant while
    // a second organization's query on the same snapshot still completes.
    let gated_auth = Arc::new(Authority {
        pause_after_query: Some(Default::default()),
        ..Default::default()
    });
    let gated = session(
        &catalog,
        &pin,
        "org-changing",
        vec![sample_id(1), sample_id(2)],
        gated_auth.clone(),
        DECLARED,
    )?;
    let (denied, allowed) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(read(&gated, "during-revoke", COUNTS, json!({})), async {
            let (queried, resume) = gated_auth.pause_after_query.as_ref().unwrap();
            queried.notified().await;
            gated_auth.revoked.store(true, Ordering::SeqCst);
            let reply = read(&b, "other-org", COUNTS, json!({})).await;
            resume.notify_one();
            reply
        })
    })
    .await
    .expect("both organizations should make progress");
    assert!(matches!(denied, Err(CompassError::AccessDenied(_))));
    assert_eq!(
        allowed?["rows"],
        json!([
            {"sample_id":"S0000","observations":1}, {"sample_id":"S1003","observations":1}
        ])
    );
    let denied_call = gated_auth.audit.lock().unwrap()["calls"][0].clone();
    assert_eq!(denied_call["state"], "denied");
    assert!(denied_call.get("response_sha256").is_none());

    for (query, prefix) in [(COUNTS, "counts"), (ROSTER, "roster")] {
        let first = read(&a, &format!("{prefix}-1"), query, json!({})).await?;
        assert_eq!(first["rows"].as_array().unwrap().len(), 1000);
        assert_eq!(first["next_after_sample_id"], "S1000");
        let next = json!({"after_sample_id": first["next_after_sample_id"]});
        if query == COUNTS {
            let updated = catalog.commit_mutation("append observation for S1002", |conn| {
                conn.execute_batch("INSERT INTO dl.obs_sample_peptide_mappings VALUES ('S1002')")
                    .map_err(|error| CompassError::Storage(error.to_string()))
            })?;
            assert_ne!(updated.dataset_digest, pin.dataset_digest);
            assert_ne!(updated.snapshot_id, pin.snapshot_id);
        }
        let last = read(&a, &format!("{prefix}-2"), query, next.clone()).await?;
        assert!(last.get("next_after_sample_id").is_none());
        let ids = first["rows"]
            .as_array()
            .unwrap()
            .iter()
            .chain(last["rows"].as_array().unwrap())
            .map(|row| row["sample_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(ids, authorized);
        if query == COUNTS {
            assert_eq!(
                last["rows"],
                json!([
                    {"sample_id":"S1001","observations":2},{"sample_id":"S1002","observations":0}
                ])
            );
        }
        let replay = read(&a, &format!("{prefix}-replay"), query, next.clone()).await?;
        assert_eq!(replay["rows"], last["rows"]);
        // A cursor is only a position. The other run always applies its own grant.
        let other = read(&b, &format!("{prefix}-b"), query, next).await?;
        assert_eq!(other["rows"].as_array().unwrap().len(), 1);
        assert_eq!(other["rows"][0]["sample_id"], "S1003");
        assert!(other.get("next_after_sample_id").is_none());
    }

    let filtered = read(
        &a,
        "filter",
        ROSTER,
        json!({"sample_class":"even","page_size":1,"after_sample_id":"S1000"}),
    )
    .await?;
    assert_eq!(
        filtered["rows"],
        json!([{"sample_id":"S1002","sample_class":"even"}])
    );
    assert!(filtered.get("next_after_sample_id").is_none());
    for size in [0, 1001] {
        assert!(matches!(
            read(
                &a,
                &format!("bad-size-{size}"),
                ROSTER,
                json!({"page_size":size})
            )
            .await,
            Err(CompassError::Validation(_))
        ));
    }
    let old_roster = read(&b, "old-roster", "sample_roster.v1", json!({})).await?;
    assert_eq!(
        old_roster,
        json!({"request_id":"old-roster","rows":[
        {"sample_id":"S0000","sample_class":"even"},{"sample_id":"S1003","sample_class":"odd"}]})
    );
    assert!(matches!(
        read(&a, "old-overflow", "sample_roster.v1", json!({})).await,
        Err(CompassError::Validation(_))
    ));
    let old_count = read(
        &a,
        "old-count",
        "sample_observation_counts.v1",
        json!({"sample_id":"S1001"}),
    )
    .await?;
    assert_eq!(
        old_count,
        json!({"request_id":"old-count","rows":[{"sample_id":"S1001","observations":2}]})
    );
    assert!(matches!(
        read(
            &a,
            "old-count-no-id",
            "sample_observation_counts.v1",
            json!({})
        )
        .await,
        Err(CompassError::Validation(_))
    ));

    let exact = read(&b, "exact", COUNTS, json!({"page_size":2})).await?;
    assert_eq!(
        exact["rows"],
        json!([
            {"sample_id":"S0000","observations":1},{"sample_id":"S1003","observations":1}
        ])
    );
    assert!(exact.get("next_after_sample_id").is_none());
    assert!(
        read(&b, "old-size", "sample_roster.v1", json!({"page_size":1}))
            .await
            .is_err()
    );
    let legacy = session(
        &catalog,
        &pin,
        "org-empty",
        vec![],
        Arc::default(),
        &["sample_roster.v1"],
    )?;
    assert_eq!(
        read(&legacy, "empty", "sample_roster.v1", json!({})).await?["rows"],
        json!([])
    );
    assert!(matches!(
        read(&legacy, "undeclared", ROSTER, json!({})).await,
        Err(CompassError::Validation(_))
    ));

    let before = read(&a, "before-revoke", ROSTER, json!({"page_size":1})).await?;
    let continuation = json!({"after_sample_id":before["next_after_sample_id"],"page_size":1});
    authority_a.revoked.store(true, Ordering::SeqCst);
    assert!(matches!(
        read(&a, "after-revoke", ROSTER, continuation.clone()).await,
        Err(CompassError::AccessDenied(_))
    ));
    let audit = authority_a.audit.lock().unwrap();
    assert_eq!(audit["dataset_version"], 1);
    assert_eq!(audit["snapshot_id"], pin.snapshot_id);
    assert_eq!(audit["dataset_digest"], pin.dataset_digest);
    assert_eq!(audit["grant_id"], "grant-org-a");
    let calls = audit["calls"].as_array().unwrap();
    let second_page = calls
        .iter()
        .find(|call| call["request_id"] == "counts-2")
        .unwrap();
    assert_eq!(second_page["state"], "succeeded");
    assert_eq!(second_page["params"], json!({"after_sample_id":"S1000"}));
    assert_eq!(second_page["response_sha256"].as_str().unwrap().len(), 64);
    let denied = calls.last().unwrap();
    assert_eq!(denied["request_id"], "after-revoke");
    assert_eq!(denied["state"], "denied");
    assert_eq!(denied["params"], continuation);
    assert!(denied.get("response_sha256").is_none());
    Ok(())
}
