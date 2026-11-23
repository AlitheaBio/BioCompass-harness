use axum::{
    Json, Router,
    routing::{get, post, put},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{net::TcpListener, process::Command};

#[tokio::test]
async fn publisher_prints_the_hubs_versioned_custom_entry() {
    const MANIFEST: &str = r#"{"schemaVersion":2}"#;
    const ENTRY: &str = "/plugin-ui-version/fixture/numeric/v1.0.0+build.2/panel.html";
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let registrations = Arc::new(AtomicUsize::new(0));
    let register_count = registrations.clone();
    let router = Router::new()
        .route(
            "/v2/fixture/numeric/manifests/:digest",
            get(|| async { MANIFEST }),
        )
        .route(
            "/api/v1/plugins",
            post(move || {
                register_count.fetch_add(1, Ordering::SeqCst);
                async { Json(json!({"visibility":"org_private"})) }
            }),
        )
        .route(
            "/api/v1/plugins/fixture/numeric/versions/:version/ui-bundle",
            put(|Json(upload): Json<Value>| async move {
                assert!(
                    upload["files"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|f| f["path"] == "panel.html")
                );
                Json(json!({"uploaded":true,"ui_entry_url":ENTRY}))
            }),
        );
    let service = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let package = tempfile::tempdir().unwrap();
    let digest = hex::encode(Sha256::digest(MANIFEST.as_bytes()));
    fs::write(package.path().join("plugin.toml"), format!(
        "id = \"fixture/numeric\"\nname = \"Numeric\"\nversion = \"1.0.0+build.2\"\nimage = \"{address}/fixture/numeric@sha256:{digest}\"\n[ui]\nentry = \"ui/panel.html\"\n"
    )).unwrap();
    let assets = package.path().join("ui/assets");
    fs::create_dir_all(&assets).unwrap();
    fs::write(
        package.path().join("ui/panel.html"),
        r#"
        <base href="./assets/">
        <!-- <script src="unused.js"></script> -->
        <script>const example = '<link rel="stylesheet" href="unused.css">';</script>
        <script type="application/json" src="inert.json"></script>
        <template><link rel="stylesheet" href="inert.css"></template>
        <SCRIPT type=module src="./entry.js?cache=1#start"></SCRIPT>
        <link HREF='theme%20&amp;%20fonts.css' REL=stylesheet>
        <link rel=modulepreload href="./chunk.js">
    "#,
    )
    .unwrap();
    let names = ["entry.js", "theme & fonts.css", "chunk.js"];
    for name in names {
        fs::write(assets.join(name), "/* fixture */").unwrap();
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_biocompass"));
    command
        .args([
            "publish-plugin",
            "--hub-url",
            &format!("http://{address}"),
            "--api-key",
            "loopback-test-key",
            "--dir",
        ])
        .arg(package.path());
    let output = command.output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ui_files_uploaded"], 4);
    assert_eq!(result["ui_entry_url"], ENTRY);
    for name in names {
        fs::remove_file(assets.join(name)).unwrap();
        let rejected = command.output().await.unwrap();
        assert!(!rejected.status.success(), "published with missing {name}");
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains(name),
            "{:?}",
            rejected.stderr
        );
        assert_eq!(registrations.load(Ordering::SeqCst), 1);
        fs::write(assets.join(name), "/* restored */").unwrap();
    }
    service.abort();
}
