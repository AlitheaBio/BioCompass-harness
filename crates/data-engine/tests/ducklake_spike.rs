//! Milestone 1 §5.1 DuckLake Spike & Archive-on-Seal Verification Suite.
//!
//! Empirically verifies against bundled DuckDB `v2.0.0` (`duckdb = 1.20000.2`):
//! 1. Offline signed extension load (`parquet` static + vendored `httpfs`, `postgres_scanner`, `ducklake`)
//!    with `allow_unsigned_extensions = false` and `autoinstall/autoload = false`.
//! 2. Local DuckLake catalog backed by a `.ducklake` DuckDB metadata file (`DATA_INLINING_ROW_LIMIT 0`).
//! 3. Cloud DuckLake catalog backed by isolated `biocompass_ducklake` Postgres DB (`ducklake_user` denied
//!    `CONNECT` to `biocompass_cloud`) and Garage S3 (`s3://biocompass-artifacts/...`) when Compose is running.
//! 4. Snapshot time travel (`AT (VERSION => sid)`) across `INSERT`, `UPDATE`, and `DELETE`.
//! 5. Positional delete schema `(file_path, pos)` & archive-on-seal content-addressed `sha256/<digest>.parquet`
//!    equivalence (`read_parquet(..., file_row_number = true)` anti-joined with `pos` == `AT (VERSION => sid)`),
//!    preserving sealed snapshot immutability even after DuckLake consolidates later delete files.
//! 6. Selective snapshot expiry (`ducklake_expire_snapshots`) & file cleanup while sealed/archived files remain intact.
//! 7. Read-only (`READ_ONLY`) local DuckLake attach.
//! 8. Postgres `pg_stat_activity` connection footprint (`+1` connection per attached cloud catalog, `+0` after `DETACH`).

use duckdb::{Config, Connection};
use sha2::{Digest, Sha256};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn vendor_ext_path(ext: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root");
    let platform = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "osx_arm64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux_arm64"
    } else {
        "linux_amd64"
    };
    root.join("vendor")
        .join("duckdb-extensions")
        .join("v2.0.0")
        .join(platform)
        .join(format!("{ext}.duckdb_extension"))
}

fn open_offline_duckdb() -> Connection {
    let cfg = Config::default()
        .with("autoinstall_known_extensions", "false")
        .expect("disable autoinstall")
        .with("autoload_known_extensions", "false")
        .expect("disable autoload");
    let conn = Connection::open_in_memory_with_flags(cfg).expect("open in-memory duckdb");
    let unsigned_flag: String = conn
        .query_row(
            "SELECT current_setting('allow_unsigned_extensions')::VARCHAR",
            [],
            |r| r.get(0),
        )
        .expect("check allow_unsigned_extensions");
    assert_eq!(
        unsigned_flag, "false",
        "DuckDB extension signature verification MUST remain enabled"
    );

    for ext in ["parquet", "httpfs", "postgres_scanner", "ducklake"] {
        let p = vendor_ext_path(ext);
        assert!(
            p.is_file(),
            "missing vendored signed DuckDB extension {}",
            p.display()
        );
        conn.execute_batch(&format!(
            "LOAD '{}';",
            p.to_string_lossy().replace('\'', "''")
        ))
        .unwrap_or_else(|e| panic!("offline signed LOAD failed for {}: {e}", p.display()));
    }
    conn
}

fn archive_by_sha256(src: &Path, archive_dir: &Path) -> (String, PathBuf) {
    let bytes = std::fs::read(src).expect("read shard for archive");
    let digest = hex::encode(Sha256::digest(&bytes));
    let dst = archive_dir.join(format!("{digest}.parquet"));
    if !dst.exists() {
        std::fs::write(&dst, &bytes).expect("write archived shard");
    }
    (digest, dst)
}

#[test]
fn ducklake_eight_point_spike_and_archive_on_seal() {
    // 1. Offline signed extension load
    let conn = open_offline_duckdb();

    // 2. Local DuckDB-file metadata DuckLake attach with DATA_INLINING_ROW_LIMIT 0
    let tmp = tempfile::tempdir().expect("tempdir");
    let meta_path = tmp.path().join("meta.ducklake");
    let data_dir = tmp.path().join("data");
    let seal_archive = tmp.path().join("sha256");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&seal_archive).unwrap();

    conn.execute_batch(&format!(
        "ATTACH 'ducklake:{}' AS dl (DATA_PATH '{}', DATA_INLINING_ROW_LIMIT 0);",
        meta_path.to_string_lossy(),
        data_dir.to_string_lossy()
    ))
    .expect("attach local ducklake");

    conn.execute_batch(
        "CREATE TABLE dl.peptides (peptide_id VARCHAR, sequence VARCHAR, affinity_nm DOUBLE);
         INSERT INTO dl.peptides VALUES
            ('PEP-1', 'KLFGTSGQKT', 18.5),
            ('PEP-2', 'YLLPWQGRV', 22.0),
            ('PEP-3', 'RPILTIITL', 31.0);",
    )
    .expect("seed snapshot 2");

    // Commit 3: UPDATE creates a new data file + positional delete file
    conn.execute_batch("UPDATE dl.peptides SET affinity_nm = 12.4 WHERE peptide_id = 'PEP-1';")
        .expect("update snapshot 3");

    // Archive Snapshot 3's active data + delete files into content-addressed cache BEFORE Commit 4
    let s3_pairs: Vec<(PathBuf, Option<PathBuf>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT data_file, delete_file
                 FROM ducklake_list_files('dl', 'peptides', snapshot_version => 3)
                 ORDER BY data_file",
            )
            .unwrap();
        stmt.query_map([], |r| {
            let df: String = r.get(0)?;
            let delf: Option<String> = r.get(1)?;
            Ok((
                archive_by_sha256(Path::new(&df), &seal_archive).1,
                delf.map(|d| archive_by_sha256(Path::new(&d), &seal_archive).1),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    assert_eq!(s3_pairs.len(), 2);

    // Commit 4: DELETE another row from the first data file (causes DuckLake to consolidate delete file)
    conn.execute_batch("DELETE FROM dl.peptides WHERE peptide_id = 'PEP-2';")
        .expect("delete snapshot 4");

    // Archive Snapshot 4 immediately on commit as well
    let s4_pairs: Vec<(PathBuf, Option<PathBuf>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT data_file, delete_file
                 FROM ducklake_list_files('dl', 'peptides', snapshot_version => 4)
                 ORDER BY data_file",
            )
            .unwrap();
        stmt.query_map([], |r| {
            let df: String = r.get(0)?;
            let delf: Option<String> = r.get(1)?;
            Ok((
                archive_by_sha256(Path::new(&df), &seal_archive).1,
                delf.map(|d| archive_by_sha256(Path::new(&d), &seal_archive).1),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };

    // 4 & 6. Compaction, cleanup_old_files, and selective snapshot expiry
    conn.execute_batch("INSERT INTO dl.peptides VALUES ('PEP-4', 'GLAGVGVKL', 9.1);")
        .unwrap();
    conn.execute_batch("CALL ducklake_cleanup_old_files('dl', cleanup_all => true);")
        .unwrap();
    conn.execute_batch("CALL ducklake_expire_snapshots('dl', versions => [4]);")
        .unwrap();
    conn.execute_batch("CALL ducklake_cleanup_old_files('dl', cleanup_all => true);")
        .unwrap();

    // Verify logical time travel on Snapshot 2 & 3 (and that expired Snapshot 4 is gone)
    let expected_s3: Vec<(String, String, f64)> = {
        let mut s = conn
            .prepare(
                "SELECT peptide_id, sequence, affinity_nm
                 FROM dl.peptides AT (VERSION => 3)
                 ORDER BY peptide_id",
            )
            .unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        expected_s3,
        vec![
            ("PEP-1".into(), "KLFGTSGQKT".into(), 12.4),
            ("PEP-2".into(), "YLLPWQGRV".into(), 22.0),
            ("PEP-3".into(), "RPILTIITL".into(), 31.0),
        ]
    );
    assert!(
        conn.query_row(
            "SELECT COUNT(*) FROM dl.peptides AT (VERSION => 4)",
            [],
            |r| r.get::<_, i64>(0)
        )
        .is_err(),
        "expired snapshot 4 must fail time-travel read"
    );

    // 5. Verify reading Snapshot 3 purely from content-addressed archived (data, delete) Parquet pairs
    // produces 100% identical rows WITHOUT touching DuckLake metadata!
    let conn_cache = open_offline_duckdb();
    let union_branches: Vec<String> = s3_pairs
        .iter()
        .map(|(data_p, del_p)| {
            let dp = data_p.to_string_lossy().replace('\'', "''");
            match del_p {
                Some(del) => {
                    let del_escaped = del.to_string_lossy().replace('\'', "''");
                    format!(
                        "SELECT peptide_id, sequence, affinity_nm \
                         FROM read_parquet('{dp}', file_row_number = true) \
                         WHERE file_row_number NOT IN (SELECT pos FROM read_parquet('{del_escaped}'))"
                    )
                }
                None => format!("SELECT peptide_id, sequence, affinity_nm FROM read_parquet('{dp}')"),
            }
        })
        .collect();
    let sealed_sql = format!(
        "SELECT peptide_id, sequence, affinity_nm FROM ({}) ORDER BY peptide_id",
        union_branches.join(" UNION ALL ")
    );
    let actual_sealed_s3: Vec<(String, String, f64)> = {
        let mut s = conn_cache.prepare(&sealed_sql).unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        actual_sealed_s3, expected_s3,
        "sealed content-addressed data+delete scan must match DuckLake AT (VERSION => 3) 100%"
    );

    // Even after Snapshot 4 was expired in DuckLake metadata, its archived content-addressed seal still reads cleanly!
    let s4_branches: Vec<String> = s4_pairs
        .iter()
        .map(|(data_p, del_p)| {
            let dp = data_p.to_string_lossy().replace('\'', "''");
            match del_p {
                Some(del) => {
                    let del_escaped = del.to_string_lossy().replace('\'', "''");
                    format!(
                        "SELECT peptide_id, sequence, affinity_nm \
                         FROM read_parquet('{dp}', file_row_number = true) \
                         WHERE file_row_number NOT IN (SELECT pos FROM read_parquet('{del_escaped}'))"
                    )
                }
                None => format!("SELECT peptide_id, sequence, affinity_nm FROM read_parquet('{dp}')"),
            }
        })
        .collect();
    let actual_sealed_s4: Vec<(String, String, f64)> = {
        let mut s = conn_cache
            .prepare(&format!(
                "SELECT peptide_id, sequence, affinity_nm FROM ({}) ORDER BY peptide_id",
                s4_branches.join(" UNION ALL ")
            ))
            .unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        actual_sealed_s4,
        vec![
            ("PEP-1".into(), "KLFGTSGQKT".into(), 12.4),
            ("PEP-3".into(), "RPILTIITL".into(), 31.0),
        ]
    );

    // 7. Hospital READ_ONLY attach rejects mutations
    conn.execute_batch("DETACH dl;").unwrap();
    let conn_ro = open_offline_duckdb();
    conn_ro
        .execute_batch(&format!(
            "ATTACH 'ducklake:{}' AS hosp (READ_ONLY);",
            meta_path.to_string_lossy()
        ))
        .expect("hospital READ_ONLY attach");
    let ro_rows: i64 = conn_ro
        .query_row("SELECT COUNT(*) FROM hosp.peptides", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ro_rows, 3);
    assert!(
        conn_ro
            .execute_batch("INSERT INTO hosp.peptides VALUES ('X', 'ACDEFGHIK', 1.0);")
            .is_err(),
        "READ_ONLY hospital DuckLake must reject INSERT"
    );

    // 3 & 8. Cloud Postgres (`biocompass_ducklake`) + Garage S3 + connection count check
    // Executed automatically whenever the local Compose dev stack (:15432 & :19000) is reachable.
    let pg_up = TcpStream::connect_timeout(
        &"127.0.0.1:15432".parse().unwrap(),
        Duration::from_millis(300),
    )
    .is_ok();
    let s3_up = TcpStream::connect_timeout(
        &"127.0.0.1:19000".parse().unwrap(),
        Duration::from_millis(300),
    )
    .is_ok();

    if pg_up && s3_up {
        let conn_cloud = open_offline_duckdb();
        // Verify ducklake_user is strictly blocked from control-plane DB `biocompass_cloud`
        let control_plane_attempt = conn_cloud.execute_batch(
            "ATTACH 'ducklake:postgres:dbname=biocompass_cloud host=127.0.0.1 port=15432 user=ducklake_user password=ducklake_dev_password' AS forbidden_dl (DATA_PATH 's3://biocompass-artifacts/forbidden/');",
        );
        assert!(
            control_plane_attempt.is_err(),
            "ducklake_user must have zero access to control-plane database biocompass_cloud"
        );

        let conn_probe = open_offline_duckdb();
        conn_probe
            .execute_batch(
                "ATTACH 'dbname=biocompass_ducklake host=127.0.0.1 port=15432 user=ducklake_user password=ducklake_dev_password' AS pg_probe (TYPE postgres, READ_ONLY);",
            )
            .unwrap();
        let count_conns = |c: &Connection| -> i64 {
            c.query_row(
                "SELECT COUNT(*) FROM postgres_query('pg_probe', 'SELECT pid FROM pg_stat_activity WHERE datname = ''biocompass_ducklake'' AND usename = ''ducklake_user''')",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };

        conn_cloud
            .execute_batch(
                "CREATE SECRET garage_s3 (
                    TYPE s3,
                    KEY_ID 'GK0123456789abcdef01234567',
                    SECRET '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef',
                    REGION 'eu-central-1',
                    ENDPOINT '127.0.0.1:19000',
                    URL_STYLE 'path',
                    USE_SSL false
                );",
            )
            .unwrap();

        let baseline = count_conns(&conn_probe);
        let schema = format!(
            "spike_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let s3_path = format!("s3://biocompass-artifacts/{schema}/");
        conn_cloud
            .execute_batch(&format!(
                "ATTACH 'ducklake:postgres:dbname=biocompass_ducklake host=127.0.0.1 port=15432 user=ducklake_user password=ducklake_dev_password' AS cloud_dl (METADATA_SCHEMA '{schema}', DATA_PATH '{s3_path}', DATA_INLINING_ROW_LIMIT 0);"
            ))
            .expect("attach cloud ducklake");
        let attached_delta = count_conns(&conn_probe) - baseline;
        assert_eq!(
            attached_delta, 1,
            "each attached cloud DuckLake catalog should hold 1 Postgres connection"
        );

        conn_cloud
            .execute_batch(
                "CREATE TABLE cloud_dl.peptides (peptide_id VARCHAR, sequence VARCHAR, affinity_nm DOUBLE);
                 INSERT INTO cloud_dl.peptides VALUES ('C-1', 'KLFGTSGQKT', 15.2), ('C-2', 'YLEPGPVTA', 12.8);
                 UPDATE cloud_dl.peptides SET affinity_nm = 10.5 WHERE peptide_id = 'C-1';",
            )
            .unwrap();

        let v2_aff: f64 = conn_cloud
            .query_row(
                "SELECT affinity_nm FROM cloud_dl.peptides AT (VERSION => 2) WHERE peptide_id = 'C-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let v3_aff: f64 = conn_cloud
            .query_row(
                "SELECT affinity_nm FROM cloud_dl.peptides AT (VERSION => 3) WHERE peptide_id = 'C-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!((v2_aff, v3_aff), (15.2, 10.5));

        conn_cloud.execute_batch("DETACH cloud_dl;").unwrap();
        let detached_delta = count_conns(&conn_probe) - baseline;
        assert_eq!(detached_delta, 0, "DETACH must release Postgres connection");
    } else {
        println!("Note: Compose Postgres/Garage not listening on :15432/:19000; skipped cloud subcheck.");
    }
}
