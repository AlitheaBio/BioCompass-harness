use axum::{
    Json, Router,
    routing::{get, post},
};
use biocompass::cloud_client::{CompassHubClient, CompassHubConfig};
use biocompass_core_traits::{CompassError, PrepareRunRequest, RunDispatcher};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::mpsc};

#[tokio::test]
async fn hosted_prepare_rejects_a_hub_that_ignores_the_request_selection() {
    let (sent, mut requests) = mpsc::channel(8);
    let api = Router::new()
        .route("/api/v1/health", get(|| async {
            Json(json!({"node_id":"node","provider_id":"provider","requester_id":"user","container_engine":"fixture"}))
        }))
        .route("/api/v1/runs/prepare", post(move |Json(body): Json<Value>| {
            let sent = sent.clone();
            async move {
                sent.send(body).await.unwrap();
                Json(json!({
                    "run_id":"fixture","plugin_id":"qa/echo","plugin_version":"2.0","offer_id":"standard",
                    "image_ref":"fixture","target_node_id":"node","estimated_cost_milli_eur":0,
                    "workload_hash":"hash","froglet_quote":{},"expires_at":"2030-01-01T00:00:00Z"
                }))
            }
        }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = CompassHubConfig {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        bundled_plugins_dir: Default::default(),
    };
    let server = tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });
    let (client, _, _) = CompassHubClient::connect(config, Default::default())
        .await
        .unwrap();

    for (id, version, offer, accepted) in [
        ("qa/echo", Some("1.0"), "standard", false),
        ("qa/other", Some("2.0"), "standard", false),
        ("qa/echo", Some("2.0"), "large", false),
        ("qa/echo", Some("2.0"), " standard ", true),
        ("qa/echo", None, "", true),
    ] {
        let mut body = json!({"org_id":"org","plugin_id":id,"offer_id":offer,"params":{}});
        if let Some(pin) = version {
            body["plugin_version"] = json!(pin);
        }
        let request: PrepareRunRequest = serde_json::from_value(body).unwrap();
        let returned = client.prepare_run(request).await;
        if accepted {
            assert_eq!(returned.unwrap().plugin_version, "2.0");
        } else {
            assert!(matches!(returned, Err(CompassError::IntegrityViolation(_))));
        }
        let wire = requests.recv().await.unwrap();
        assert_eq!(wire["plugin_id"], id);
        assert_eq!(wire["offer_id"], offer);
        assert_eq!(wire.get("plugin_version").and_then(Value::as_str), version);
        assert_eq!(wire.get("plugin_version").is_some(), version.is_some());
    }
    server.abort();
}
