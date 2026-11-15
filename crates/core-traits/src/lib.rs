//! `biocompass-core-traits`: Cloud-agnostic core interfaces and durable local backing stores.

pub mod fs_blob_store;
pub mod ids;
pub mod ledger;
pub mod sqlite_store;
pub mod traits;
pub mod types;

pub use fs_blob_store::{LocalFsBlobStore, StandardOciPolicyVerifier};
pub use ids::{CatalogId, CreditAmountAct, NodeId, OrgId, PluginId, RunId, SessionId};
pub use ledger::SqliteActLedger;
pub use sqlite_store::SqliteSessionStore;
pub use traits::{BlobStore, OciRegistryClient, PaymentGateway, RunDispatcher, SessionStore};
pub use types::{
    ArtifactRef, CompassError, CompassResult, DatasetGrant, LedgerTx, LedgerTxKind, OciImageRef,
    PrepareRunRequest, RunLogEvent, RunQuote, RunRecord, RunStatus, SessionEvent, SessionEventKind,
    SessionRecord,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn oci_image_ref_enforces_immutable_sha256_and_rejects_adversarial_tags() {
        let valid =
            "registry.alithea.bio/alithea/hla-binding-net@sha256:8f3a9d0b1c2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f80";
        let parsed = OciImageRef::parse(valid, false).expect("valid @sha256 ref");
        assert_eq!(
            parsed.repository,
            "registry.alithea.bio/alithea/hla-binding-net"
        );
        assert_eq!(parsed.sha256_digest.as_deref().map(str::len), Some(64));

        for invalid in [
            "",
            "registry.alithea.bio/alithea/hla:latest",
            "registry.alithea.bio/alithea/hla:v1.0.0",
            "local/custom-plugin:dev",
            "@sha256:8f3a9d0b1c2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f80",
            // 63 hex chars (short)
            "registry.alithea.bio/alithea/hla@sha256:8f3a9d0b1c2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8",
            // Uppercase hex rejected per OCI canonical lowercase digest spec
            "registry.alithea.bio/alithea/hla@sha256:8F3A9D0B1C2E3F4A5B6C7D8E9F0A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F80",
        ] {
            assert!(
                OciImageRef::parse(invalid, false).is_err(),
                "strict OCI parser should have rejected '{invalid}'"
            );
        }

        let local_dev = OciImageRef::parse("local/custom-plugin:dev", true)
            .expect("local/ dev tag allowed in workstation mode");
        assert_eq!(local_dev.local_tag.as_deref(), Some("dev"));
    }

    #[tokio::test]
    async fn sqlite_session_store_persists_ordered_events_across_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("state.sqlite");
        let org = OrgId::new("org-alithea").expect("org");

        let session_id = {
            let store = SqliteSessionStore::open(&db_path).expect("open sqlite");
            let session = store
                .create_session(&org, "Lung NSCLC Cohort Analysis", "grant-full")
                .await
                .expect("create session");

            let e1 = store
                .append_event(
                    &session.session_id,
                    SessionEventKind::UserMessage,
                    "Analyze HLA-A*02:01 9-mers in Lung NSCLC",
                    json!({}),
                )
                .await
                .expect("append e1");
            let e2 = store
                .append_event(
                    &session.session_id,
                    SessionEventKind::AssistantMessage,
                    "Prepared run quote for alithea/hla-motif-enrichment.",
                    json!({"plugin_id": "alithea/hla-motif-enrichment"}),
                )
                .await
                .expect("append e2");
            assert_eq!(e1.seq, 1);
            assert_eq!(e2.seq, 2);
            session.session_id
        };

        // Reopen from disk to prove WAL durability (not an in-memory illusion)
        let reopened = SqliteSessionStore::open(&db_path).expect("reopen sqlite");
        let events = reopened.list_events(&session_id).await.expect("list events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, SessionEventKind::UserMessage);
        assert_eq!(events[1].kind, SessionEventKind::AssistantMessage);
    }

    #[tokio::test]
    async fn local_fs_blob_store_blocks_all_traversal_and_absolute_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blobs = LocalFsBlobStore::new(dir.path()).expect("blob store");

        for malicious in [
            "",
            "../escape.txt",
            "runs/../../escape.txt",
            "/etc/passwd",
            "./hidden.txt",
            "runs\\windows\\escape",
            "runs/null\0byte.json",
        ] {
            assert!(
                blobs.put_bytes(malicious, b"nope").await.is_err(),
                "malicious key '{malicious:?}' must be rejected"
            );
        }

        let sha = blobs
            .put_bytes("runs/run-1/result.json", br#"{"ok":true}"#)
            .await
            .expect("put_bytes");
        assert_eq!(sha.len(), 64);
        assert!(blobs.exists("runs/run-1/result.json").await.expect("exists"));
        let roundtrip = blobs
            .get_bytes("runs/run-1/result.json")
            .await
            .expect("get_bytes");
        assert_eq!(roundtrip, br#"{"ok":true}"#);
    }

    #[tokio::test]
    async fn act_ledger_reserves_settles_refunds_and_rejects_idempotency_collision() {
        let ledger =
            SqliteActLedger::in_memory(CreditAmountAct::from_whole_act(100)).expect("ledger");
        let org = OrgId::from("org-biotech");
        let run_ok = RunId::from("run-ok-1");
        let run_fail = RunId::from("run-fail-2");

        assert_eq!(
            ledger.available_balance(&org).await.expect("bal"),
            CreditAmountAct::from_whole_act(100)
        );

        ledger
            .reserve_for_run(&org, &run_ok, CreditAmountAct::from_whole_act(15), "idem-1")
            .await
            .expect("reserve 1");
        // Same (run_id, idempotency_key) -> safe idempotent replay
        ledger
            .reserve_for_run(&org, &run_ok, CreditAmountAct::from_whole_act(15), "idem-1")
            .await
            .expect("idempotent replay must not double-deduct");
        // Different run_id reusing idem-1 -> MUST FAIL LOUD
        let collision = ledger
            .reserve_for_run(
                &org,
                &RunId::from("run-hijack"),
                CreditAmountAct::from_whole_act(15),
                "idem-1",
            )
            .await;
        assert!(
            matches!(collision, Err(CompassError::Payment(_))),
            "reusing idempotency_key across different run_ids must fail"
        );

        assert_eq!(
            ledger.available_balance(&org).await.expect("bal after reserve"),
            CreditAmountAct::from_whole_act(85)
        );

        ledger
            .settle_run(&org, &run_ok, "ed86f83b5a1b4e56")
            .await
            .expect("settle");
        // Double settle must fail
        assert!(ledger.settle_run(&org, &run_ok, "ed86f83b5a1b4e56").await.is_err());

        ledger
            .reserve_for_run(&org, &run_fail, CreditAmountAct::from_whole_act(25), "idem-2")
            .await
            .expect("reserve 2");
        assert_eq!(
            ledger.available_balance(&org).await.expect("bal mid"),
            CreditAmountAct::from_whole_act(60)
        );
        ledger
            .refund_run(&org, &run_fail, "container exited non-zero")
            .await
            .expect("refund");
        assert_eq!(
            ledger.available_balance(&org).await.expect("bal refunded"),
            CreditAmountAct::from_whole_act(85)
        );

        ledger
            .credit_topup(
                &org,
                CreditAmountAct::from_whole_act(50),
                "PO-2026-ALITHEA-0042",
                false,
            )
            .await
            .expect("topup");
        assert_eq!(
            ledger.available_balance(&org).await.expect("bal topup"),
            CreditAmountAct::from_whole_act(135)
        );
    }

    #[tokio::test]
    async fn act_ledger_prevents_overdraft_under_concurrent_reservation_race() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = SqliteActLedger::open(
            dir.path().join("ledger.sqlite"),
            CreditAmountAct::from_whole_act(100),
        )
        .expect("open disk ledger");
        let org = OrgId::from("org-race-tenant");

        // Prime the org genesis balance first
        assert_eq!(
            ledger.available_balance(&org).await.expect("prime"),
            CreditAmountAct::from_whole_act(100)
        );

        // Spawn 10 concurrent reservations of 20 ACT each against a 100 ACT balance
        let mut handles = Vec::new();
        for idx in 0..10 {
            let l = ledger.clone();
            let o = org.clone();
            handles.push(tokio::spawn(async move {
                let run_id = RunId::from(format!("run-race-{idx}"));
                let idem = format!("idem-race-{idx}");
                l.reserve_for_run(&o, &run_id, CreditAmountAct::from_whole_act(20), &idem)
                    .await
                    .map(|_| run_id)
            }));
        }

        let mut winners = Vec::new();
        let mut losers = 0usize;
        for h in handles {
            match h.await.expect("join task") {
                Ok(run_id) => winners.push(run_id),
                Err(CompassError::Payment(_)) => losers += 1,
                Err(other) => panic!("unexpected error variant: {other}"),
            }
        }

        assert_eq!(winners.len(), 5, "exactly 5 of 10 20-ACT reservations may succeed");
        assert_eq!(losers, 5, "remaining 5 reservations must fail with insufficient balance");
        assert_eq!(
            ledger.available_balance(&org).await.expect("depleted"),
            CreditAmountAct::ZERO
        );

        // Settle 3 winners and refund 2 winners -> exact 40 ACT available!
        for (i, run_id) in winners.iter().enumerate() {
            if i < 3 {
                ledger
                    .settle_run(&org, run_id, "receipt-sha256")
                    .await
                    .expect("settle winner");
            } else {
                ledger
                    .refund_run(&org, run_id, "simulated container failure")
                    .await
                    .expect("refund winner");
            }
        }
        assert_eq!(
            ledger.available_balance(&org).await.expect("conserved"),
            CreditAmountAct::from_whole_act(40)
        );
    }
}
