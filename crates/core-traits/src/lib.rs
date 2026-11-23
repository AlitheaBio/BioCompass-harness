//! `biocompass-core-traits`: Cloud-agnostic core interfaces and durable local backing stores.

pub mod commitments;
pub mod error;
pub mod fs_blob_store;
pub mod ids;
pub use commitments::{
    EXTENSION_REF_CHILD_PREFIX, EXTENSION_REF_INPUT_PREFIX, EXTENSION_REF_PARENT_PREFIX,
    ExtensionRefStage, ParsedExtensionRef, WorkloadDescriptor, compute_canonical_workload_hash,
    expected_extension_refs, parse_extension_ref, result_manifest_hash, verify_extension_refs,
};
pub use error::{
    ERR_ACCESS_DENIED, ERR_AGENT_CEILING_EXCEEDED, ERR_AGENT_CREDENTIAL_EXPIRED,
    ERR_AGENT_CREDENTIAL_REVOKED, ERR_CHILD_MODULE_NOT_ALLOWED, ERR_CONTAINER_ENGINE_ERROR,
    ERR_DAILY_ACT_BREAKER_EXCEEDED, ERR_DOC_VERSION_CONFLICT, ERR_IDEMPOTENCY_CONFLICT,
    ERR_INPUT_LIMIT_EXCEEDED, ERR_INSUFFICIENT_FUNDS, ERR_INTEGRITY_VIOLATION, ERR_INTERNAL_ERROR,
    ERR_NOT_FOUND, ERR_OFF_LIST_CATALOG, ERR_OFF_LIST_MODULE, ERR_PAYMENT_ERROR,
    ERR_PROTOCOL_ERROR, ERR_RATE_LIMITED, ERR_ROW_BUDGET_EXCEEDED, ERR_STORAGE_ERROR,
    ERR_UI_BUNDLE_TAMPERED, ERR_UNAUTHENTICATED, ERR_UPSTREAM_LLM_ERROR, ERR_VALIDATION_ERROR,
    ErrorCode,
};
#[cfg(feature = "local-sqlite")]
pub mod ledger;
#[cfg(feature = "local-sqlite")]
pub mod sqlite_store;
pub mod traits;
pub mod types;
pub mod run_events;

pub use fs_blob_store::{LocalFsBlobStore, StandardOciPolicyVerifier};
pub use ids::{CatalogId, CreditAmountAct, NodeId, OrgId, PluginId, ProjectId, RunId, SessionId};
#[cfg(feature = "local-sqlite")]
pub use ledger::SqliteActLedger;
#[cfg(feature = "local-sqlite")]
pub use sqlite_store::{SqliteSessionStore, local_delivery};
pub use traits::{BlobStore, OciRegistryClient, PaymentGateway, RunDispatcher, SessionStore};
pub use types::{
    ArtifactRef, AttachedCatalogPin, ColumnDescriptor, CompassError, CompassResult, DatasetGrant,
    DatasetHostingMode, DatasetOfferRecord, DatasetOfferScope, FilterExpr, GrantLevel, IngestFormat, IngestMode,
    LedgerTx, LedgerTxKind, MAX_RUN_INPUTS, ModuleListingMeta, ModuleRunsIn, ModuleSource,
    OciImageRef, PrepareRunRequest, ProjectRecord, QueryAst, RunInput, RunLogEvent,
    ReservationState, RunAdmission, RunQuote, RunRecord, RunReservation, RunStatus, SavedViewPlane, SavedViewRecord, SavedViewSourcePin,
    SavedViewStatus, SavedViewVerificationReport, ScalarValue, SessionEvent, SessionEventKind,
    ScopedQueryDefinition, SessionRecord, SortDirection, SortSpec, TableSchema,
    canonicalize_run_inputs, is_sha256_hex, validate_requested_plugin_version,
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

    #[test]
    fn sec_23_oci_image_ref_parse_rejects_traversal_query_fragment_userinfo_and_control_chars() {
        let valid_sha = "8f3a9d0b1c2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f80";
        for valid in [
            format!("ghcr.io/alitheabio/hla-motif-enrichment@sha256:{valid_sha}"),
            format!("127.0.0.1:5500/alithea/hla-motif-enrichment@sha256:{valid_sha}"),
            format!("[::1]:5000/alithea/hla-motif-enrichment@sha256:{valid_sha}"),
            format!("localhost:5000/alithea/hla-motif-enrichment@sha256:{valid_sha}"),
            format!("test/echo@sha256:{valid_sha}"),
        ] {
            assert!(OciImageRef::parse(&valid, false).is_ok(), "should accept {valid}");
        }
        for bad in [
            format!("registry.example/org/../secret@sha256:{valid_sha}"),
            format!("registry.example/org/./img@sha256:{valid_sha}"),
            format!("registry.example/org/img?foo=bar@sha256:{valid_sha}"),
            format!("registry.example/org/img#frag@sha256:{valid_sha}"),
            format!("user:pass@registry.example/org/img@sha256:{valid_sha}"),
            format!("registry.example/org/img\r\nX-Injected:1@sha256:{valid_sha}"),
            format!("registry.example/org/img with space@sha256:{valid_sha}"),
            format!("registry.example/org//img@sha256:{valid_sha}"),
            format!("registry.example/org\\img@sha256:{valid_sha}"),
            format!("https://registry.example/org/img@sha256:{valid_sha}"),
            format!("registry.example/org/img:tag@sha256:{valid_sha}"),
        ] {
            assert!(
                OciImageRef::parse(&bad, false).is_err(),
                "SEC-23 parser must reject '{bad}'"
            );
        }
        assert!(OciImageRef::parse("local/../escape:dev", true).is_err());
        assert!(OciImageRef::parse("local/ok:bad?tag", true).is_err());
    }

    #[cfg(feature = "local-sqlite")]
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

    #[cfg(feature = "local-sqlite")]
    #[tokio::test]
    async fn sqlite_session_and_project_crud_works_and_migrates_legacy_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("legacy.sqlite");
        // Seed a legacy SQLite DB without `projects` table or `sessions.project_id` column.
        {
            let conn = rusqlite::Connection::open(&db_path).expect("raw sqlite");
            conn.execute_batch(
                "CREATE TABLE sessions (
                    session_id TEXT PRIMARY KEY,
                    org_id TEXT NOT NULL,
                    title TEXT NOT NULL,
                    active_grant_id TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                INSERT INTO sessions VALUES (
                    'ses-legacy', 'org-alithea', 'Legacy Chat', 'full',
                    '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
                );",
            )
            .expect("seed legacy schema");
        }

        let store = SqliteSessionStore::open(&db_path).expect("migrate legacy db");
        let org = OrgId::new("org-alithea").expect("org");
        let legacy_sid = SessionId::new("ses-legacy").expect("sid");

        let legacy = store.get_session(&legacy_sid).await.expect("get legacy");
        assert_eq!(legacy.title, "Legacy Chat");
        assert!(legacy.project_id.is_none());

        let project = store
            .create_project(&org, "Melanoma Neoantigens")
            .await
            .expect("create project");
        let renamed_proj = store
            .rename_project(&project.project_id, "Melanoma Cohort Q4")
            .await
            .expect("rename project");
        assert_eq!(renamed_proj.name, "Melanoma Cohort Q4");

        let updated = store
            .update_session(
                &legacy_sid,
                Some("Renamed Legacy Chat"),
                None,
                Some(Some(&project.project_id)),
            )
            .await
            .expect("rename and assign project");
        assert_eq!(updated.title, "Renamed Legacy Chat");
        assert_eq!(updated.project_id.as_ref(), Some(&project.project_id));

        // Deleting project ungroups the session rather than deleting it.
        store
            .delete_project(&project.project_id)
            .await
            .expect("delete project");
        let after_proj_del = store.get_session(&legacy_sid).await.expect("session intact");
        assert!(after_proj_del.project_id.is_none());

        // Deleting the session removes it cleanly.
        store
            .delete_session(&legacy_sid)
            .await
            .expect("delete session");
        assert!(store.get_session(&legacy_sid).await.is_err());
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

    #[cfg(feature = "local-sqlite")]
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
        // A different signed receipt cannot supersede a completed settlement.
        assert!(ledger.settle_run(&org, &run_ok, "different-receipt").await.is_err());

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

    #[cfg(feature = "local-sqlite")]
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

    #[test]
    fn compass_error_formats_user_message_code_and_hint() {
        let missing_run = CompassError::NotFound("run r-123".into());
        assert_eq!(missing_run.code(), "NOT_FOUND");
        assert_eq!(missing_run.user_message(), "Run r-123 not found.");

        let missing_plugin = CompassError::NotFound("plugin 'x' is not registered".into());
        assert_eq!(missing_plugin.user_message(), "Plugin 'x' is not registered");

        let missing_llm = CompassError::UpstreamLlm("No LLM backend is configured yet.".into());
        assert_eq!(missing_llm.code(), "UPSTREAM_LLM_ERROR");
        assert_eq!(missing_llm.user_message(), "No LLM backend is configured yet.");
        assert_eq!(
            missing_llm.hint(),
            Some("Open Settings -> Agent to select and configure a model provider.")
        );

        let denied = CompassError::AccessDenied("organ 'brain' is not allowed".into());
        assert_eq!(denied.code(), "ACCESS_DENIED");
        assert_eq!(denied.user_message(), "Access denied: Organ 'brain' is not allowed");
        assert!(denied.hint().is_some());

        let already_prefixed = CompassError::AccessDenied("Access denied by policy".into());
        assert_eq!(already_prefixed.user_message(), "Access denied by policy");

        let payment = CompassError::Payment("insufficient ACT balance".into());
        assert_eq!(payment.code(), "PAYMENT_ERROR");
        assert_eq!(
            payment.user_message(),
            "Credit & billing error: Insufficient ACT balance"
        );
        assert_eq!(
            payment.hint(),
            Some("Check your available ACT credits and daily ACT breaker in Settings -> Usage.")
        );

        let limited = CompassError::RateLimited("burst rpm exceeded".into());
        assert_eq!(limited.code(), "RATE_LIMITED");
        assert!(limited.hint().is_some());
    }

    #[test]
    fn ux_cloud_web_only_error_omits_unrelated_dataset_grant_hint() {
        let cloud_only =
            CompassError::AccessDenied("this endpoint is only available in cloud-web mode".into());
        let hint = cloud_only.hint().unwrap_or_default();
        assert!(
            !hint.contains("Dataset Grant"),
            "cloud-web mode error should not display Dataset Grant hint, got: {hint}"
        );
    }

    #[test]
    fn stable_error_codes_roundtrip_and_classify_independently_of_currency_text() {
        let insuf = CompassError::insufficient_funds("need 12.50 EUR, available 4.00 EUR");
        assert_eq!(insuf.error_code(), ErrorCode::InsufficientFunds);
        assert_eq!(insuf.code(), ERR_INSUFFICIENT_FUNDS);
        assert!(insuf.is_code(ErrorCode::InsufficientFunds));
        assert!(matches!(insuf, CompassError::Payment(_)));
        assert!(insuf.hint().is_some());

        let ceiling = CompassError::agent_ceiling_exceeded("daily spend cap reached");
        assert_eq!(ceiling.error_code(), ErrorCode::AgentCeilingExceeded);
        assert_eq!(ceiling.code(), ERR_AGENT_CEILING_EXCEEDED);
        assert!(matches!(ceiling, CompassError::Payment(_)));
        assert!(ceiling.hint().is_some());

        let off_mod = CompassError::off_list_module("module 'org/x' not in credential allowlist");
        assert_eq!(off_mod.error_code(), ErrorCode::OffListModule);
        assert!(matches!(off_mod, CompassError::AccessDenied(_)));

        let off_cat = CompassError::off_list_catalog("catalog 'org/c' not in credential allowlist");
        assert_eq!(off_cat.error_code(), ErrorCode::OffListCatalog);
        assert!(matches!(off_cat, CompassError::AccessDenied(_)));

        let tampered = CompassError::ui_bundle_tampered("bundle SHA-256 mismatch");
        assert_eq!(tampered.error_code(), ErrorCode::UiBundleTampered);
        assert!(matches!(tampered, CompassError::IntegrityViolation(_)));

        let child = CompassError::child_module_not_allowed("child module not declared");
        assert_eq!(child.error_code(), ErrorCode::ChildModuleNotAllowed);
        assert!(matches!(child, CompassError::AccessDenied(_)));

        let conflict = CompassError::doc_version_conflict("expected base_version 3");
        assert_eq!(conflict.error_code(), ErrorCode::DocVersionConflict);
        assert!(matches!(conflict, CompassError::IntegrityViolation(_)));

        for code in [
            ErrorCode::InsufficientFunds,
            ErrorCode::DailyActBreakerExceeded,
            ErrorCode::AgentCeilingExceeded,
            ErrorCode::AgentCredentialRevoked,
            ErrorCode::AgentCredentialExpired,
            ErrorCode::IdempotencyConflict,
            ErrorCode::OffListModule,
            ErrorCode::OffListCatalog,
            ErrorCode::UiBundleTampered,
            ErrorCode::ChildModuleNotAllowed,
            ErrorCode::DocVersionConflict,
            ErrorCode::InputLimitExceeded,
            ErrorCode::RowBudgetExceeded,
            ErrorCode::Unauthenticated,
        ] {
            assert_eq!(ErrorCode::parse(code.as_str()), Some(code));
        }
    }

    #[test]
    fn run_input_validation_enforces_limits_and_path_safety() {
        let valid = RunInput {
            alias: "cohort_a".into(),
            source_run_id: "run-123".into(),
            name: "nested/result.parquet".into(),
            sha256: "ab".repeat(32),
            size_bytes: 128,
            source_receipt_hash: "cd".repeat(32),
        };
        assert_eq!(
            canonicalize_run_inputs(std::slice::from_ref(&valid)).unwrap(),
            vec![valid.clone()]
        );

        let mut bad_alias = valid.clone();
        bad_alias.alias = "../escape".into();
        assert!(canonicalize_run_inputs(&[bad_alias]).is_err());

        let mut bad_name = valid.clone();
        bad_name.name = "a/../b".into();
        assert!(canonicalize_run_inputs(&[bad_name]).is_err());

        let mut bad_sha = valid.clone();
        bad_sha.sha256 = "AB".repeat(32);
        assert!(canonicalize_run_inputs(&[bad_sha]).is_err());

        // Duplicate stage path rejected
        assert!(canonicalize_run_inputs(&[valid.clone(), valid.clone()]).is_err());

        // Exceeding MAX_RUN_INPUTS rejected
        let too_many: Vec<RunInput> = (0..=MAX_RUN_INPUTS)
            .map(|i| RunInput {
                alias: format!("in-{i}"),
                ..valid.clone()
            })
            .collect();
        assert!(canonicalize_run_inputs(&too_many).is_err());
    }

    #[test]
    fn module_listing_meta_and_prepare_request_serde_defaults_remain_backwards_compatible() {
        let meta: ModuleListingMeta = serde_json::from_value(json!({})).unwrap();
        assert_eq!(meta.source, ModuleSource::Workspace);
        assert_eq!(meta.runs_in, ModuleRunsIn::Local);
        assert!(!meta.can_install);
        assert!(!meta.installed_locally);
        assert!(meta.ui_bundle_hash.is_none());
        assert!(meta.ui_bundle_size_bytes.is_none());
        assert!(meta.provider_label.is_none());

        let legacy_req: PrepareRunRequest = serde_json::from_value(json!({
            "org_id": "org-1",
            "session_id": null,
            "plugin_id": "alithea/demo",
            "params": {},
            "dataset_grant": null
        }))
        .unwrap();
        assert!(legacy_req.inputs.is_empty());
        assert!(legacy_req.parent_run_id.is_none());
        assert!(legacy_req.agent_credential_id.is_none());
        assert!(legacy_req.agent_tool_call_id.is_none());
        assert!(legacy_req.doc_id.is_none());
        assert!(legacy_req.doc_version.is_none());
    }
}
