use biocompass_core_traits::{CatalogId, CompassError, DatasetGrant, OrgId};
use biocompass_data_engine::{
    CatalogSummary, ColumnDescriptor, DuckLakeCatalogEngine, QueryAst, SealedFilePair,
    SealedSnapshot, SealedTableSnapshot, ShardCache, TableSchema, build_sealed_table_scan_sql,
    validate_run_query,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[tokio::test]
async fn duplicate_cold_gets_share_one_verified_fetch_and_retry_after_failure() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ShardCache::open(dir.path()).unwrap();
    let sha = hash(b"valid");
    assert!(
        cache
            .ensure_shard(&sha, || async { Ok(b"wrong".to_vec()) })
            .await
            .is_err()
    );
    assert!(!cache.has_verified(&sha).unwrap());

    let mut tasks = tokio::task::JoinSet::new();
    let fetches = Arc::new(AtomicUsize::new(0));
    for _ in 0..16 {
        let (cache, sha, fetches) = (cache.clone(), sha.clone(), fetches.clone());
        tasks.spawn(async move {
            cache
                .ensure_shard(&sha, || async {
                    fetches.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Ok(b"valid".to_vec())
                })
                .await
                .unwrap();
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap()
    }
    assert_eq!(fetches.load(Ordering::Relaxed), 1);
    assert_eq!(cache.verified_ingest_count(), 1);

    // No remote read when a new process verifies an existing cache entry.
    let reopened = ShardCache::open(dir.path()).unwrap();
    reopened
        .ensure_shard(&sha, || async { panic!("unnecessary download") })
        .await
        .unwrap();
    // Replacing a cache entry with different same-length bytes must not reuse a prior verification.
    let target = reopened.shard_path(&sha).unwrap();
    let replacement = dir.path().join("replacement");
    std::fs::write(&replacement, b"wrong").unwrap();
    std::fs::rename(&replacement, &target).unwrap();
    assert!(matches!(
        reopened.has_verified(&sha),
        Err(CompassError::IntegrityViolation(_))
    ));
    assert!(matches!(
        ShardCache::open(dir.path()).unwrap().has_verified(&sha),
        Err(CompassError::IntegrityViolation(_))
    ));
    assert!(
        reopened
            .ensure_shard(&sha, || async { Ok(b"dirty".to_vec()) })
            .await
            .is_err()
    );
    reopened
        .ensure_shard(&sha, || async { Ok(b"valid".to_vec()) })
        .await
        .unwrap();
    assert_eq!(std::fs::read(target).unwrap(), b"valid");
}

#[test]
fn a_valid_filename_or_an_external_file_is_not_proof_of_integrity() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ShardCache::open(dir.path()).unwrap();
    let digest = hash(b"good");
    let path = cache.ingest_bytes(&digest, b"good").unwrap();
    assert!(cache.has_verified(&digest).unwrap());
    std::fs::write(&path, b"evil").unwrap();
    assert!(matches!(
        cache.has_verified(&digest),
        Err(CompassError::IntegrityViolation(_))
    ));
    #[cfg(unix)]
    {
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"good").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(matches!(
            cache.has_verified(&digest),
            Err(CompassError::IntegrityViolation(_))
        ));
    }
}

#[tokio::test]
async fn cold_fill_limit_applies_across_concurrent_callers_and_cancellation_releases_lock() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ShardCache::open(dir.path()).unwrap();
    let sha = hash(b"abandoned");
    let (began, started) = tokio::sync::oneshot::channel();
    let task = {
        let (cache, sha) = (cache.clone(), sha.clone());
        tokio::spawn(async move {
            cache
                .ensure_shard(&sha, || async {
                    began.send(()).unwrap();
                    std::future::pending().await
                })
                .await
        })
    };
    started.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(
        Duration::from_secs(2),
        cache.ensure_shard(&sha, || async { Ok(b"abandoned".to_vec()) }),
    )
    .await
    .unwrap()
    .unwrap();

    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..16u8 {
        let (cache, active, peak) = (cache.clone(), active.clone(), peak.clone());
        tasks.spawn(async move {
            let body = vec![i];
            cache
                .ensure_shard(&hash(&body), || async {
                    let current = active.fetch_add(1, Ordering::Relaxed) + 1;
                    peak.fetch_max(current, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    active.fetch_sub(1, Ordering::Relaxed);
                    Ok(body)
                })
                .await
                .unwrap();
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap()
    }
    assert_eq!(peak.load(Ordering::Relaxed), 4);
}

fn summary() -> CatalogSummary {
    let tables: Vec<_> = ["samples", "controls"]
        .into_iter()
        .map(|name| SealedTableSnapshot {
            schema: TableSchema {
                name: name.into(),
                columns: vec![ColumnDescriptor {
                    name: "value".into(),
                    data_type: "INTEGER".into(),
                    nullable: false,
                }],
                primary_key: vec![],
                row_count: 1,
            },
            files: vec![SealedFilePair {
                data_sha256: hash(name.as_bytes()),
                data_size_bytes: name.len() as u64,
                delete_sha256: None,
                delete_size_bytes: None,
            }],
        })
        .collect();
    let id = CatalogId::from("study");
    let digest = SealedSnapshot::compute_dataset_digest(&id, 1, &tables);
    CatalogSummary {
        id: id.clone(),
        name: "study".into(),
        version: "1".into(),
        description: String::new(),
        organism: String::new(),
        row_count: 2,
        latest_snapshot_id: 1,
        dataset_digest: digest.clone(),
        tables: tables.iter().map(|t| t.schema.clone()).collect(),
        facets: Default::default(),
        snapshots: vec![],
        scope_relation: None,
        latest_sealed: SealedSnapshot {
            catalog_id: id,
            snapshot_id: 1,
            dataset_digest: digest,
            created_at: String::new(),
            commit_message: String::new(),
            tables,
            facets: Default::default(),
        },
    }
}

#[test]
fn restricted_reader_keeps_full_pin_and_refuses_omitted_table_even_after_it_is_cached() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ShardCache::open(dir.path()).unwrap();
    let registered = summary();
    let seal = &registered.latest_sealed;
    cache.ingest_bytes(&hash(b"samples"), b"samples").unwrap();
    let selected = DuckLakeCatalogEngine::from_cached_tables_at(
        dir.path().into(),
        registered.clone(),
        cache.clone(),
        &BTreeSet::from(["samples".into()]),
    )
    .unwrap();
    assert!(
        DuckLakeCatalogEngine::from_cached_summary_at(
            dir.path().into(),
            registered.clone(),
            selected.shard_cache().clone(),
        )
        .is_err()
    );
    selected.verify_integrity().unwrap();
    assert_eq!(selected.load_sealed_snapshot(None).unwrap(), *seal);
    assert!(selected.load_sealed_snapshot(Some(2)).is_err());
    assert!(
        DuckLakeCatalogEngine::from_cached_summary_at(
            dir.path().into(),
            registered.clone(),
            cache.clone()
        )
        .is_err()
    );
    cache.ingest_bytes(&hash(b"controls"), b"controls").unwrap();
    assert!(
        DuckLakeCatalogEngine::from_cached_summary_at(dir.path().into(), registered, cache).is_ok()
    );
    let control = &selected.summary().latest_sealed.tables[1];
    assert!(matches!(
        build_sealed_table_scan_sql(control, selected.shard_cache()),
        Err(CompassError::AccessDenied(_))
    ));
    assert!(matches!(
        selected.shard_cache().shard_path(&hash(b"controls")),
        Err(CompassError::AccessDenied(_))
    ));
}

#[test]
fn preflight_checks_columns_scope_and_pin_without_using_any_shards() {
    let s = summary().latest_sealed;
    let mut g = DatasetGrant::full_access(OrgId::from("org"), s.catalog_id.clone());
    let mut q = QueryAst {
        table: Some("samples".into()),
        snapshot_id: Some(1),
        columns: vec!["value".into()],
        ..Default::default()
    };
    validate_run_query(&s, &g, &q).unwrap();
    q.columns.push("absent".into());
    assert!(validate_run_query(&s, &g, &q).is_err());
    q.columns.pop();
    q.snapshot_id = Some(2);
    assert!(validate_run_query(&s, &g, &q).is_err());
    q.snapshot_id = Some(1);
    g.row_scope = Some(
        serde_json::from_value(serde_json::json!({"column":"missing","op":"in","values":["x"]}))
            .unwrap(),
    );
    assert!(validate_run_query(&s, &g, &q).is_err());
}

#[test]
fn selecting_metadata_uses_only_the_registered_version_and_checks_row_count_overflow() {
    let head = summary();
    let old = head.latest_sealed.clone();
    let pinned = head.with_pinned_seal(old.clone()).unwrap();
    assert_eq!(pinned.latest_sealed, old);
    assert_eq!(pinned.row_count, 2);
    let mut bad_column = old.tables[0].clone();
    bad_column.schema.columns[0].name = "with spaces".into();
    assert!(biocompass_data_engine::validate_table_scan_schema(&bad_column).is_err());
    let mut bad_empty_type = old.tables[0].clone();
    bad_empty_type.schema.columns[0].data_type = "?".into();
    assert!(biocompass_data_engine::validate_table_scan_schema(&bad_empty_type).is_ok());
    bad_empty_type.files.clear();
    assert!(biocompass_data_engine::validate_table_scan_schema(&bad_empty_type).is_err());
    let mut wrong = old.clone();
    wrong.dataset_digest = hash(b"other");
    assert!(head.with_pinned_seal(wrong).is_err());
    let mut wrong = old.clone();
    wrong.snapshot_id += 1;
    assert!(head.with_pinned_seal(wrong).is_err());
    let mut too_large = old;
    too_large.tables[0].schema.row_count = u64::MAX;
    assert!(head.with_pinned_seal(too_large).is_err());
}
