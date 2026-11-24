use super::*;
use biocompass_core_traits::RunStatus;
use serde_json::{Value, json};
use std::sync::atomic::AtomicUsize;
use tokio::sync::Notify;

type SyntheticBodyFn = Arc<
    dyn Fn(PathBuf, PathBuf, Arc<Notify>) -> Pin<Box<dyn Future<Output = i32> + Send>>
        + Send
        + Sync,
>;

/// Synthetic in-process `ContainerRunner` proving `supervise()` works identically across
/// non-Docker/Podman backends (such as Kubernetes Run Pods) with combined stdout and no stderr.
struct SyntheticRunner {
    kind: EngineKind,
    version: String,
    killed: Arc<AtomicBool>,
    cancelled_runs: Arc<std::sync::Mutex<Vec<RunId>>>,
    out_stats_override: Option<OutDirStats>,
    /// Hook executed inside a background task simulating the container body; returns exit code.
    body: SyntheticBodyFn,
}

struct SyntheticHandle {
    name: String,
    stdout: Option<Box<dyn AsyncRead + Send + Unpin>>,
    exit_rx: tokio::sync::oneshot::Receiver<i32>,
    kill_notify: Arc<Notify>,
    killed: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl ContainerHandle for SyntheticHandle {
    fn container_name(&self) -> &str {
        &self.name
    }

    fn take_stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        self.stdout.take()
    }

    fn take_stderr(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        None
    }

    async fn wait(&mut self) -> CompassResult<i32> {
        let rx = &mut self.exit_rx;
        rx.await
            .map_err(|_| CompassError::ContainerEngine("synthetic container dropped".into()))
    }

    async fn kill(&mut self) -> CompassResult<()> {
        self.killed.store(true, Ordering::Release);
        self.kill_notify.notify_waiters();
        Ok(())
    }
}

#[async_trait::async_trait]
impl ContainerRunner for SyntheticRunner {
    fn engine_kind(&self) -> EngineKind {
        self.kind
    }

    fn engine_version(&self) -> String {
        self.version.clone()
    }

    async fn spawn(&self, spec: &ContainerRunSpec) -> CompassResult<Box<dyn ContainerHandle>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let kill_notify = Arc::new(Notify::new());
        let (reader, mut writer) = tokio::io::duplex(4096);
        let body = Arc::clone(&self.body);
        let in_dir = spec.in_dir.clone();
        let out_dir = spec.out_dir.clone();
        let kill_clone = Arc::clone(&kill_notify);
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = writer.write_all(b"synthetic pod started\n").await;
            let code = tokio::select! {
                c = body(in_dir, out_dir, Arc::clone(&kill_clone)) => c,
                () = kill_clone.notified() => 137,
            };
            let _ = writer.shutdown().await;
            let _ = tx.send(code);
        });
        Ok(Box::new(SyntheticHandle {
            name: format!("bc-pod-{}", spec.run_id.as_str()),
            stdout: Some(Box::new(reader)),
            exit_rx: rx,
            kill_notify,
            killed: Arc::clone(&self.killed),
        }))
    }

    async fn inspect_out_dir(&self, out_dir: &Path) -> CompassResult<OutDirStats> {
        if let Some(stats) = self.out_stats_override {
            return Ok(stats);
        }
        Ok(measure_out_dir_stats(out_dir))
    }

    async fn cancel(&self, run_id: &RunId) -> CompassResult<()> {
        self.cancelled_runs.lock().unwrap().push(run_id.clone());
        Ok(())
    }
}

fn make_spec(tmp: &Path, run_id: &str, timeout_sec: u64) -> ContainerRunSpec {
    let in_dir = tmp.join("in");
    let out_dir = tmp.join("out");
    prepare_stage_permissions(tmp, &in_dir, &out_dir).expect("prepare stage");
    ContainerRunSpec {
        run_id: RunId::from(run_id),
        image: OciImageRef::parse("local/test:dev", true).expect("image"),
        in_dir,
        out_dir,
        resources: PluginResources {
            cpus: 1,
            memory_mb: 256,
            gpu: 0,
            timeout_sec,
            pids_limit: 64,
        },
        network: NetworkPolicy::default(),
        command_override: None,
    }
}

fn atomic_write_request(out_dir: &Path, req_id: &str, body: &[u8]) {
    let req_dir = out_dir.join(MAILBOX_REQUESTS_REL);
    let tmp = req_dir.join(format!(".tmp-{req_id}.tmp"));
    let final_path = req_dir.join(format!("{req_id}.json"));
    std::fs::write(&tmp, body).expect("write temp req");
    std::fs::rename(&tmp, &final_path).expect("atomic rename req");
}

async fn wait_for_reply(in_dir: &Path, req_id: &str) -> Value {
    let path = in_dir.join(MAILBOX_REPLIES_REL).join(format!("{req_id}.json"));
    for _ in 0..200 {
        if path.is_file() {
            let bytes = std::fs::read(&path).expect("read reply");
            return serde_json::from_slice(&bytes).expect("parse reply json");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for mailbox reply at {}", path.display());
}

struct StrictLiveQueryEcho {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LiveQueryResponder for StrictLiveQueryEcho {
    async fn call(&self, frame: &[u8]) -> CompassResult<Vec<u8>> {
        // Verify exact 3-field LiveQuerySession schema (`deny_unknown_fields`, no `"kind"`).
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct StrictReq {
            request_id: String,
            query: String,
            params: Value,
        }
        let parsed: StrictReq = serde_json::from_slice(frame)
            .map_err(|e| CompassError::Validation(format!("strict live query frame failed: {e}")))?;
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(serde_json::to_vec(&json!({
            "request_id": parsed.request_id,
            "query": parsed.query,
            "echo_params": parsed.params,
            "seq": n
        }))
        .unwrap())
    }
}

#[tokio::test]
async fn w2_live_query_atomic_mailbox_roundtrip_ignores_inflight_temp_files() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = make_spec(tmp.path(), "run-w2-live", 10);
    let responder = Arc::new(StrictLiveQueryEcho {
        calls: AtomicUsize::new(0),
    });
    let killed = Arc::new(AtomicBool::new(false));
    let runner = SyntheticRunner {
        kind: EngineKind::Kubernetes,
        version: "1.31.2".into(),
        killed: Arc::clone(&killed),
        cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
        out_stats_override: None,
        body: Arc::new(|in_dir, out_dir, _kill| {
            Box::pin(async move {
                // Verify mailbox config.json is readable by the container.
                let cfg: Value = serde_json::from_slice(
                    &std::fs::read(in_dir.join(MAILBOX_CONFIG_REL)).expect("config.json"),
                )
                .unwrap();
                assert_eq!(cfg["requests_dir"], CONTAINER_MAILBOX_REQUESTS_DIR);
                assert_eq!(cfg["replies_dir"], CONTAINER_MAILBOX_REPLIES_DIR);

                // Leave an in-flight dot-prefixed temp file AND a `.tmp` file to prove scanner ignores both.
                let req_dir = out_dir.join(MAILBOX_REQUESTS_REL);
                std::fs::write(req_dir.join(".tmp-inflight.tmp"), b"{partial").unwrap();
                std::fs::write(req_dir.join("staging.tmp"), b"{partial2").unwrap();

                // 1. Request with explicit `"kind": "query"`.
                let req1 = serde_json::to_vec(&json!({
                    "kind": "query",
                    "request_id": "q-1",
                    "query": "sample_roster.v2",
                    "params": { "page_size": 5 }
                }))
                .unwrap();
                atomic_write_request(&out_dir, "q-1", &req1);
                let reply1 = wait_for_reply(&in_dir, "q-1").await;
                assert_eq!(reply1["request_id"], "q-1");
                assert_eq!(reply1["seq"], 1);
                assert_eq!(reply1["echo_params"]["page_size"], 5);

                // 2. Shorthand request without `"kind"`.
                let req2 = serde_json::to_vec(&json!({
                    "request_id": "q_2.next",
                    "query": "sample_counts.v2",
                    "params": {}
                }))
                .unwrap();
                atomic_write_request(&out_dir, "q_2.next", &req2);
                let reply2 = wait_for_reply(&in_dir, "q_2.next").await;
                assert_eq!(reply2["request_id"], "q_2.next");
                assert_eq!(reply2["seq"], 2);
                0
            })
        }),
    };

    let report = supervise(
        &runner,
        &spec,
        SuperviseOptions {
            live_query: Some(responder.clone()),
            poll_interval: Duration::from_millis(20),
            ..SuperviseOptions::default()
        },
    )
    .await
    .expect("live query mailbox run succeeds");

    assert_eq!(report.exit_code, 0);
    assert_eq!(report.engine, EngineKind::Kubernetes);
    assert_eq!(report.engine_version, "1.31.2");
    assert!(!killed.load(Ordering::Acquire));
    assert_eq!(responder.calls.load(Ordering::SeqCst), 2);
}

#[cfg(unix)]
#[tokio::test]
async fn w2_abuse_symlink_kills_run_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = make_spec(tmp.path(), "run-abuse-symlink", 10);
    let secret = tmp.path().join("host-secret.json");
    std::fs::write(
        &secret,
        br#"{"request_id":"sym1","query":"demo","params":{}}"#,
    )
    .unwrap();
    let killed = Arc::new(AtomicBool::new(false));
    let secret_clone = secret.clone();
    let runner = SyntheticRunner {
        kind: EngineKind::Kubernetes,
        version: "1.31".into(),
        killed: Arc::clone(&killed),
        cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
        out_stats_override: None,
        body: Arc::new(move |_in_dir, out_dir, kill| {
            let target = secret_clone.clone();
            Box::pin(async move {
                let link = out_dir.join(MAILBOX_REQUESTS_REL).join("sym1.json");
                std::os::unix::fs::symlink(&target, &link).expect("create symlink");
                kill.notified().await;
                0
            })
        }),
    };

    let err = supervise(
        &runner,
        &spec,
        SuperviseOptions {
            live_query: Some(Arc::new(StrictLiveQueryEcho {
                calls: AtomicUsize::new(0),
            })),
            poll_interval: Duration::from_millis(20),
            ..SuperviseOptions::default()
        },
    )
    .await
    .expect_err("symlink in mailbox must kill run");
    assert!(err.to_string().contains("MAILBOX_VIOLATION"), "{err}");
    assert!(err.to_string().contains("symlink"), "{err}");
    assert!(killed.load(Ordering::Acquire), "container must be killed");
}

#[cfg(unix)]
#[tokio::test]
async fn w2_abuse_directory_symlink_swap_kills_run_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = make_spec(tmp.path(), "run-abuse-dirswap", 10);
    let decoy = tmp.path().join("decoy-dir");
    std::fs::create_dir_all(&decoy).unwrap();
    let killed = Arc::new(AtomicBool::new(false));
    let decoy_clone = decoy.clone();
    let runner = SyntheticRunner {
        kind: EngineKind::Kubernetes,
        version: "1.31".into(),
        killed: Arc::clone(&killed),
        cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
        out_stats_override: None,
        body: Arc::new(move |_in_dir, out_dir, kill| {
            let target = decoy_clone.clone();
            Box::pin(async move {
                let req_dir = out_dir.join(MAILBOX_REQUESTS_REL);
                std::fs::remove_dir(&req_dir).unwrap();
                std::os::unix::fs::symlink(&target, &req_dir).unwrap();
                kill.notified().await;
                0
            })
        }),
    };

    let err = supervise(
        &runner,
        &spec,
        SuperviseOptions {
            live_query: Some(Arc::new(StrictLiveQueryEcho {
                calls: AtomicUsize::new(0),
            })),
            poll_interval: Duration::from_millis(20),
            ..SuperviseOptions::default()
        },
    )
    .await
    .expect_err("replacing /out/.bc/requests with a symlink must kill run");
    assert!(err.to_string().contains("MAILBOX_VIOLATION"), "{err}");
    assert!(killed.load(Ordering::Acquire));
}

#[cfg(unix)]
#[tokio::test]
async fn w2_abuse_fifo_is_rejected_without_blocking_and_kills_run() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = make_spec(tmp.path(), "run-abuse-fifo", 10);
    let killed = Arc::new(AtomicBool::new(false));
    let runner = SyntheticRunner {
        kind: EngineKind::Kubernetes,
        version: "1.31".into(),
        killed: Arc::clone(&killed),
        cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
        out_stats_override: None,
        body: Arc::new(|_in_dir, out_dir, kill| {
            Box::pin(async move {
                let fifo = out_dir.join(MAILBOX_REQUESTS_REL).join("fifo1.json");
                let status = std::process::Command::new("mkfifo")
                    .arg(&fifo)
                    .status()
                    .expect("run mkfifo");
                assert!(status.success(), "mkfifo must succeed");
                kill.notified().await;
                0
            })
        }),
    };

    // Must fail fast (well under 2s) without ever blocking on the FIFO!
    let err = tokio::time::timeout(
        Duration::from_secs(2),
        supervise(
            &runner,
            &spec,
            SuperviseOptions {
                live_query: Some(Arc::new(StrictLiveQueryEcho {
                    calls: AtomicUsize::new(0),
                })),
                poll_interval: Duration::from_millis(20),
                ..SuperviseOptions::default()
            },
        ),
    )
    .await
    .expect("FIFO rejection must never block")
    .expect_err("FIFO must be rejected");
    assert!(err.to_string().contains("MAILBOX_VIOLATION"), "{err}");
    assert!(err.to_string().contains("non-regular"), "{err}");
    assert!(killed.load(Ordering::Acquire));
}

#[tokio::test]
async fn w2_abuse_oversize_payload_kills_run_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = make_spec(tmp.path(), "run-abuse-oversize", 10);
    let killed = Arc::new(AtomicBool::new(false));
    let runner = SyntheticRunner {
        kind: EngineKind::Kubernetes,
        version: "1.31".into(),
        killed: Arc::clone(&killed),
        cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
        out_stats_override: None,
        body: Arc::new(|_in_dir, out_dir, kill| {
            Box::pin(async move {
                let huge = vec![b'a'; MAX_MAILBOX_REQUEST_BYTES + 16];
                atomic_write_request(&out_dir, "big1", &huge);
                kill.notified().await;
                0
            })
        }),
    };

    let err = supervise(
        &runner,
        &spec,
        SuperviseOptions {
            live_query: Some(Arc::new(StrictLiveQueryEcho {
                calls: AtomicUsize::new(0),
            })),
            poll_interval: Duration::from_millis(20),
            ..SuperviseOptions::default()
        },
    )
    .await
    .expect_err("oversize mailbox payload must kill run");
    assert!(err.to_string().contains("MAILBOX_VIOLATION"), "{err}");
    assert!(err.to_string().contains("exceeds limit"), "{err}");
    assert!(killed.load(Ordering::Acquire));
}

#[tokio::test]
async fn w2_abuse_duplicate_request_id_kills_run_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = make_spec(tmp.path(), "run-abuse-dup", 10);
    let killed = Arc::new(AtomicBool::new(false));
    let runner = SyntheticRunner {
        kind: EngineKind::Kubernetes,
        version: "1.31".into(),
        killed: Arc::clone(&killed),
        cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
        out_stats_override: None,
        body: Arc::new(|in_dir, out_dir, kill| {
            Box::pin(async move {
                let req = serde_json::to_vec(&json!({
                    "request_id": "dup-1",
                    "query": "sample_roster.v2",
                    "params": {}
                }))
                .unwrap();
                atomic_write_request(&out_dir, "dup-1", &req);
                let _ = wait_for_reply(&in_dir, "dup-1").await;
                // Re-submit same request_id!
                atomic_write_request(&out_dir, "dup-1", &req);
                kill.notified().await;
                0
            })
        }),
    };

    let err = supervise(
        &runner,
        &spec,
        SuperviseOptions {
            live_query: Some(Arc::new(StrictLiveQueryEcho {
                calls: AtomicUsize::new(0),
            })),
            poll_interval: Duration::from_millis(20),
            ..SuperviseOptions::default()
        },
    )
    .await
    .expect_err("duplicate request_id must kill run");
    assert!(err.to_string().contains("MAILBOX_VIOLATION"), "{err}");
    assert!(err.to_string().contains("duplicate"), "{err}");
    assert!(killed.load(Ordering::Acquire));
}

#[tokio::test]
async fn w2_abuse_call_limit_kills_run_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = make_spec(tmp.path(), "run-abuse-limit", 10);
    let killed = Arc::new(AtomicBool::new(false));
    let runner = SyntheticRunner {
        kind: EngineKind::Kubernetes,
        version: "1.31".into(),
        killed: Arc::clone(&killed),
        cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
        out_stats_override: None,
        body: Arc::new(|in_dir, out_dir, kill| {
            Box::pin(async move {
                for i in 1..=3 {
                    let id = format!("req-{i}");
                    let req = serde_json::to_vec(&json!({
                        "request_id": id,
                        "query": "sample_roster.v2",
                        "params": {}
                    }))
                    .unwrap();
                    atomic_write_request(&out_dir, &id, &req);
                    if i <= 2 {
                        let _ = wait_for_reply(&in_dir, &id).await;
                    }
                }
                kill.notified().await;
                0
            })
        }),
    };

    let err = supervise(
        &runner,
        &spec,
        SuperviseOptions {
            live_query: Some(Arc::new(StrictLiveQueryEcho {
                calls: AtomicUsize::new(0),
            })),
            max_mailbox_calls: 2,
            poll_interval: Duration::from_millis(20),
            ..SuperviseOptions::default()
        },
    )
    .await
    .expect_err("exceeding call limit must kill run");
    assert!(err.to_string().contains("MAILBOX_VIOLATION"), "{err}");
    assert!(err.to_string().contains("call limit"), "{err}");
    assert!(killed.load(Ordering::Acquire));
}

#[tokio::test]
async fn w2_abuse_path_traversal_and_id_mismatch_and_subdir_kill_run() {
    // 1. Subdirectory inside /out/.bc/requests
    {
        let tmp = tempfile::tempdir().unwrap();
        let spec = make_spec(tmp.path(), "run-abuse-subdir", 10);
        let killed = Arc::new(AtomicBool::new(false));
        let runner = SyntheticRunner {
            kind: EngineKind::Kubernetes,
            version: "1.31".into(),
            killed: Arc::clone(&killed),
            cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
            out_stats_override: None,
            body: Arc::new(|_in_dir, out_dir, kill| {
                Box::pin(async move {
                    std::fs::create_dir(out_dir.join(MAILBOX_REQUESTS_REL).join("sub.json"))
                        .unwrap();
                    kill.notified().await;
                    0
                })
            }),
        };
        let err = supervise(
            &runner,
            &spec,
            SuperviseOptions {
                live_query: Some(Arc::new(StrictLiveQueryEcho {
                    calls: AtomicUsize::new(0),
                })),
                poll_interval: Duration::from_millis(20),
                ..SuperviseOptions::default()
            },
        )
        .await
        .expect_err("subdirectory in mailbox must kill run");
        assert!(err.to_string().contains("MAILBOX_VIOLATION"), "{err}");
        assert!(killed.load(Ordering::Acquire));
    }

    // 2. Invalid filename with traversal `..`
    {
        let tmp = tempfile::tempdir().unwrap();
        let spec = make_spec(tmp.path(), "run-abuse-dotdot", 10);
        let killed = Arc::new(AtomicBool::new(false));
        let runner = SyntheticRunner {
            kind: EngineKind::Kubernetes,
            version: "1.31".into(),
            killed: Arc::clone(&killed),
            cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
            out_stats_override: None,
            body: Arc::new(|_in_dir, out_dir, kill| {
                Box::pin(async move {
                    std::fs::write(
                        out_dir.join(MAILBOX_REQUESTS_REL).join("a..b.json"),
                        b"{}",
                    )
                    .unwrap();
                    kill.notified().await;
                    0
                })
            }),
        };
        let err = supervise(
            &runner,
            &spec,
            SuperviseOptions {
                live_query: Some(Arc::new(StrictLiveQueryEcho {
                    calls: AtomicUsize::new(0),
                })),
                poll_interval: Duration::from_millis(20),
                ..SuperviseOptions::default()
            },
        )
        .await
        .expect_err("a..b.json must be rejected");
        assert!(err.to_string().contains("MAILBOX_VIOLATION"), "{err}");
        assert!(killed.load(Ordering::Acquire));
    }

    // 3. Filename stem vs JSON `request_id` mismatch
    {
        let tmp = tempfile::tempdir().unwrap();
        let spec = make_spec(tmp.path(), "run-abuse-mismatch", 10);
        let killed = Arc::new(AtomicBool::new(false));
        let runner = SyntheticRunner {
            kind: EngineKind::Kubernetes,
            version: "1.31".into(),
            killed: Arc::clone(&killed),
            cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
            out_stats_override: None,
            body: Arc::new(|_in_dir, out_dir, kill| {
                Box::pin(async move {
                    let req = serde_json::to_vec(&json!({
                        "request_id": "other-id",
                        "query": "sample_roster.v2",
                        "params": {}
                    }))
                    .unwrap();
                    atomic_write_request(&out_dir, "file-id", &req);
                    kill.notified().await;
                    0
                })
            }),
        };
        let err = supervise(
            &runner,
            &spec,
            SuperviseOptions {
                live_query: Some(Arc::new(StrictLiveQueryEcho {
                    calls: AtomicUsize::new(0),
                })),
                poll_interval: Duration::from_millis(20),
                ..SuperviseOptions::default()
            },
        )
        .await
        .expect_err("request_id mismatch must kill run");
        assert!(err.to_string().contains("MAILBOX_VIOLATION"), "{err}");
        assert!(killed.load(Ordering::Acquire));
    }
}

struct MockChildBroker {
    delay: Duration,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl ChildRunBroker for MockChildBroker {
    async fn run_child(
        &self,
        ctx: &ChildRunBrokerContext,
        request: ChildModuleRequest,
    ) -> CompassResult<ChildRunReply> {
        let idx = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let alias = child_artifact_alias(&request.request_id);
        let staged_path = stage_child_artifact_bytes(
            &ctx.parent_in_dir,
            &alias,
            "scores.csv",
            b"allele,score\nA0201,0.99\n",
        )?;
        let receipt_hash = format!("{:064x}", idx);
        Ok(ChildRunReply {
            request_id: request.request_id,
            ok: true,
            status: Some(RunStatus::Succeeded),
            run_id: Some(format!("run-child-{idx}")),
            receipt_hash: Some(receipt_hash),
            result_json: Some(json!({ "plugin": request.plugin_id, "passed": true })),
            artifacts: vec![ChildRunStagedArtifact {
                name: "scores.csv".into(),
                sha256: "aa".repeat(32),
                size_bytes: 23,
                media_type: "text/csv".into(),
                staged_path: Some(staged_path),
            }],
            error_code: None,
            error: None,
        })
    }
}

#[tokio::test]
async fn w7_off_list_child_module_is_rejected_without_invoking_broker() {
    // Case A: Parent has child broker + allow-list ["alithea/allowed"]; requesting "alithea/forbidden"
    // returns structured CHILD_MODULE_NOT_ALLOWED reply and NEVER invokes the broker.
    {
        let tmp = tempfile::tempdir().unwrap();
        let spec = make_spec(tmp.path(), "run-w7-offlist-a", 10);
        let broker = Arc::new(MockChildBroker {
            delay: Duration::ZERO,
            calls: AtomicUsize::new(0),
        });
        let killed = Arc::new(AtomicBool::new(false));
        let runner = SyntheticRunner {
            kind: EngineKind::Kubernetes,
            version: "1.31".into(),
            killed: Arc::clone(&killed),
            cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
            out_stats_override: None,
            body: Arc::new(|in_dir, out_dir, _kill| {
                Box::pin(async move {
                    let req = serde_json::to_vec(&json!({
                        "kind": "run_module",
                        "request_id": "child-bad",
                        "plugin_id": "alithea/forbidden",
                        "params": {}
                    }))
                    .unwrap();
                    atomic_write_request(&out_dir, "child-bad", &req);
                    let reply = wait_for_reply(&in_dir, "child-bad").await;
                    assert_eq!(reply["ok"], false);
                    assert_eq!(reply["error_code"], "CHILD_MODULE_NOT_ALLOWED");
                    0
                })
            }),
        };

        let report = supervise(
            &runner,
            &spec,
            SuperviseOptions {
                child_broker: Some(broker.clone()),
                allowed_child_modules: vec!["alithea/allowed".into()],
                max_child_wait_sec: Some(60),
                poll_interval: Duration::from_millis(20),
                ..SuperviseOptions::default()
            },
        )
        .await
        .expect("notebook receives structured CHILD_MODULE_NOT_ALLOWED reply");
        assert_eq!(report.exit_code, 0);
        assert!(report.child_receipt_hashes.is_empty());
        assert_eq!(
            broker.calls.load(Ordering::SeqCst),
            0,
            "off-list child module MUST NEVER reach ChildRunBroker"
        );
    }

    // Case B: Non-parent module without ChildRunBroker trying `run_module` is killed immediately!
    {
        let tmp = tempfile::tempdir().unwrap();
        let spec = make_spec(tmp.path(), "run-w7-offlist-b", 10);
        let killed = Arc::new(AtomicBool::new(false));
        let runner = SyntheticRunner {
            kind: EngineKind::Kubernetes,
            version: "1.31".into(),
            killed: Arc::clone(&killed),
            cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
            out_stats_override: None,
            body: Arc::new(|_in_dir, out_dir, kill| {
                Box::pin(async move {
                    let req = serde_json::to_vec(&json!({
                        "kind": "run_module",
                        "request_id": "sneaky",
                        "plugin_id": "alithea/motif",
                        "params": {}
                    }))
                    .unwrap();
                    atomic_write_request(&out_dir, "sneaky", &req);
                    kill.notified().await;
                    0
                })
            }),
        };
        let err = supervise(
            &runner,
            &spec,
            SuperviseOptions {
                live_query: Some(Arc::new(StrictLiveQueryEcho {
                    calls: AtomicUsize::new(0),
                })),
                poll_interval: Duration::from_millis(20),
                ..SuperviseOptions::default()
            },
        )
        .await
        .expect_err("run_module without child broker must kill run");
        assert_eq!(err.error_code(), ErrorCode::ChildModuleNotAllowed);
        assert!(killed.load(Ordering::Acquire));
    }
}

#[tokio::test]
async fn w7_parent_deadline_pauses_during_child_run_and_enforces_max_child_wait_sec() {
    // 1. Parent timeout = 1s, child takes 1.4s, max_child_wait_sec = 5s -> SUCCEEDS because parent clock pauses!
    {
        let tmp = tempfile::tempdir().unwrap();
        let spec = make_spec(tmp.path(), "run-w7-pause-ok", 1);
        let broker = Arc::new(MockChildBroker {
            delay: Duration::from_millis(1_400),
            calls: AtomicUsize::new(0),
        });
        let killed = Arc::new(AtomicBool::new(false));
        let runner = SyntheticRunner {
            kind: EngineKind::Kubernetes,
            version: "1.31".into(),
            killed: Arc::clone(&killed),
            cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
            out_stats_override: None,
            body: Arc::new(|in_dir, out_dir, _kill| {
                Box::pin(async move {
                    let req = serde_json::to_vec(&json!({
                        "kind": "run_module",
                        "request_id": "child-slow-1",
                        "plugin_id": "alithea/motif",
                        "params": { "k": 9 },
                        "catalog_id": "alithea/hla-demo",
                        "timeout_sec": 30
                    }))
                    .unwrap();
                    atomic_write_request(&out_dir, "child-slow-1", &req);
                    let reply = wait_for_reply(&in_dir, "child-slow-1").await;
                    assert_eq!(reply["ok"], true);
                    assert_eq!(reply["run_id"], "run-child-1");
                    let staged = reply["artifacts"][0]["staged_path"].as_str().unwrap();
                    assert_eq!(staged, "/in/artifacts/child-child-slow-1/scores.csv");
                    let host_artifact = in_dir
                        .join("artifacts")
                        .join("child-child-slow-1")
                        .join("scores.csv");
                    assert_eq!(
                        std::fs::read_to_string(&host_artifact).unwrap(),
                        "allele,score\nA0201,0.99\n"
                    );
                    0
                })
            }),
        };

        let report = supervise(
            &runner,
            &spec,
            SuperviseOptions {
                child_broker: Some(broker.clone()),
                allowed_child_modules: vec!["alithea/motif".into()],
                max_child_wait_sec: Some(5),
                poll_interval: Duration::from_millis(25),
                ..SuperviseOptions::default()
            },
        )
        .await
        .expect("parent run must succeed because wall-clock paused while child ran");

        assert_eq!(report.exit_code, 0);
        assert!(!killed.load(Ordering::Acquire));
        assert_eq!(report.child_receipt_hashes, vec![format!("{:064x}", 1)]);
        assert!(
            report.child_wait_ms >= 1_200,
            "expected child_wait_ms >= 1200, got {}",
            report.child_wait_ms
        );
    }

    // 2. Child exceeds signed max_child_wait_sec (1s) -> KILLED with CONTAINER_TIMEOUT!
    {
        let tmp = tempfile::tempdir().unwrap();
        let spec = make_spec(tmp.path(), "run-w7-pause-exceed", 10);
        let broker = Arc::new(MockChildBroker {
            delay: Duration::from_secs(5),
            calls: AtomicUsize::new(0),
        });
        let killed = Arc::new(AtomicBool::new(false));
        let runner = SyntheticRunner {
            kind: EngineKind::Kubernetes,
            version: "1.31".into(),
            killed: Arc::clone(&killed),
            cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
            out_stats_override: None,
            body: Arc::new(|_in_dir, out_dir, kill| {
                Box::pin(async move {
                    let req = serde_json::to_vec(&json!({
                        "kind": "run_module",
                        "request_id": "child-hang",
                        "plugin_id": "alithea/motif",
                        "params": {}
                    }))
                    .unwrap();
                    atomic_write_request(&out_dir, "child-hang", &req);
                    kill.notified().await;
                    0
                })
            }),
        };

        let err = tokio::time::timeout(
            Duration::from_secs(4),
            supervise(
                &runner,
                &spec,
                SuperviseOptions {
                    child_broker: Some(broker),
                    allowed_child_modules: vec!["alithea/motif".into()],
                    max_child_wait_sec: Some(1),
                    poll_interval: Duration::from_millis(25),
                    ..SuperviseOptions::default()
                },
            ),
        )
        .await
        .expect("max_child_wait_sec enforcement must complete quickly")
        .expect_err("exceeding max_child_wait_sec must time out");

        assert!(err.to_string().contains("CONTAINER_TIMEOUT"), "{err}");
        assert!(err.to_string().contains("max_child_wait_sec"), "{err}");
        assert!(killed.load(Ordering::Acquire));
    }
}

#[tokio::test]
async fn w1_w3_supervise_enforces_entry_count_cap_free_space_watchdog_and_cancel() {
    // 1. Entry count cap
    {
        let tmp = tempfile::tempdir().unwrap();
        let spec = make_spec(tmp.path(), "run-w3-entries", 10);
        let killed = Arc::new(AtomicBool::new(false));
        let runner = SyntheticRunner {
            kind: EngineKind::Kubernetes,
            version: "1.31".into(),
            killed: Arc::clone(&killed),
            cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
            out_stats_override: Some(OutDirStats {
                regular_bytes: 100,
                entry_count: 501,
                free_bytes: Some(10 * 1024 * 1024),
            }),
            body: Arc::new(|_in_dir, _out_dir, kill| {
                Box::pin(async move {
                    kill.notified().await;
                    0
                })
            }),
        };
        let err = supervise(
            &runner,
            &spec,
            SuperviseOptions {
                max_output_entries: Some(500),
                poll_interval: Duration::from_millis(20),
                ..SuperviseOptions::default()
            },
        )
        .await
        .expect_err("exceeding max_output_entries must abort");
        assert!(err.to_string().contains("OUTPUT_QUOTA_EXCEEDED"), "{err}");
        assert!(err.to_string().contains("entry count"), "{err}");
        assert!(killed.load(Ordering::Acquire));
    }

    // 2. Free-space watchdog
    {
        let tmp = tempfile::tempdir().unwrap();
        let spec = make_spec(tmp.path(), "run-w3-freespace", 10);
        let killed = Arc::new(AtomicBool::new(false));
        let runner = SyntheticRunner {
            kind: EngineKind::Kubernetes,
            version: "1.31".into(),
            killed: Arc::clone(&killed),
            cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
            out_stats_override: Some(OutDirStats {
                regular_bytes: 100,
                entry_count: 2,
                free_bytes: Some(64 * 1024),
            }),
            body: Arc::new(|_in_dir, _out_dir, kill| {
                Box::pin(async move {
                    kill.notified().await;
                    0
                })
            }),
        };
        let err = supervise(
            &runner,
            &spec,
            SuperviseOptions {
                min_free_bytes: Some(1024 * 1024),
                poll_interval: Duration::from_millis(20),
                ..SuperviseOptions::default()
            },
        )
        .await
        .expect_err("low free space must abort");
        assert!(err.to_string().contains("OUTPUT_QUOTA_EXCEEDED"), "{err}");
        assert!(err.to_string().contains("free space"), "{err}");
        assert!(killed.load(Ordering::Acquire));
    }

    // 3. Explicit cancel flag kills running pod
    {
        let tmp = tempfile::tempdir().unwrap();
        let spec = make_spec(tmp.path(), "run-w5-cancel", 10);
        let killed = Arc::new(AtomicBool::new(false));
        let runner = SyntheticRunner {
            kind: EngineKind::Kubernetes,
            version: "1.31".into(),
            killed: Arc::clone(&killed),
            cancelled_runs: Arc::new(std::sync::Mutex::new(Vec::new())),
            out_stats_override: None,
            body: Arc::new(|_in_dir, _out_dir, kill| {
                Box::pin(async move {
                    kill.notified().await;
                    0
                })
            }),
        };
        let cancelled = AtomicBool::new(false);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let trigger = async {
            rx.await.expect("started");
            cancelled.store(true, Ordering::Release);
        };
        let (res, ()) = tokio::join!(
            supervise(
                &runner,
                &spec,
                SuperviseOptions {
                    cancelled: Some(&cancelled),
                    started_tx: Some(tx),
                    poll_interval: Duration::from_millis(20),
                    ..SuperviseOptions::default()
                },
            ),
            trigger,
        );
        let err = res.expect_err("cancelled run must abort");
        assert!(err.to_string().contains("RUN_CANCELLED"), "{err}");
        assert!(killed.load(Ordering::Acquire));
    }
}
