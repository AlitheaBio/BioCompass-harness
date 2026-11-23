use super::*;
use std::os::unix::fs::PermissionsExt;

const PIN: &str = "registry.test/qa/echo@sha256:27f4fed6472fe2039b6f6298bc36a6f06fb1fb9a440c94aeedca51622a172e4b";

fn setup(root: &Path) -> (OciContainerRunner, ContainerRunSpec) {
    let cli = root.join("engine");
    std::fs::write(
        &cli,
        r#"#!/bin/sh
set -eu
cd "${0%/*}"
case "$1" in
    version) echo linux/arm64 ;;
    image)
        test -f cached
        printf '[{"RepoDigests":["%s"],"Os":"linux","Architecture":"arm64"}]\n' "$3"
        ;;
    pull)
        echo pull >> events
        sleep 2
        touch cached
        ;;
    run)
        echo run >> events
        if ! test -f cached; then
            # Emulate the engine's ordinary implicit pull when run omits --pull=never.
            case " $* " in *" --pull=never "*) exit 1 ;; esac
            sleep 2
        else
            case " $* " in *" --pull=never "*) ;; *) exit 2 ;; esac
        fi
        if test -f slow-job; then exec sleep 5; fi
        ;;
    rm) echo removed >> events ;;
    *) exit 1 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let runner = OciContainerRunner::from_engine(DetectedEngine {
        kind: EngineKind::Docker,
        binary_path: cli,
        server_version: "fixture".into(),
        rootless: false,
    });
    let spec = ContainerRunSpec {
        run_id: RunId::from("cold-image-check"),
        image: OciImageRef::parse(PIN, false).unwrap(),
        in_dir: root.join("in"),
        out_dir: root.join("out"),
        resources: PluginResources { timeout_sec: 1, ..Default::default() },
        network: NetworkPolicy::default(),
        command_override: None,
    };
    (runner, spec)
}

#[tokio::test]
async fn cold_pull_does_not_consume_the_job_deadline() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (runner, spec) = setup(root);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let observe = async {
        rx.await.expect("worker received execution start");
        assert!(root.join("cached").is_file(), "did not wait for the cold image");
    };
    let (run, ()) = tokio::join!(runner.execute_with_start_notification(&spec, None, None, None, Some(tx)), observe);
    assert_eq!(run.unwrap().exit_code, 0);
    assert_eq!(std::fs::read_to_string(root.join("events")).unwrap(), "pull\nrun\n");

    std::fs::write(root.join("slow-job"), "").unwrap();
    let error = runner.execute(&spec, None).await.unwrap_err().to_string();
    assert!(error.contains("CONTAINER_TIMEOUT"), "{error}");
    assert_eq!(std::fs::read_to_string(root.join("events")).unwrap(), "pull\nrun\nrun\nremoved\n");
}

#[tokio::test]
async fn revocation_during_pull_does_not_launch_the_job() {
    let tmp = tempfile::tempdir().unwrap();
    let (runner, spec) = setup(tmp.path());
    let revoked = AtomicBool::new(false);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let events = tmp.path().join("events");
    let revoke = async {
        while !events.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        revoked.store(true, Ordering::Release);
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(runner.execute_with_start_notification(&spec, None, Some(&revoked), None, Some(tx)), revoke)
    }).await.unwrap();
    assert!(result.unwrap_err().to_string().contains("DATASET_ACCESS_REVOKED"));
    assert!(rx.await.is_err(), "start event sent for unexecuted job");
    assert_eq!(std::fs::read_to_string(events).unwrap(), "pull\n");
}
