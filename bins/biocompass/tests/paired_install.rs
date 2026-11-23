//! Clean paired workspace selection through the native install HTTP route.
//! The deliberate descriptor/package mismatch stops before OCI download.
use axum::{
    Json, Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
    routing::get,
};
use biocompass::{
    server::build_router,
    state::{AppState, DeploymentMode},
    workspace::InstalledWorkspace,
};
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower::ServiceExt;

#[tokio::test]
async fn missing_local_module_resolves_authorized_publication_order_and_pins_digest() {
    let tmp = tempfile::tempdir().unwrap();
    let good = format!("sha256:{}", "a".repeat(64));
    let wrong = format!("sha256:{}", "b".repeat(64));
    let downloads = json!({"plugins":[
        {"plugin_id":"demo/arithmetic","version":"1","image_digest":good},
        {"plugin_id":"demo/arithmetic","version":"9","image_digest":wrong}
    ]});
    let manifest = json!({"id":"demo/arithmetic","version":"1","name":"Arithmetic",
        "image":format!("registry.test/demo/arithmetic@{wrong}"),"execution":"local"});
    let package = json!({"plugin_id":"demo/arithmetic","plugin_version":"1",
        "image_digest":wrong,"manifest":manifest,"skill":null,"ui_files":[],"ui_sha256":null});
    let market = Router::new()
        .route("/api/v1/plugins/downloads", get(move |h: HeaderMap| async move {
            if h.get("authorization").and_then(|s| s.to_str().ok())==Some("Bearer qa-native-install") {
                (StatusCode::OK,Json(downloads))
            } else {
                (StatusCode::UNAUTHORIZED,Json(json!({"error":"sign in"})))
            }
        }))
        .route("/api/v1/auth/whoami",get(||async{Json(json!({"org_id":"org-fixture","user_id":"user-fixture","auth_method":"session","actor":"scientist"}))}))
        .route("/api/v1/plugins/demo/arithmetic/versions/1/package",get(move ||async move{Json(package)}));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, market).await.unwrap() });
    let mut state = AppState::bootstrap(tmp.path(), DeploymentMode::Local, true)
        .await
        .unwrap();
    let owner_addr: std::net::SocketAddr = "127.0.0.1:18450".parse().unwrap();
    let owner_link = state.enable_local_owner(owner_addr).unwrap().unwrap();
    state.installed_workspace = Some(Arc::new(
        InstalledWorkspace::open(tmp.path(), &url).unwrap(),
    ));
    let app = build_router(state);
    let ticket = owner_link.trim().split_once("#local-owner=").unwrap().1;
    let redeem = app
        .clone()
        .oneshot(
            Request::post("/api/v1/local/owner-session")
                .header("host", "127.0.0.1:18450")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "ticket": ticket }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let owner_session = serde_json::from_slice::<serde_json::Value>(
        &axum::body::to_bytes(redeem.into_body(), 4096).await.unwrap(),
    )
    .unwrap()["owner_session"]
        .as_str()
        .unwrap()
        .to_owned();
    for (path, body, signed_in, status) in [
        ("demo/arithmetic", "{}", false, StatusCode::UNAUTHORIZED),
        ("demo/arithmetic", "{}", true, StatusCode::CONFLICT),
        (
            "demo/arithmetic",
            r#"{"plugin_version":"9"}"#,
            true,
            StatusCode::NOT_FOUND,
        ),
        ("demo/other", "{}", true, StatusCode::NOT_FOUND),
    ] {
        let mut request = Request::post(format!("/api/v1/plugins/{path}/install"))
            .header("host", "127.0.0.1:18450")
            .header("x-biocompass-local-owner", &owner_session)
            .header("content-type", "application/json");
        if signed_in {
            request = request.header("authorization", "Bearer qa-native-install");
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{path} {body}");
        if status == StatusCode::CONFLICT {
            let bytes = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap();
            assert!(
                std::str::from_utf8(&bytes)
                    .unwrap()
                    .contains("digest differs from marketplace discovery")
            );
        }
        assert!(!tmp.path().join("module-releases").exists());
    }
    server.abort();
}
