//! Explicit live regression against a disposable, labelled Docker daemon and a private fixture registry.
use biocompass_core_traits::{OciImageRef, RunId};
use biocompass_oci_runner::{ContainerRunSpec, DetectedEngine, EngineKind, OciContainerRunner};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn docker(path: &Path, args: &[&str]) -> String {
    let o = Command::new(path).args(args).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

fn inventory(cli: &Path) -> Value {
    let ids = docker(cli, &["image", "ls", "-aq", "--no-trunc"]);
    let ids: std::collections::BTreeSet<_> = ids.split_whitespace().collect();
    let mut refs = Vec::new();
    let mut bytes = 0;
    for id in ids {
        let raw = docker(cli, &["image", "inspect", id]);
        let r = serde_json::from_str::<Value>(&raw).unwrap();
        bytes += r[0]["Size"].as_u64().unwrap();
        refs.extend(r[0]["RepoDigests"].as_array().unwrap().clone());
    }
    json!({"retained_bytes":bytes,"refs":refs})
}

fn spec(reference: &str, dir: &Path, cmd: &str) -> ContainerRunSpec {
    ContainerRunSpec {
        run_id: RunId::from("cache-probe"),
        image: OciImageRef::parse(reference, false).unwrap(),
        in_dir: dir.join("in"),
        out_dir: dir.join("out"),
        resources: Default::default(),
        network: Default::default(),
        command_override: Some((
            "/bin/busybox".into(),
            vec!["sh".into(), "-c".into(), cmd.into()],
        )),
    }
}

#[tokio::test]
#[ignore = "requires four private inert images and an explicitly labelled, isolated Docker daemon"]
async fn retained_cache_live() {
    let env = |k| std::env::var(k).unwrap();
    let cli = PathBuf::from(env("BC_CACHE_QA_DOCKER"));
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&docker(
            &cli,
            &["info", "--format", "{{json .Labels}}"]
        ))
        .unwrap(),
        vec!["io.biocompass.worker-cache=bc-pilot-cache-test"]
    );
    assert_eq!(inventory(&cli)["retained_bytes"], 0);
    let fixture: Value =
        serde_json::from_slice(&std::fs::read(env("BC_CACHE_QA_IMAGES")).unwrap()).unwrap();
    let img = |key: &str| fixture[key]["ref"].as_str().unwrap().to_owned();
    let (v1, v2, v3, v4) = (img("v1"), img("v2"), img("v3"), img("v4"));
    let limit = fixture["v1"]["bytes"].as_u64().unwrap() * 2;
    let raw = || {
        OciContainerRunner::from_engine(DetectedEngine {
            kind: EngineKind::Docker,
            binary_path: cli.clone(),
            server_version: "isolated-test".into(),
            rootless: false,
        })
    };
    let owner = "bc-pilot-cache-test";
    let state = tempfile::tempdir().unwrap();
    let error = raw()
        .with_dedicated_worker_cache("wrong-engine", limit, &[], state.path())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ownership label"));
    let runner = raw()
        .with_dedicated_worker_cache(owner, limit, std::slice::from_ref(&v1), state.path())
        .await
        .unwrap();
    runner.pull_image(&v2).await.unwrap();
    runner.pull_image(&v2).await.unwrap();
    let filled = inventory(&cli);
    assert_eq!(filled["retained_bytes"], limit);
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("in")).unwrap();
    std::fs::create_dir(dir.path().join("out")).unwrap();
    let mut missing_tag = spec(&v1, dir.path(), "exit 0");
    missing_tag.image = OciImageRef::parse("local/cache-fixture:missing", true).unwrap();
    let missing = runner.execute(&missing_tag, None).await.unwrap();
    assert_ne!(missing.exit_code, 0);
    assert!(
        missing
            .stderr_lines
            .iter()
            .any(|line| line.contains("No such image")),
        "{:?}",
        missing.stderr_lines
    );
    assert_eq!(inventory(&cli), filled);
    let live = runner.clone();
    let work = spec(
        &v2,
        dir.path(),
        "/bin/busybox cat /version; /bin/busybox sleep 12",
    );
    let job = tokio::spawn(async move { live.execute(&work, None).await });
    let mut running = false;
    for _ in 0..50 {
        if !docker(&cli, &["ps", "-q"]).trim().is_empty() {
            running = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !running {
        panic!("normal run did not start: {:?}", job.await);
    }
    let container = docker(&cli, &["ps", "-q"]);
    assert_eq!(
        docker(
            &cli,
            &[
                "inspect",
                "--format",
                "{{.HostConfig.LogConfig.Type}}",
                container.trim()
            ]
        )
        .trim(),
        "none"
    );
    let refusal = runner.pull_image(&v3).await.unwrap_err().to_string();
    assert!(refusal.contains("worker image cache full"), "{refusal}");
    assert_eq!(inventory(&cli), filled);
    let report = job.await.unwrap().unwrap();
    assert_eq!(report.exit_code, 0);
    assert_eq!(report.stdout_lines, ["v2"]);
    runner.pull_image(&v3).await.unwrap();
    assert!(
        runner.has_verified_local_digest(&v3).await && runner.has_verified_local_digest(&v1).await
    );
    assert!(!runner.has_verified_local_digest(&v2).await);
    runner.pull_image(&v4).await.unwrap();
    assert!(!runner.has_verified_local_digest(&v3).await);
    let updated = inventory(&cli);
    assert_eq!(updated["retained_bytes"], limit);
    // v1 was created first; using it last makes LRU differ from creation order.
    runner.pull_image(&v1).await.unwrap();
    drop(runner);
    let restart = Command::new(env("BC_CACHE_QA_RESTART")).status().unwrap();
    assert!(restart.success());
    assert_eq!(inventory(&cli), updated);
    let reopened = raw()
        .with_dedicated_worker_cache(owner, limit, &[], state.path())
        .await
        .unwrap();
    reopened.pull_image(&v3).await.unwrap();
    assert!(reopened.has_verified_local_digest(&v1).await);
    assert!(!reopened.has_verified_local_digest(&v4).await);
    drop(reopened);
    let next = raw()
        .with_dedicated_worker_cache(owner, limit, std::slice::from_ref(&v4), state.path())
        .await
        .unwrap();
    let report = next
        .execute(&spec(&v2, dir.path(), "/bin/busybox cat /version"), None)
        .await
        .unwrap();
    assert_eq!(report.exit_code, 0);
    assert_eq!(report.stdout_lines, ["v2"]);
    assert!(next.has_verified_local_digest(&v2).await && next.has_verified_local_digest(&v4).await);
    assert!(!next.has_verified_local_digest(&v1).await);
    let reloaded = inventory(&cli);
    drop(next);
    let tiny = raw()
        .with_dedicated_worker_cache(owner, 64, &[], state.path())
        .await
        .unwrap();
    let event_time = || {
        docker(&cli, &["info", "--format", "{{.SystemTime}}"])
            .trim()
            .to_owned()
    };
    let since = event_time();
    let oversized = tiny.pull_image(&v1).await.unwrap_err().to_string();
    assert!(oversized.contains("cache full"));
    let until = event_time();
    let pull_events = docker(
        &cli,
        &[
            "events",
            "--since",
            &since,
            "--until",
            &until,
            "--filter",
            "type=image",
            "--filter",
            "event=pull",
        ],
    );
    assert!(
        pull_events.trim().is_empty(),
        "unexpected registry pull: {pull_events}"
    );
    assert_eq!(inventory(&cli)["retained_bytes"], 0);
    std::fs::write(env("BC_CACHE_QA_PROOF"),serde_json::to_vec_pretty(&json!({"limit_bytes":limit,
      "filled":filled,"active_and_protected_refusal":refusal,"updated":updated,"engine_restart_preserved_bytes":true,
      "old_digest_rerun_stdout":report.stdout_lines,"reloaded":reloaded,"oversized_refusal":oversized,
      "after_oversized":inventory(&cli),"reopened_lru_evicted_newer_creation":true,"known_oversized_pull_events":pull_events.lines().count(),"attached_output_not_retained_by_docker":true,"wrong_engine_rejected":true,"missing_local_tag_no_implicit_pull":true})).unwrap()).unwrap();
}
