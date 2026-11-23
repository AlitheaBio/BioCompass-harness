use super::agent_budget::{BudgetedAgentPort, ExecutionBudget};
use super::*;
use axum::{
    Json, Router,
    routing::{get, post},
};
use biocompass_agent_core::tools::AdmissionStatus;
use std::sync::Arc;
use tokio::{net::TcpListener, sync::mpsc};
use uuid::Uuid;

#[tokio::test]
async fn agent_preparation_shares_gateway_limit_and_preserves_budget_and_selection() {
    let execution_id = Uuid::new_v4();
    let (sent, mut requests) = mpsc::unbounded_channel();
    let prepares = sent.clone();
    let app = Router::new()
        .route("/api/v1/health", get(|| async { Json(json!({
            "node_id": "test", "provider_id": "test", "requester_id": "test", "container_engine": "fixture"
        })) }))
        .route("/api/v1/agent-executions", post(move || async move {
            Json(json!({"execution_id": execution_id}))
        }))
        .route("/api/v1/runs/prepare", post(move |Json(body): Json<Value>| {
            let prepares = prepares.clone();
            async move {
                prepares.send(body).unwrap();
                Json(json!({
                    "run_id":"r1","plugin_id":"test/echo","plugin_version":"2","offer_id":"paid",
                    "image_ref":"fixture","target_node_id":"node","estimated_cost_act":2000,
                    "workload_hash":"hash","froglet_quote":{},"expires_at":"2030-01-01T00:00:00Z"
                }))
            }
        }))
        .route(&format!("/api/v1/agent-executions/{execution_id}/runs/r1/confirm"),
            post(move || { let sent = sent.clone(); async move {
                sent.send(json!("confirmed")).unwrap();
                Json(json!({"run_id": "r1"}))
            }}));
    let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", socket.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(socket, app).await.unwrap() });
    let (mut hub, _, _) = CompassHubClient::connect(
        CompassHubConfig {
            base_url,
            bundled_plugins_dir: Default::default(),
        },
        HashMap::new(),
    )
    .await
    .unwrap();
    hub.prepare_slots = Semaphore::new(1);
    let hub = Arc::new(hub);
    let request: PrepareRunRequest = serde_json::from_value(json!({
        "org_id":"org", "plugin_id":"test/echo", "plugin_version":"2", "offer_id":"paid", "params":{}
    })).unwrap();
    let start = |allowance| {
        let hub = hub.clone();
        async move {
            BudgetedAgentPort::start(
                hub,
                &SessionId::from("session"),
                &ExecutionBudget::Finite {
                    limit_milli_act: allowance,
                },
                None,
            )
            .await
            .unwrap()
        }
    };

    let manual = start(0).await;
    let held = hub.try_prepare_slot().unwrap();
    assert!(matches!(
        manual.prepare_plugin_quote(request.clone()).await,
        Err(CompassError::RateLimited(_))
    ));
    assert!(requests.try_recv().is_err());
    drop(held);

    let (_, decision) = manual.prepare_plugin_quote(request.clone()).await.unwrap();
    assert!(matches!(
        decision.unwrap().status,
        AdmissionStatus::ApprovalRequired
    ));
    let wire = requests.try_recv().unwrap();
    assert_eq!(wire["agent_execution_id"], json!(execution_id));
    assert_eq!(wire["plugin_version"], "2");
    assert_eq!(wire["offer_id"], "paid");
    assert!(requests.try_recv().is_err());

    let approved = start(2000).await;
    let (_, decision) = approved.prepare_plugin_quote(request).await.unwrap();
    assert!(matches!(
        decision.unwrap().status,
        AdmissionStatus::Submitted
    ));
    assert_eq!(requests.try_recv().unwrap(), wire);
    assert_eq!(requests.try_recv().unwrap(), json!("confirmed"));
    assert!(requests.try_recv().is_err());
    server.abort();
}
