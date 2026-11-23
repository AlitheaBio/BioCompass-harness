use super::*;
use std::os::unix::fs::PermissionsExt;

fn fixture(root: &Path, mode: &str) -> (OciContainerRunner, ContainerRunSpec) {
    let bin = root.join("docker");
    std::fs::write(root.join("mode"), mode).unwrap();
    std::fs::write(
        &bin,
        r#"#!/bin/sh
set -eu
cd "${0%/*}"
case "$1" in
  run)
    echo "$$" > run.pid
    exec sleep 15
    ;;
  rm)
    echo "$$" > rm.pid
    printf '%s' "$3" > rm.target
    case "$(cat mode)" in
      success) exit 0 ;;
      reject) exit 23 ;;
      stall) exec sleep 14 ;;
    esac
    ;;
  *) exit 65 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let runner = OciContainerRunner::from_engine(DetectedEngine {
        kind: EngineKind::Docker,
        binary_path: bin,
        server_version: "fixture".into(),
        rootless: false,
    });
    let spec = ContainerRunSpec {
        run_id: RunId::from("deadline-test"),
        image: OciImageRef::parse("local/check:one", true).unwrap(),
        in_dir: root.join("in"),
        out_dir: root.join("out"),
        resources: PluginResources {
            timeout_sec: 1,
            ..Default::default()
        },
        network: NetworkPolicy::default(),
        command_override: None,
    };
    (runner, spec)
}

fn assert_reaped(root: &Path, file: &str) {
    let pid = std::fs::read_to_string(root.join(file)).unwrap();
    // Check process visibility before treating a failed signal check as proof.
    let own_pid = std::process::id().to_string();
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-0", &own_pid])
            .status()
            .unwrap()
            .success()
    );
    // On Unix kill(0) also sees zombie processes until the parent calls wait().
    let status = std::process::Command::new("/bin/kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "{file} {} still exists", pid.trim());
}

async fn check_timeout(mode: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let (runner, spec) = fixture(tmp.path(), mode);
    let err = tokio::time::timeout(Duration::from_secs(8), runner.execute(&spec, None))
        .await
        .expect("the unresponsive cleanup CLI blocked terminal settlement")
        .expect_err("the run missed its deadline")
        .to_string();
    let name = std::fs::read_to_string(tmp.path().join("rm.target")).unwrap();
    assert!(
        err.contains("CONTAINER_TIMEOUT") && err.contains(&name),
        "{err}"
    );
    if mode == "success" {
        assert!(!err.contains("OCI_CLEANUP_UNCONFIRMED"), "{err}");
    } else {
        assert!(err.contains("OCI_CLEANUP_UNCONFIRMED"), "{err}");
        assert!(!err.contains("was terminated"), "{err}");
    }
    assert_reaped(tmp.path(), "run.pid");
    assert_reaped(tmp.path(), "rm.pid");
}

#[tokio::test]
async fn successful_removal_finishes_a_timed_out_run() {
    check_timeout("success").await;
}

#[tokio::test]
async fn stalled_removal_finishes_a_timed_out_run_and_reaps_both_clients() {
    check_timeout("stall").await;
}

#[tokio::test]
async fn rejected_removal_reports_an_unconfirmed_container() {
    check_timeout("reject").await;
}

#[tokio::test]
async fn cleanup_is_scoped_to_the_publisher_and_workspace_and_uses_immutable_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (engine, _) = fixture(root, "success");
    let other_path = root.join("second");
    std::fs::create_dir(&other_path).unwrap();
    let ours = engine.for_native_workspace(root, "owner").unwrap();
    let other = engine.for_native_workspace(&other_path, "owner").unwrap();
    let third = engine.for_native_workspace(root, "another-key").unwrap();
    let run = RunId::from("run-fixture");
    let own_name = ours.container_name(&run);
    let other_name = other.container_name(&run);
    assert_ne!(own_name, other_name);
    assert_ne!(own_name, third.container_name(&run));
    let mut args = vec!["run".into(), "image".into()];
    ours.label_native_job(&mut args);
    assert_eq!(args[1], "--label");
    let ownership_label = &args[2];
    std::fs::write(
        root.join("expected-label"),
        format!("label={ownership_label}"),
    )
    .unwrap();
    let id = "1".repeat(64);
    let listing = format!(
        "{id} {own_name}\n{} {other_name}\n{} biocompass-job-deadbeef\nshort {own_name}\n",
        "2".repeat(64),
        "3".repeat(64)
    );
    std::fs::write(root.join("listing"), listing).unwrap();
    std::fs::write(
        &ours.engine.binary_path,
        r#"#!/bin/sh
set -eu
cd "${0%/*}"
case "$1" in
  ps)
    [ "$5" = "$(cat expected-label)" ] || exit 6
    cat listing
    ;;
  rm) printf '%s' "$3" > rm.target ;;
  *) exit 4 ;;
esac
"#,
    )
    .unwrap();
    let jobs = ours.native_job_containers().await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].run_id(), &run);
    assert_eq!(jobs[0].engine_id(), id);
    assert!(other.remove_abandoned_container(&jobs[0]).await.is_err());
    assert!(!root.join("rm.target").exists());
    ours.remove_abandoned_container(&jobs[0]).await.unwrap();
    assert_eq!(std::fs::read_to_string(root.join("rm.target")).unwrap(), id);
}
