//! Gate 2 Live OCI Container Confinement & Exploit Verification Suite.
//!
//! Runs against a real Podman/Docker daemon and a live OCI Distribution v1.1 registry (`127.0.0.1:5500`),
//! verifying cryptographic manifest digests, `--network=none` egress blocking, `--read-only` + `/in:ro`
//! immutability, UID `65532:65532` + `CapEff=0` + `NoNewPrivs=1`, hard timeout kill without zombie leak,
//! and end-to-end execution of `alithea/hla-motif-enrichment@sha256:...` on a masked DuckDB Parquet slice.

use biocompass_core_traits::{CompassError, DatasetGrant, OciRegistryClient, OrgId, QueryAst, RunId};
use biocompass_data_engine::{DuckDbCatalogEngine, StagingMode, seed_demo_hla_catalog};
use biocompass_oci_runner::{
    ContainerRunSpec, OciContainerRunner, OciDistributionClient, prepare_stage_permissions,
};
use biocompass_plugin_spec::{LoadedPlugin, NetworkPolicy, PluginResources};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/hla-motif-enrichment")
        .canonicalize()
        .expect("canonicalize plugins/hla-motif-enrichment")
}

#[tokio::test]
async fn live_oci_registry_manifest_verification_and_confinement_exploit_suite() {
    let loaded =
        LoadedPlugin::load_from_dir(plugin_dir(), false).expect("load @sha256 pinned plugin.toml");
    let runner = OciContainerRunner::detect()
        .await
        .expect("detect active Docker/Podman engine");

    // 1. Cryptographic OCI Distribution Spec v1.1 manifest verification against 127.0.0.1:5500
    let oci_client = OciDistributionClient::new(true).expect("oci client");
    let verified_image = oci_client
        .verify_image(&loaded.manifest.image)
        .await
        .expect("verify real manifest @sha256 against 127.0.0.1:5500/v2/");
    assert!(verified_image.sha256_digest.is_some());

    // Tamper 1 hex nibble of digest -> must fail with NotFound or IntegrityViolation
    let mut bad_image = loaded.manifest.image.clone();
    let last_ch = bad_image.pop().unwrap();
    bad_image.push(if last_ch == '0' { '1' } else { '0' });
    assert!(
        oci_client.verify_image(&bad_image).await.is_err(),
        "forged @sha256 digest must be rejected by OCI v1.1 registry verification"
    );

    // 2. Exploit Probe A: Outbound network connection MUST fail under --network=none
    let probe_stage = tempfile::tempdir().expect("probe tempdir");
    let probe_in = probe_stage.path().join("in");
    let probe_out = probe_stage.path().join("out");
    prepare_stage_permissions(probe_stage.path(), &probe_in, &probe_out).expect("perms");
    std::fs::write(probe_in.join("params.json"), br#"{"probe":true}"#).expect("write probe params");

    let net_probe_script = r#"
import socket, sys
try:
    s = socket.create_connection(("1.1.1.1", 53), timeout=2.0)
    s.close()
    print("EGRESS_ALLOWED_VULNERABILITY")
    sys.exit(99)
except OSError as exc:
    print(f"EGRESS_BLOCKED:{exc.errno}")
    sys.exit(0)
"#;

    let net_report = runner
        .execute(
            &ContainerRunSpec {
                run_id: RunId::from("run-probe-net"),
                image: verified_image.clone(),
                in_dir: probe_in.clone(),
                out_dir: probe_out.clone(),
                resources: PluginResources::default(),
                network: NetworkPolicy::default(),
                command_override: Some((
                    "/opt/venv/bin/python".into(),
                    vec!["-c".into(), net_probe_script.into()],
                )),
            },
            None,
        )
        .await
        .expect("execute network confinement probe");
    assert_eq!(
        net_report.exit_code, 0,
        "outbound network connection succeeded when --network=none was required! stdout={:?} stderr={:?}",
        net_report.stdout_lines, net_report.stderr_lines
    );
    assert!(
        net_report
            .stdout_lines
            .iter()
            .any(|l| l.starts_with("EGRESS_BLOCKED:")),
        "expected EGRESS_BLOCKED confirmation, got {:?}",
        net_report.stdout_lines
    );

    // 3. Exploit Probe B: Read-only rootfs, read-only /in, UID 65532:65532, CapEff=0, NoNewPrivs=1
    let fs_priv_probe = r#"
import errno, json, os, pathlib, sys

checks = {}
checks["uid"] = os.getuid()
checks["gid"] = os.getgid()

for label, target in [("root_etc", "/etc/pwn"), ("app_code", "/app/pwn"), ("ro_input", "/in/params.json")]:
    try:
        with open(target, "w") as fh:
            fh.write("tamper")
        checks[label] = "WRITABLE_VULNERABILITY"
    except OSError as exc:
        checks[label] = "EROFS" if exc.errno == errno.EROFS else f"ERR_{exc.errno}"

# /tmp and /out MUST be writable
pathlib.Path("/tmp/scratch.ok").write_text("ok")
checks["tmp_writable"] = pathlib.Path("/tmp/scratch.ok").read_text() == "ok"

status_kv = {}
for line in pathlib.Path("/proc/self/status").read_text().splitlines():
    if ":" in line:
        k, v = line.split(":", 1)
        status_kv[k.strip()] = v.strip()
checks["no_new_privs"] = status_kv.get("NoNewPrivs")
checks["cap_eff"] = status_kv.get("CapEff")

pathlib.Path("/out/confinement_audit.json").write_text(json.dumps(checks))
print("AUDIT_OK")
"#;

    let priv_report = runner
        .execute(
            &ContainerRunSpec {
                run_id: RunId::from("run-probe-priv"),
                image: verified_image.clone(),
                in_dir: probe_in.clone(),
                out_dir: probe_out.clone(),
                resources: PluginResources::default(),
                network: NetworkPolicy::default(),
                command_override: Some((
                    "/opt/venv/bin/python".into(),
                    vec!["-c".into(), fs_priv_probe.into()],
                )),
            },
            None,
        )
        .await
        .expect("execute fs & privilege probe");
    assert_eq!(
        priv_report.exit_code, 0,
        "privilege probe failed: stderr={:?}",
        priv_report.stderr_lines
    );
    let audit_raw =
        std::fs::read_to_string(probe_out.join("confinement_audit.json")).expect("read audit json");
    let audit: Value = serde_json::from_str(&audit_raw).expect("parse audit json");
    assert_eq!(audit["uid"], json!(65532));
    assert_eq!(audit["gid"], json!(65532));
    assert_eq!(audit["root_etc"], json!("EROFS"));
    assert_eq!(audit["app_code"], json!("EROFS"));
    assert_eq!(audit["ro_input"], json!("EROFS"));
    assert_eq!(audit["tmp_writable"], json!(true));
    assert_eq!(audit["no_new_privs"], json!("1"));
    assert_eq!(audit["cap_eff"], json!("0000000000000000"));

    // 4. Exploit Probe C: Hard Timeout Termination & Zero Zombie Container Leak
    let timeout_res = runner
        .execute(
            &ContainerRunSpec {
                run_id: RunId::from("run-probe-timeout"),
                image: verified_image.clone(),
                in_dir: probe_in,
                out_dir: probe_out,
                resources: PluginResources {
                    timeout_sec: 2,
                    ..PluginResources::default()
                },
                network: NetworkPolicy::default(),
                command_override: Some((
                    "/opt/venv/bin/python".into(),
                    vec!["-c".into(), "import time; time.sleep(60)".into()],
                )),
            },
            None,
        )
        .await;
    assert!(
        matches!(timeout_res, Err(CompassError::ContainerEngine(_))),
        "runaway sleep(60) container must fail on 2s timeout"
    );

    let ps_out = Command::new(&runner.engine().binary_path)
        .args([
            "ps",
            "--filter",
            "name=biocompass-job-",
            "--format",
            "{{.Names}}",
        ])
        .output()
        .expect("docker/podman ps");
    let leaked = String::from_utf8_lossy(&ps_out.stdout).trim().to_owned();
    assert!(
        leaked.is_empty(),
        "LEAK DETECTED: runaway container still running after timeout kill: {leaked}"
    );

    // 5. Full Bio-Compute Execution of alithea/hla-motif-enrichment@sha256 on Scoped DuckLake Parquet
    let catalog_dir = tempfile::tempdir().expect("catalog dir");
    let catalog_manifest = seed_demo_hla_catalog(catalog_dir.path()).expect("seed catalog");
    let catalog_engine =
        DuckDbCatalogEngine::open_verified(catalog_dir.path()).expect("open catalog");
    let scoped_grant = DatasetGrant::scoped_rows_partner(
        OrgId::from("org-partner"),
        catalog_manifest.id.clone(),
    );

    let bio_stage = tempfile::tempdir().expect("bio stage");
    let bio_in = bio_stage.path().join("in");
    let bio_out = bio_stage.path().join("out");
    prepare_stage_permissions(bio_stage.path(), &bio_in, &bio_out).expect("bio perms");

    let slice = catalog_engine
        .materialize_parquet_for_run(
            &scoped_grant,
            &QueryAst::default(),
            StagingMode::CustomerScoped,
            &bio_in.join("data/cohort.parquet"),
        )
        .expect("materialize Lung-scoped cohort.parquet");
    assert!(slice.row_count > 0);

    let params = json!({
        "alleles": ["HLA-A*02:01", "HLA-B*07:02"],
        "min_length": 8,
        "max_length": 12,
        "strong_binder_cutoff_nm": 50.0
    });
    loaded
        .manifest
        .validate_inputs(&params)
        .expect("validate plugin inputs");
    std::fs::write(
        bio_in.join("params.json"),
        serde_json::to_vec_pretty(&params).unwrap(),
    )
    .expect("write /in/params.json");

    let bio_report = runner
        .execute(
            &ContainerRunSpec {
                run_id: RunId::from("run-hla-real-1"),
                image: verified_image,
                in_dir: bio_in,
                out_dir: bio_out.clone(),
                resources: loaded.manifest.resources.clone(),
                network: loaded.manifest.network.clone(),
                command_override: None,
            },
            None,
        )
        .await
        .expect("run real hla-motif-enrichment OCI container");

    assert_eq!(
        bio_report.exit_code, 0,
        "hla-motif-enrichment exited non-zero: stderr={:?}",
        bio_report.stderr_lines
    );

    let result_raw =
        std::fs::read_to_string(bio_out.join("result.json")).expect("read /out/result.json");
    let result_json: Value = serde_json::from_str(&result_raw).expect("parse /out/result.json");
    assert_eq!(result_json["sequence_policy"], json!("unmasked_research"));
    let candidates = result_json["top_candidates"]
        .as_array()
        .expect("top_candidates array");
    assert!(!candidates.is_empty());
    assert!(
        candidates.iter().all(|c| {
            c["sequence"]
                .as_str()
                .is_some_and(|s| s.len() >= 8 && s != "***")
        }),
        "every candidate emitted under 3-level rows grant must carry unmasked IUPAC sequence"
    );
    assert!(
        candidates.iter().all(|c| c["organ"] == json!("Lung")),
        "every candidate emitted by container must respect Lung row_scope"
    );
    assert!(
        bio_out.join("artifacts/allele_binding_summary.csv").is_file(),
        "expected supplementary CSV artifact in /out/artifacts/"
    );
}
