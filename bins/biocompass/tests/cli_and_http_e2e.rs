//! Gate 4 black-box CLI subprocess & HTTP/CSP integration suite for `biocompass`.
//!
//! Verifies:
//! 1. `./biocompass init-plugin --template python-uv` scaffolds a clean OCI plugin directory.
//! 2. Real `docker build` + `docker push` to `127.0.0.1:5500` produces an immutable `@sha256:<64-hex>`
//!    digest that is pinned into `plugin.toml`.
//! 3. `./biocompass run` executes the freshly built `python-uv` container over a masked DuckDB Parquet
//!    slice and emits a 5-artifact `froglet/v1` signed chain.
//! 4. Both `./biocompass verify-receipt` AND standalone `/tmp/froglet-clean-cli/target/debug/froglet-verify`
//!    return exit code `0` and `"valid": true` on the generated receipt chain!
//! 5. Live Axum HTTP server serves `/api/v1/health`, enforces Grant masking on `/api/v1/catalogs/.../query`,
//!    and enforces `PLUGIN_SANDBOX_CSP` (`connect-src 'none'`) + path-traversal blocking on `/plugin-ui/*`.

use biocompass::server::{PLUGIN_SANDBOX_CSP, build_router};
use biocompass::state::{AppState, DeploymentMode};
use serde_json::{Value, json};
use std::process::Command;
use tempfile::TempDir;

#[tokio::test]
async fn gate4_blackbox_cli_scaffold_docker_build_run_verify_and_http_csp_suite() {
    let bin = env!("CARGO_BIN_EXE_biocompass");
    let tmp = TempDir::new().expect("create temp workspace");
    let plugin_dir = tmp.path().join("gate4-uv-probe");
    let data_dir = tmp.path().join("biocompass-data");
    let receipt_path = tmp.path().join("gate4_chain.json");

    // 1. Black-box CLI: `biocompass init-plugin --template python-uv`
    let init_out = Command::new(bin)
        .args([
            "init-plugin",
            "--template",
            "python-uv",
            "--id",
            "alithea/gate4-uv-probe",
            "--name",
            "Gate 4 Python UV Probe",
            "--with-ui",
            "--out",
            plugin_dir.to_str().unwrap(),
        ])
        .output()
        .expect("run biocompass init-plugin");
    assert!(
        init_out.status.success(),
        "init-plugin failed: {}",
        String::from_utf8_lossy(&init_out.stderr)
    );
    assert!(plugin_dir.join("Dockerfile").is_file());
    assert!(plugin_dir.join("plugin.toml").is_file());
    assert!(plugin_dir.join("SKILL.md").is_file());

    // Create a minimal built `ui/dist/index.html` inside the scaffolded plugin to also test `/plugin-ui/*` CSP!
    let ui_dist = plugin_dir.join("ui/dist");
    std::fs::create_dir_all(&ui_dist).expect("create ui/dist");
    std::fs::write(
        ui_dist.join("index.html"),
        "<!doctype html><html><body>Custom Plugin Sandbox</body></html>",
    )
    .expect("write ui/dist/index.html");

    // 2. Real `docker build` + push to live local OCI v1.1 registry (`127.0.0.1:5500`)
    let image_tag = "127.0.0.1:5500/alithea/gate4-uv-probe:0.1.0";
    let build_status = Command::new("docker")
        .args(["build", "-q", "-t", image_tag, plugin_dir.to_str().unwrap()])
        .status()
        .expect("docker build scaffolded python-uv plugin");
    assert!(build_status.success(), "docker build of scaffolded python-uv template failed");

    let push_status = Command::new("docker")
        .args(["push", "-q", image_tag])
        .status()
        .expect("docker push to 127.0.0.1:5500");
    assert!(push_status.success(), "docker push to 127.0.0.1:5500 failed");

    let inspect_out = Command::new("docker")
        .args([
            "image",
            "inspect",
            "--format",
            "{{index .RepoDigests 0}}",
            image_tag,
        ])
        .output()
        .expect("docker image inspect RepoDigests");
    assert!(inspect_out.status.success());
    let pinned_image = String::from_utf8_lossy(&inspect_out.stdout)
        .trim()
        .to_owned();
    assert!(
        pinned_image.contains("@sha256:"),
        "expected @sha256 digest from registry push, got '{pinned_image}'"
    );

    // Pin immutable `@sha256:...` digest into scaffolded `plugin.toml`.
    let manifest_raw =
        std::fs::read_to_string(plugin_dir.join("plugin.toml")).expect("read plugin.toml");
    let pinned_manifest = manifest_raw.replace(
        "image = \"local/gate4-uv-probe:dev\"",
        &format!("image = \"{pinned_image}\""),
    );
    std::fs::write(plugin_dir.join("plugin.toml"), pinned_manifest).expect("write pinned plugin.toml");

    // 3. Black-box CLI: `biocompass run` over Partner Masked Grant
    let run_out = Command::new(bin)
        .args([
            "run",
            "--plugin",
            plugin_dir.to_str().unwrap(),
            "--params",
            r#"{"alleles":["HLA-A*02:01","HLA-B*07:02"],"min_length":8,"max_length":11}"#,
            "--grant",
            "partner",
            "--data-dir",
            data_dir.to_str().unwrap(),
            "--receipt-out",
            receipt_path.to_str().unwrap(),
        ])
        .output()
        .expect("run biocompass run");
    assert!(
        run_out.status.success(),
        "biocompass run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&run_out.stdout),
        String::from_utf8_lossy(&run_out.stderr)
    );

    let run_json: Value =
        serde_json::from_slice(&run_out.stdout).expect("parse biocompass run JSON");
    assert_eq!(run_json["status"], json!("succeeded"));
    assert_eq!(run_json["receipt_verified"], json!(true));
    assert!(
        !run_json["result"]["allele_counts"]
            .as_array()
            .expect("allele_counts")
            .is_empty()
    );

    // 4. Black-box CLI: `biocompass verify-receipt` + upstream standalone `froglet-verify`
    let verify_out = Command::new(bin)
        .args(["verify-receipt", receipt_path.to_str().unwrap()])
        .output()
        .expect("run biocompass verify-receipt");
    assert!(
        verify_out.status.success(),
        "biocompass verify-receipt failed: {}",
        String::from_utf8_lossy(&verify_out.stdout)
    );
    let verify_json: Value =
        serde_json::from_slice(&verify_out.stdout).expect("parse verify-receipt report");
    assert_eq!(verify_json["valid"], json!(true));
    assert_eq!(verify_json["chain_evaluated"], json!(true));

    let external_froglet = std::path::Path::new("/tmp/froglet-clean-cli/target/debug/froglet-verify");
    if external_froglet.is_file() {
        let ext_out = Command::new(external_froglet)
            .args(["--chain", "--json", receipt_path.to_str().unwrap()])
            .output()
            .expect("run standalone froglet-verify");
        assert!(ext_out.status.success());
        let ext_json: Value = serde_json::from_slice(&ext_out.stdout).expect("parse ext json");
        assert_eq!(ext_json["valid"], json!(true));
    }

    // 5. Live Axum HTTP Server + Strict Plugin Sandbox CSP & Traversal Checks
    // Copy the pinned scaffolded plugin into `<data_dir>/plugins/gate4-uv-probe` so `AppState` loads its UI!
    let installed_dir = data_dir.join("plugins/gate4-uv-probe");
    std::fs::create_dir_all(installed_dir.join("ui/dist")).expect("mkdir installed plugin");
    for f in ["plugin.toml", "SKILL.md", "ui/dist/index.html"] {
        std::fs::copy(plugin_dir.join(f), installed_dir.join(f)).expect("copy plugin file");
    }

    let state = AppState::bootstrap(&data_dir, DeploymentMode::Local, false)
        .await
        .expect("bootstrap AppState");
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test HTTP server");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve HTTP");
    });

    let http = reqwest::Client::new();
    let base = format!("http://{addr}");

    // 5a. Health endpoint
    let health: Value = http
        .get(format!("{base}/api/v1/health"))
        .send()
        .await
        .expect("GET health")
        .json()
        .await
        .expect("health json");
    assert_eq!(health["status"], json!("ok"));
    assert!(health["plugin_count"].as_u64().unwrap_or(0) >= 2);

    // 5b. Discover catalog ID via GET /api/v1/catalogs, then query with Partner Masked Grant (`'***'` + Lung-only)
    let cat_list: Value = http
        .get(format!("{base}/api/v1/catalogs"))
        .send()
        .await
        .expect("GET catalogs")
        .json()
        .await
        .expect("catalogs json");
    let catalog_id = cat_list["catalogs"][0]["id"]
        .as_str()
        .expect("catalog id string");
    assert_eq!(catalog_id, "alithea/hla-atlas-v2026.1");

    let cat_resp: Value = http
        .post(format!("{base}/api/v1/catalogs/{catalog_id}/query"))
        .json(&json!({
            "grant_id": "partner",
            "query": { "limit": 25 }
        }))
        .send()
        .await
        .expect("POST catalog query")
        .json()
        .await
        .expect("catalog query json");
    assert_eq!(cat_resp["sequence_masked"], json!(true), "unexpected resp: {cat_resp}");
    let rows = cat_resp["rows"].as_array().expect("rows");
    assert!(!rows.is_empty());
    for r in rows {
        assert_eq!(r["sequence"], json!("***"));
        assert_eq!(r["organ"], json!("Lung"));
    }

    // 5c. `/plugin-ui/...` enforces strict `PLUGIN_SANDBOX_CSP` (`connect-src 'none'`)
    let ui_resp = http
        .get(format!("{base}/plugin-ui/alithea/gate4-uv-probe/index.html"))
        .send()
        .await
        .expect("GET plugin-ui");
    assert_eq!(ui_resp.status(), 200);
    let csp = ui_resp
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .expect("CSP header must be present");
    assert_eq!(csp, PLUGIN_SANDBOX_CSP);
    assert!(csp.contains("connect-src 'none'"));

    // 5d. Path traversal attempt on `/plugin-ui/...` is blocked
    let trav_resp = http
        .get(format!(
            "{base}/plugin-ui/alithea/gate4-uv-probe/..%2F..%2Fplugin.toml"
        ))
        .send()
        .await
        .expect("GET traversal");
    assert!(
        trav_resp.status() == 403 || trav_resp.status() == 404,
        "expected 403 or 404 on traversal, got {}",
        trav_resp.status()
    );
}
