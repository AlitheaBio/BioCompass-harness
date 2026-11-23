//! Runs the actual native router against an isolated workspace and authenticated loopback hub.
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
    routing::get,
};
use biocompass::{
    server::build_router,
    state::{AppState, DeploymentMode},
    workspace::InstalledWorkspace,
};
use biocompass_core_traits::{RunId, RunStatus, SessionStore};
use serde_json::{Value, json};
use std::{fs, path::Path, sync::Arc};
use tower::ServiceExt;

const TEST_HOST: &str = "127.0.0.1:18451";

async fn state(root: &Path, url: &str) -> (AppState, String) {
    let mut state = AppState::bootstrap(root, DeploymentMode::Local, true)
        .await
        .unwrap();
    let link = state
        .enable_local_owner(TEST_HOST.parse().unwrap())
        .unwrap()
        .unwrap();
    state.installed_workspace = Some(Arc::new(InstalledWorkspace::open(root, url).unwrap()));
    (state, link)
}

async fn redeem_owner(app: &Router, link: &str) -> String {
    let ticket = link.trim().split_once("#local-owner=").unwrap().1;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/local/owner-session")
                .method("POST")
                .header("host", TEST_HOST)
                .header("content-type", "application/json")
                .body(Body::from(json!({ "ticket": ticket }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    serde_json::from_slice::<Value>(&bytes).unwrap()["owner_session"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn call(
    app: &Router,
    owner: &str,
    method: &str,
    path: &str,
    body: Value,
    user: &str,
) -> (u16, Value) {
    let request = Request::builder()
        .uri(path)
        .method(method)
        .header("host", TEST_HOST)
        .header("x-biocompass-local-owner", owner)
        .header("content-type", "application/json")
        .header("authorization", user)
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let r = app.clone().oneshot(request).await.unwrap();
    let s = r.status().as_u16();
    let bytes = to_bytes(r.into_body(), 4_000_000).await.unwrap();
    (s, serde_json::from_slice(&bytes).unwrap())
}
fn source(root: &Path, id: &str, v: &str, plane: &str) -> String {
    let dir = root.join("plugins").join(format!("{id}-{v}"));
    fs::create_dir_all(&dir).unwrap();
    let manifest = format!(
        "id='fixture/{id}'\nname='Example'\nversion='{v}'\nimage='local/fixture:dev'\nexecution='{plane}'\nprice_milli_act=0\n"
    );
    fs::write(dir.join("plugin.toml"), &manifest).unwrap();
    manifest
}
#[tokio::test]
async fn exact_source_detach_restore_and_quote_lifecycle_through_native_http() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let original = source(root, "source", "1", "cloud_only");
    source(root, "source", "2", "cloud_only");
    source(root, "quoted", "1", "local");
    let mock=Router::new().route("/api/v1/auth/whoami",get(|h:HeaderMap|async move{
        let org=match h.get("authorization").and_then(|v|v.to_str().ok()) {
            Some("Bearer org-a")=>"org-a",Some("Bearer org-b")=>"org-b",
            _=>return(StatusCode::UNAUTHORIZED,Json(json!({"error":"sign in"}))),
        }; (StatusCode::OK,Json(json!({"org_id":org,"user_id":"fixture","actor":"scientist","auth_method":"session"})))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let hub = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    // Install two valid managed eviction fixtures exactly as a prior paired release would persist
    // them. The browser acceptance suite also acquires real OCI images through the marketplace.
    use sha2::{Digest, Sha256};
    let mut managed_dirs = Vec::new();
    for version in ["1", "2"] {
        let id = "fixture/managed";
        let name = hex::encode(Sha256::digest(serde_json::to_vec(&(id, version)).unwrap()));
        let dir = root.join("module-releases").join(name);
        fs::create_dir_all(dir.join("content")).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let receipt = json!({"format":1,"origin":url,"acquired_by_org":"org-a","installed_at":chrono::Utc::now(),
            "manifest":{"id":id,"name":"Managed","version":version,"execution":"local","image":format!("oci.test/fixture/managed@{digest}")},
            "image_digest":digest,"ui_sha256":null,"skill":null,"files":{}});
        fs::write(
            dir.join("release.json"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        managed_dirs.push(dir);
    }
    let (initial, link1) = state(root, &url).await;
    let app = build_router(initial.clone());
    let owner1 = redeem_owner(&app, &link1).await;
    let managed = "/api/v1/plugins/fixture/managed/versions/1/install";
    assert_eq!(
        call(&app, &owner1, "DELETE", managed, json!({}), "Bearer org-b")
            .await
            .0,
        403
    );
    assert_eq!(
        call(&app, &owner1, "DELETE", managed, json!({}), "Bearer org-a")
            .await
            .1["removed"],
        true
    );
    assert_eq!(
        call(&app, &owner1, "DELETE", managed, json!({}), "Bearer org-a")
            .await
            .1["removed"],
        false
    );
    assert!(!managed_dirs[0].exists() && managed_dirs[1].join("release.json").is_file());
    let remove = "/api/v1/plugins/fixture/source/versions/1/install";
    assert_eq!(call(&app, &owner1, "DELETE", remove, json!({}), "").await.0, 401);
    assert_eq!(
        call(&app, &owner1, "DELETE", remove, json!({}), "Bearer org-a")
            .await
            .1["removed"],
        true
    );
    assert_eq!(
        call(&app, &owner1, "DELETE", remove, json!({}), "Bearer org-a")
            .await
            .1["removed"],
        false
    );
    assert_eq!(
        call(&app, &owner1, "DELETE", remove, json!({}), "Bearer org-b")
            .await
            .0,
        403
    );
    let source_path = root.join("plugins/source-1/plugin.toml");
    assert_eq!(fs::read_to_string(&source_path).unwrap(), original);
    let (st2, link2) = state(root, &url).await;
    let app = build_router(st2);
    let owner2 = redeem_owner(&app, &link2).await;
    let cards = call(&app, &owner2, "GET", "/api/v1/plugins", json!({}), "").await.1;
    let card = cards["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["manifest"]["id"] == "fixture/source")
        .unwrap();
    assert_eq!(card["manifest"]["version"], "2");
    assert_eq!(card["detached_versions"], json!(["1"]));
    let restore = "/api/v1/plugins/fixture/source/install";
    let req = json!({"source":"workspace","plugin_version":"1"});
    assert_eq!(
        call(&app, &owner2, "POST", restore, req.clone(), "Bearer org-b")
            .await
            .0,
        403
    );
    fs::write(
        &source_path,
        original.replace("price_milli_act=0", "price_milli_act=123"),
    )
    .unwrap();
    assert_eq!(
        call(&app, &owner2, "POST", restore, req.clone(), "Bearer org-a")
            .await
            .0,
        409
    );
    fs::write(&source_path, &original).unwrap();
    let (s, r) = call(&app, &owner2, "POST", restore, req, "Bearer org-a").await;
    assert_eq!((s, r["plugin_version"].as_str()), (200, Some("1")));
    let prep = json!({"plugin_id":"fixture/quoted","plugin_version":"1","params":{}});
    let (s, quote) = call(&app, &owner2, "POST", "/api/v1/runs/prepare", prep, "Bearer org-a").await;
    assert_eq!(s, 200, "{quote}");
    let quote = quote.get("quote").unwrap_or(&quote);
    let run_id = RunId::from(quote["run_id"].as_str().unwrap().to_string());
    let db = initial.local_sqlite.as_ref().unwrap();
    let delete = "/api/v1/plugins/fixture/quoted/versions/1/install";
    assert_eq!(
        call(&app, &owner2, "DELETE", delete, json!({}), "Bearer org-a")
            .await
            .0,
        409
    );
    let mut record = db.get_run(&run_id).await.unwrap();
    record.quote.expires_at = chrono::Utc::now() - chrono::Duration::minutes(2);
    record.status = RunStatus::Running;
    db.save_run(&record).await.unwrap();
    assert_eq!(
        call(&app, &owner2, "DELETE", delete, json!({}), "Bearer org-a")
            .await
            .0,
        409
    );
    record.status = RunStatus::Prepared;
    db.save_run(&record).await.unwrap();
    assert_eq!(
        call(&app, &owner2, "DELETE", delete, json!({}), "Bearer org-a")
            .await
            .0,
        200
    );
    assert_eq!(
        db.get_run(&run_id).await.unwrap().status,
        RunStatus::Prepared
    );
    hub.abort();
}
