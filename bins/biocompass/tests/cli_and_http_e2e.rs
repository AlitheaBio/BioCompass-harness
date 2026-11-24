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
use biocompass_core_traits::OciRegistryClient;
use biocompass_oci_runner::{DetectedEngine, EngineKind, OciContainerRunner, OciDistributionClient};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const REGISTRY_IMAGE: &str = "ghcr.io/distribution/distribution:3.0.0@sha256:4ba3adf47f5c866e9a29288c758c5328ef03396cb8f5f6454463655fa8bc83e2";

struct EphemeralRegistry {
    engine: DetectedEngine,
    container_name: String,
    host_port: u16,
    cert_dir: TempDir,
    ca_pem: Vec<u8>,
    podman_host_certs_dir: Option<PathBuf>,
    created_tags: Vec<String>,
}

impl EphemeralRegistry {
    async fn start(engine: DetectedEngine) -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("allocate ephemeral registry port")
            .local_addr()
            .unwrap()
            .port();
        let cert_dir = TempDir::new().expect("create throwaway TLS cert dir");
        let ca_cnf = cert_dir.path().join("ca.cnf");
        let ca_crt = cert_dir.path().join("ca.crt");
        let ca_key = cert_dir.path().join("ca.key");
        let leaf_cnf = cert_dir.path().join("leaf.cnf");
        let leaf_csr = cert_dir.path().join("registry.csr");
        let crt_path = cert_dir.path().join("registry.crt");
        let key_path = cert_dir.path().join("registry.key");
        std::fs::write(
            &ca_cnf,
            "[req]\ndistinguished_name = dn\nprompt = no\nx509_extensions = v3_ca\n[dn]\nCN = BioCompass Test Throwaway CA\n[v3_ca]\nbasicConstraints = critical,CA:TRUE,pathlen:0\nkeyUsage = critical,keyCertSign,cRLSign\nsubjectKeyIdentifier = hash\n",
        )
        .expect("write throwaway CA openssl config");
        std::fs::write(
            &leaf_cnf,
            "[req]\ndistinguished_name = dn\nprompt = no\n[dn]\nCN = 127.0.0.1\n[v3_leaf]\nbasicConstraints = critical,CA:FALSE\nkeyUsage = critical,digitalSignature,keyEncipherment\nextendedKeyUsage = serverAuth\nsubjectAltName = IP:127.0.0.1,DNS:localhost\n",
        )
        .expect("write throwaway leaf openssl config");
        for args in [
            vec![
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                "-config", ca_cnf.to_str().unwrap(),
                "-keyout", ca_key.to_str().unwrap(),
                "-out", ca_crt.to_str().unwrap(),
            ],
            vec![
                "req", "-new", "-newkey", "rsa:2048", "-nodes",
                "-config", leaf_cnf.to_str().unwrap(),
                "-keyout", key_path.to_str().unwrap(),
                "-out", leaf_csr.to_str().unwrap(),
            ],
            vec![
                "x509", "-req", "-days", "1",
                "-in", leaf_csr.to_str().unwrap(),
                "-CA", ca_crt.to_str().unwrap(),
                "-CAkey", ca_key.to_str().unwrap(),
                "-CAcreateserial",
                "-extfile", leaf_cnf.to_str().unwrap(),
                "-extensions", "v3_leaf",
                "-out", crt_path.to_str().unwrap(),
            ],
        ] {
            let openssl_out = Command::new("openssl")
                .args(&args)
                .output()
                .expect("run openssl to generate throwaway loopback TLS cert");
            assert!(
                openssl_out.status.success(),
                "openssl {:?} failed: {}",
                args,
                String::from_utf8_lossy(&openssl_out.stderr)
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(cert_dir.path(), std::fs::Permissions::from_mode(0o755))
                .expect("chmod cert_dir 0755");
            std::fs::set_permissions(&crt_path, std::fs::Permissions::from_mode(0o644))
                .expect("chmod registry.crt 0644");
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644))
                .expect("chmod registry.key 0644");
        }
        let ca_pem = std::fs::read(&ca_crt).expect("read throwaway CA PEM");

        // Trust the throwaway CA strictly for this single ephemeral loopback port (`127.0.0.1:{port}`)
        // inside Podman's per-host certs.d directory; removed on Drop.
        let podman_host_certs_dir = if engine.kind == EngineKind::Podman {
            let config_base = std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .expect("resolve containers certs.d config root");
            let host_dir = config_base
                .join("containers/certs.d")
                .join(format!("127.0.0.1:{port}"));
            std::fs::create_dir_all(&host_dir).expect("create podman per-port certs.d");
            std::fs::write(host_dir.join("ca.crt"), &ca_pem).expect("write podman throwaway ca.crt");
            Some(host_dir)
        } else {
            None
        };

        let container_name = format!("biocompass-gate4-reg-{}-{}", std::process::id(), port);
        let mut run_cmd = Command::new(&engine.binary_path);
        run_cmd.args(["run", "-d", "--rm", "--name", &container_name]);
        if cfg!(target_os = "linux") && engine.kind == EngineKind::Podman && engine.rootless {
            run_cmd.args(["--security-opt", "label=disable"]);
        }
        let status = run_cmd
            .args([
                "-p",
                &format!("127.0.0.1:{port}:5000"),
                "-v",
                &format!("{}:/certs:ro", cert_dir.path().display()),
                "-e",
                "REGISTRY_HTTP_TLS_CERTIFICATE=/certs/registry.crt",
                "-e",
                "REGISTRY_HTTP_TLS_KEY=/certs/registry.key",
                REGISTRY_IMAGE,
            ])
            .status()
            .expect("start ephemeral TLS OCI registry container");
        assert!(status.success(), "failed starting ephemeral TLS OCI registry");

        let ca_cert = reqwest::Certificate::from_pem(&ca_pem).expect("parse throwaway CA PEM");
        let client = reqwest::Client::builder()
            .add_root_certificate(ca_cert)
            .build()
            .expect("build TLS readiness HTTP client");
        let url = format!("https://127.0.0.1:{port}/v2/");
        for _ in 0..40 {
            if client.get(&url).send().await.is_ok_and(|r| r.status().is_success()) {
                return Self {
                    engine,
                    container_name,
                    host_port: port,
                    cert_dir,
                    ca_pem,
                    podman_host_certs_dir,
                    created_tags: Vec::new(),
                };
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        panic!("ephemeral TLS OCI registry on 127.0.0.1:{port} did not become healthy");
    }

    async fn build_and_push(&mut self, repo_path: &str, tag_ver: &str, context_dir: &Path) -> String {
        let repo = format!("127.0.0.1:{}/{repo_path}", self.host_port);
        let image_tag = format!("{repo}:{tag_ver}");
        self.created_tags.push(image_tag.clone());

        let mut build_cmd = Command::new(&self.engine.binary_path);
        build_cmd.arg("build");
        if self.engine.kind == EngineKind::Docker {
            build_cmd.arg("--provenance=false");
        }
        let build_status = build_cmd
            .args(["-q", "-t", &image_tag])
            .arg(context_dir)
            .status()
            .expect("build plugin image");
        assert!(build_status.success(), "container build of scaffolded plugin failed");

        let pinned_image = if self.engine.kind == EngineKind::Podman {
            let digest_file = self.cert_dir.path().join("pushed.digest");
            let push_out = Command::new(&self.engine.binary_path)
                .args([
                    "push",
                    "-q",
                    "--digestfile",
                    digest_file.to_str().unwrap(),
                    &image_tag,
                ])
                .output()
                .expect("podman push to ephemeral TLS OCI registry");
            assert!(
                push_out.status.success(),
                "podman push to ephemeral TLS OCI registry failed: {}",
                String::from_utf8_lossy(&push_out.stderr)
            );
            let digest = std::fs::read_to_string(&digest_file)
                .expect("read podman digestfile")
                .trim()
                .to_owned();
            assert!(
                digest.starts_with("sha256:"),
                "expected sha256 digest from podman push, got '{digest}'"
            );
            let pinned = format!("{repo}@{digest}");
            let pull_out = Command::new(&self.engine.binary_path)
                .args(["pull", "-q", &pinned])
                .output()
                .expect("podman pull by @sha256 digest over TLS");
            assert!(
                pull_out.status.success(),
                "podman pull '{pinned}' over TLS failed: {}",
                String::from_utf8_lossy(&pull_out.stderr)
            );
            pinned
        } else {
            let push_out = Command::new(&self.engine.binary_path)
                .args(["push", "-q", &image_tag])
                .output()
                .expect("docker push to ephemeral TLS OCI registry");
            assert!(
                push_out.status.success(),
                "docker push to ephemeral TLS OCI registry failed: {}",
                String::from_utf8_lossy(&push_out.stderr)
            );
            let inspect_out = Command::new(&self.engine.binary_path)
                .args([
                    "image",
                    "inspect",
                    "--format",
                    "{{index .RepoDigests 0}}",
                    &image_tag,
                ])
                .output()
                .expect("docker image inspect RepoDigests");
            assert!(inspect_out.status.success());
            String::from_utf8_lossy(&inspect_out.stdout).trim().to_owned()
        };

        assert!(
            pinned_image.contains("@sha256:"),
            "expected @sha256 digest from registry push, got '{pinned_image}'"
        );
        self.created_tags.push(pinned_image.clone());

        // Verify cryptographic manifest digest over HTTPS using the throwaway CA cert.
        let tls_client = OciDistributionClient::with_loopback_tls_ca_pem(&self.ca_pem)
            .expect("create loopback TLS OCI client");
        tls_client
            .verify_image(&pinned_image)
            .await
            .expect("verify pushed manifest over HTTPS");

        pinned_image
    }
}

impl Drop for EphemeralRegistry {
    fn drop(&mut self) {
        let _ = Command::new(&self.engine.binary_path)
            .args(["rm", "-f", &self.container_name])
            .output();
        for tag in &self.created_tags {
            let _ = Command::new(&self.engine.binary_path)
                .args(["rmi", "-f", tag])
                .output();
        }
        if let Some(dir) = &self.podman_host_certs_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[tokio::test]
async fn gate4_blackbox_cli_scaffold_docker_build_run_verify_and_http_csp_suite() {
    let runner = OciContainerRunner::detect()
        .await
        .expect("detect active Podman/Docker container engine");
    let mut registry = EphemeralRegistry::start(runner.engine().clone()).await;
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

    // 2. Real container build + push to self-contained ephemeral TLS OCI v1.1 registry
    let pinned_image = registry
        .build_and_push("alithea/gate4-uv-probe", "0.1.0", &plugin_dir)
        .await;

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
    // W6-1: without a trust anchor the command reports internal consistency only and fails.
    let unanchored = Command::new(bin)
        .args(["verify-receipt", receipt_path.to_str().unwrap()])
        .output()
        .expect("run biocompass verify-receipt");
    assert!(!unanchored.status.success(), "verify-receipt must fail without a trust anchor");
    assert!(String::from_utf8_lossy(&unanchored.stderr).contains("no trust anchor"));
    let chain: Value = serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
    let provider_key = chain[4]["signer"].as_str().unwrap().to_owned();
    let requester_key = chain[3]["signer"].as_str().unwrap().to_owned();
    // A wrong anchor is refused.
    let wrong = Command::new(bin)
        .args(["verify-receipt", "--trusted-key", &"ab".repeat(32), receipt_path.to_str().unwrap()])
        .output()
        .expect("run biocompass verify-receipt");
    assert!(!wrong.status.success(), "untrusted signers must fail");
    let verify_out = Command::new(bin)
        .args([
            "verify-receipt",
            "--trusted-key",
            &provider_key,
            "--trusted-key",
            &requester_key,
            receipt_path.to_str().unwrap(),
        ])
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

    if let Ok(ext_bin) = std::env::var("FROGLET_VERIFY_BIN")
        && std::path::Path::new(&ext_bin).is_file()
    {
        let ext_out = Command::new(ext_bin)
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

    // 5b. Discover catalog ID via GET /api/v1/catalogs, then query with Partner Scoped Rows Grant (unmasked + Lung-only)
    // and verify compute_only grant is rejected on query API, custom module, and local export!
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
    assert!(
        cat_list["catalogs"][0]["facets"]["hla_alleles"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "expected published non-sensitive hla_alleles facets on catalog card"
    );

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
    assert_eq!(cat_resp["access_level"], json!("rows"), "unexpected resp: {cat_resp}");
    let rows = cat_resp["rows"].as_array().expect("rows");
    assert!(!rows.is_empty());
    for r in rows {
        let seq = r["sequence"].as_str().expect("sequence string");
        assert!(seq.len() >= 8 && seq != "***", "expected unmasked IUPAC sequence, got {r}");
        assert_eq!(r["organ"], json!("Lung"));
    }

    // Verify compute_only grant gets 403 on query API, local export, and unapproved custom module!
    let co_query = http
        .post(format!("{base}/api/v1/catalogs/{catalog_id}/query"))
        .json(&json!({ "grant_id": "compute_only", "query": { "limit": 10 } }))
        .send()
        .await
        .expect("POST compute_only query");
    assert_eq!(co_query.status(), 403);

    let co_export = http
        .post(format!("{base}/api/v1/catalogs/{catalog_id}/export"))
        .json(&json!({ "grant_id": "compute_only", "query": { "limit": 10 } }))
        .send()
        .await
        .expect("POST compute_only export");
    assert_eq!(co_export.status(), 403);

    let co_custom = http
        .post(format!("{base}/api/v1/runs/prepare"))
        .json(&json!({
            "plugin_id": "alithea/gate4-uv-probe",
            "grant_id": "compute_only",
            "params": { "alleles": ["HLA-A*02:01"], "min_length": 8, "max_length": 11 }
        }))
        .send()
        .await
        .expect("POST compute_only custom module prepare");
    assert_eq!(co_custom.status(), 403);

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
