use cronharbor::external::{self, DockerSpec, ExecutorPolicy, Identity};

#[test]
fn external_executors_are_disabled_and_require_explicit_policy() {
    let dir = tempfile::tempdir().unwrap();
    let policy = external::load_policy(dir.path()).unwrap();
    let spec = DockerSpec {
        image: format!("alpine@sha256:{}", "a".repeat(64)),
        user: "10001:10001".into(),
        network: "none".into(),
        memory_bytes: 67_108_864,
        pids_limit: 32,
    };
    assert!(external::validate_docker(&policy, &spec).is_err());
}

#[test]
fn docker_policy_rejects_root_unpinned_images_and_excessive_resources() {
    let image = format!("alpine@sha256:{}", "a".repeat(64));
    let policy = ExecutorPolicy {
        docker_enabled: true,
        images: vec![image.clone()],
        ..ExecutorPolicy::default()
    };
    let mut spec = DockerSpec {
        image,
        user: "10001:10001".into(),
        network: "none".into(),
        memory_bytes: 67_108_864,
        pids_limit: 32,
    };
    assert!(external::validate_docker(&policy, &spec).is_ok());
    spec.user = "0".into();
    assert!(external::validate_docker(&policy, &spec).is_err());
    spec.user = "10001".into();
    spec.image = "alpine:latest".into();
    assert!(external::validate_docker(&policy, &spec).is_err());
    spec.image = policy.images[0].clone();
    spec.network = "host".into();
    assert!(external::validate_docker(&policy, &spec).is_err());
}

#[test]
fn docker_identity_binds_every_owner_and_launch_nonce() {
    let identity = Identity {
        instance_id: "instance".into(),
        workload_id: "workload".into(),
        revision_id: "revision".into(),
        execution_id: "execution".into(),
        launch_nonce: uuid::Uuid::new_v4().to_string(),
    };
    let handle = external::docker_handle(&identity);
    let json = serde_json::to_value(handle).unwrap();
    assert_eq!(json["labels"]["dev.cronharbor.execution"], "execution");
    assert_eq!(
        json["labels"]["dev.cronharbor.launch"],
        identity.launch_nonce
    );
}

#[test]
fn systemd_bridge_only_controls_existing_allowlisted_units() {
    use cronharbor::external::{ExternalHandle, ExternalState, SystemdSpec};
    use std::{fs, os::unix::fs::PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let systemctl = directory.path().join("systemctl");
    fs::write(
        &systemctl,
        r##"#!/bin/sh
printf '%s\n' "$@" >> "$0.calls"
if [ "$1" = '--user' ]; then shift; fi
case "$1" in
start) printf active > "$0.state" ;;
stop) printf inactive > "$0.state" ;;
show)
  state=inactive
  if [ -f "$0.state" ]; then state=$(cat "$0.state"); fi
  printf 'LoadState=loaded\nActiveState=%s\nSubState=running\nExecMainStatus=0\n' "$state"
  ;;
*) exit 90 ;;
esac
"##,
    )
    .unwrap();
    fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o700)).unwrap();
    let policy = ExecutorPolicy {
        systemd_enabled: true,
        systemctl_binary: systemctl.to_str().unwrap().into(),
        systemd_user: true,
        units: vec!["reviewed-worker.service".into()],
        ..ExecutorPolicy::default()
    };
    assert!(
        external::start_systemd(
            &policy,
            &SystemdSpec {
                unit: "other.service".into()
            }
        )
        .is_err()
    );
    assert!(!systemctl.with_extension("calls").exists());
    let handle = external::start_systemd(
        &policy,
        &SystemdSpec {
            unit: "reviewed-worker.service".into(),
        },
    )
    .unwrap();
    assert!(matches!(handle, ExternalHandle::Systemd { .. }));
    assert_eq!(
        external::inspect(&policy, &handle).unwrap().state,
        ExternalState::Running
    );
    assert!(external::stop(&policy, &handle, 1).unwrap());
    assert_eq!(
        external::inspect(&policy, &handle).unwrap().state,
        ExternalState::Exited
    );
    let calls = fs::read_to_string(systemctl.with_extension("calls")).unwrap();
    assert!(calls.contains("--user\nstart\nreviewed-worker.service\n"));
    assert!(calls.contains("--user\nstop\nreviewed-worker.service\n"));
    assert!(!calls.contains("set-property"));
    assert!(!calls.contains("daemon-reload"));
    let journalctl = directory.path().join("journalctl");
    fs::write(
        &journalctl,
        r##"#!/bin/sh
printf '%s\n' "$@" > "$0.calls"
case "$*" in
*--since=*) printf 'current-attempt\n' ;;
*) printf 'old-unit-history\n' ;;
esac
"##,
    )
    .unwrap();
    fs::set_permissions(&journalctl, fs::Permissions::from_mode(0o700)).unwrap();
    let policy = ExecutorPolicy {
        journalctl_binary: journalctl.to_str().unwrap().into(),
        ..policy
    };
    let (logs, truncated) = external::logs(&policy, &handle, 4096, &[]).unwrap();
    assert_eq!(logs, b"current-attempt\n");
    assert!(!truncated);
    let calls = fs::read_to_string(journalctl.with_extension("calls")).unwrap();
    assert!(calls.contains("--user\n"));
    assert!(calls.contains("--since="));
    assert!(calls.contains(" UTC\n"));
    let unbounded = ExternalHandle::Systemd {
        unit: "reviewed-worker.service".into(),
        since_us: None,
    };
    assert!(external::logs(&policy, &unbounded, 4096, &[]).is_err());
}

#[test]
fn docker_log_capture_does_not_silently_discard_earlier_lines() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("docker");
    fs::write(
        &binary,
        r##"#!/bin/sh
shift 2
case "$1" in
inspect) cat "$0.inspect" ;;
logs)
  if [ "$2" = '--tail' ]; then tail -n "$3" "$0.logs"; else cat "$0.logs"; fi
  ;;
*) exit 1 ;;
esac
"##,
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let identity = Identity {
        instance_id: "instance".into(),
        workload_id: "workload".into(),
        revision_id: "revision".into(),
        execution_id: "execution".into(),
        launch_nonce: uuid::Uuid::new_v4().to_string(),
    };
    let handle = external::docker_handle(&identity);
    let labels = serde_json::to_value(&handle).unwrap()["labels"].clone();
    fs::write(binary.with_extension("inspect"), serde_json::to_vec(&serde_json::json!([
        {"Config": {"Labels": labels}, "State": {"Status": "exited", "Running": false, "ExitCode": 0}}
    ])).unwrap()).unwrap();
    let expected = (1..=301)
        .map(|line| format!("line{line}\n"))
        .collect::<String>();
    fs::write(binary.with_extension("logs"), &expected).unwrap();
    let policy = ExecutorPolicy {
        docker_enabled: true,
        docker_binary: binary.to_str().unwrap().into(),
        ..ExecutorPolicy::default()
    };
    let (captured, truncated) = external::logs(&policy, &handle, 65_536, &[]).unwrap();
    assert_eq!(captured, expected.as_bytes());
    assert!(!truncated);

    let secret = b"sensitive-".repeat(700);
    fs::write(binary.with_extension("logs"), &secret).unwrap();
    let (captured, truncated) =
        external::logs(&policy, &handle, 4096, std::slice::from_ref(&secret)).unwrap();
    assert_eq!(captured, b"[REDACTED]");
    assert!(!truncated);

    let mut crossing = vec![b'x'; 65_530];
    crossing.extend_from_slice(&secret);
    fs::write(binary.with_extension("logs"), crossing).unwrap();
    assert!(external::logs(&policy, &handle, 65_536, &[secret]).is_err());
}

fn fake_docker() -> (
    tempfile::TempDir,
    ExecutorPolicy,
    cronharbor::external::ExternalHandle,
) {
    use std::{fs, os::unix::fs::PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("docker");
    fs::write(
        &binary,
        r##"#!/bin/sh
shift 2
printf '%s\n' "$1" >> "$0.calls"
case "$1" in
inspect)
  if [ -f "$0.inspect-failed" ]; then exit 1; fi
  cat "$0.inspect"
  if [ -f "$0.inspect-stderr" ]; then cat "$0.inspect-stderr" >&2; fi
  ;;
logs)
  cat "$0.logs"
  if [ -f "$0.stderr" ]; then cat "$0.stderr" >&2; fi
  ;;
stop|rm) exit 0 ;;
info) cat "$0.info" ;;
container)
  printf '%s\n' "$@" > "$0.list-args"
  if [ -f "$0.list-failed" ]; then exit 1; fi
  cat "$0.list"
  ;;
*) exit 1 ;;
esac
"##,
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let handle = external::docker_handle(&Identity {
        instance_id: "instance".into(),
        workload_id: "workload".into(),
        revision_id: "revision".into(),
        execution_id: "execution".into(),
        launch_nonce: uuid::Uuid::new_v4().to_string(),
    });
    let policy = ExecutorPolicy {
        docker_enabled: true,
        docker_binary: binary.to_str().unwrap().into(),
        ..ExecutorPolicy::default()
    };
    (directory, policy, handle)
}

fn docker_inspection(handle: &cronharbor::external::ExternalHandle) -> serde_json::Value {
    serde_json::json!({
        "Config": {"Labels": serde_json::to_value(handle).unwrap()["labels"]},
        "State": {"Status": "exited", "Running": false, "ExitCode": 0}
    })
}

#[test]
fn mismatched_docker_ownership_never_signals_reads_logs_or_removes() {
    use std::fs;
    let (directory, policy, handle) = fake_docker();
    let mut object = docker_inspection(&handle);
    for key in ["instance", "workload", "revision", "execution", "launch"] {
        object["Config"]["Labels"][format!("dev.cronharbor.{key}")] = "other".into();
        fs::write(
            directory.path().join("docker.inspect"),
            serde_json::to_vec(&vec![&object]).unwrap(),
        )
        .unwrap();
        assert_eq!(
            external::stop(&policy, &handle, 1).unwrap_err().code,
            "OWNERSHIP_MISMATCH"
        );
        assert_eq!(
            external::logs(&policy, &handle, 4096, &[])
                .unwrap_err()
                .code,
            "OWNERSHIP_MISMATCH"
        );
        assert_eq!(
            external::remove_finished(&policy, &handle)
                .unwrap_err()
                .code,
            "OWNERSHIP_MISMATCH"
        );
        object = docker_inspection(&handle);
    }
    let calls = fs::read_to_string(directory.path().join("docker.calls")).unwrap();
    assert_eq!(calls.lines().collect::<Vec<_>>(), vec!["inspect"; 15]);
}

#[test]
fn truncated_docker_inspection_cannot_authorize_removal() {
    use std::fs;
    let (directory, policy, handle) = fake_docker();
    let object = docker_inspection(&handle);
    let mut output = serde_json::to_vec(&vec![object]).unwrap();
    output.resize(65_537, b' ');
    fs::write(directory.path().join("docker.inspect"), output).unwrap();
    let result = external::remove_finished(&policy, &handle);
    let calls = fs::read_to_string(directory.path().join("docker.calls")).unwrap();
    assert_eq!(
        calls, "inspect\n",
        "incomplete inspection authorized removal"
    );
    assert_eq!(result.unwrap_err().code, "EXECUTOR_UNAVAILABLE");
}

#[test]
fn truncated_docker_stderr_cannot_authorize_removal() {
    use std::fs;
    let (directory, policy, handle) = fake_docker();
    fs::write(
        directory.path().join("docker.inspect"),
        serde_json::to_vec(&vec![docker_inspection(&handle)]).unwrap(),
    )
    .unwrap();
    fs::write(
        directory.path().join("docker.inspect-stderr"),
        vec![b'x'; 65_537],
    )
    .unwrap();
    let result = external::remove_finished(&policy, &handle);
    let calls = fs::read_to_string(directory.path().join("docker.calls")).unwrap();
    assert_eq!(
        calls, "inspect\n",
        "incomplete inspection authorized removal"
    );
    assert_eq!(result.unwrap_err().code, "EXECUTOR_UNAVAILABLE");
}

#[test]
fn multiple_docker_inspection_objects_cannot_authorize_removal() {
    use std::fs;
    let (directory, policy, handle) = fake_docker();
    fs::write(
        directory.path().join("docker.inspect"),
        serde_json::to_vec(&vec![
            docker_inspection(&handle),
            docker_inspection(&handle),
        ])
        .unwrap(),
    )
    .unwrap();
    let result = external::remove_finished(&policy, &handle);
    let calls = fs::read_to_string(directory.path().join("docker.calls")).unwrap();
    assert_eq!(
        calls, "inspect\n",
        "ambiguous inspection authorized removal"
    );
    assert_eq!(result.unwrap_err().code, "EXECUTOR_UNAVAILABLE");
}

#[test]
fn executor_policy_rejects_shared_or_linked_files() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("executors.json");
    fs::write(&path, "{}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(external::load_policy(directory.path()).is_ok());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(external::load_policy(directory.path()).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&path, directory.path().join("alias")).unwrap();
    assert!(external::load_policy(directory.path()).is_err());
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(directory.path().join("alias"), &path).unwrap();
    assert!(external::load_policy(directory.path()).is_err());
}

#[test]
fn helper_drains_both_output_streams_and_reports_their_capture_limit() {
    use std::fs;
    let (directory, policy, handle) = fake_docker();
    fs::write(
        directory.path().join("docker.inspect"),
        serde_json::to_vec(&vec![docker_inspection(&handle)]).unwrap(),
    )
    .unwrap();
    fs::write(directory.path().join("docker.logs"), vec![b'o'; 70_000]).unwrap();
    fs::write(directory.path().join("docker.stderr"), vec![b'e'; 70_000]).unwrap();
    let (captured, truncated) = external::logs(&policy, &handle, 200_000, &[]).unwrap();
    assert_eq!(captured.len(), 131_072);
    assert_eq!(&captured[..65_536], vec![b'o'; 65_536]);
    assert_eq!(&captured[65_536..], vec![b'e'; 65_536]);
    assert!(truncated);
}

#[test]
fn truncated_systemd_inspection_cannot_authorize_a_stop() {
    use cronharbor::external::ExternalHandle;
    use std::{fs, os::unix::fs::PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("systemctl");
    fs::write(
        &binary,
        r##"#!/bin/sh
printf '%s\n' "$1" >> "$0.calls"
case "$1" in
show) cat "$0.show"; cat "$0.stderr" >&2 ;;
stop) exit 0 ;;
*) exit 1 ;;
esac
"##,
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let policy = ExecutorPolicy {
        systemd_enabled: true,
        systemctl_binary: binary.to_str().unwrap().into(),
        units: vec!["worker.service".into()],
        ..ExecutorPolicy::default()
    };
    let handle = ExternalHandle::Systemd {
        unit: "worker.service".into(),
        since_us: Some(1),
    };
    for truncate_stdout in [true, false] {
        let mut output = b"LoadState=loaded\nActiveState=active\nExecMainStatus=0\n".to_vec();
        let stderr = if truncate_stdout {
            output.resize(65_537, b' ');
            Vec::new()
        } else {
            vec![b'e'; 65_537]
        };
        fs::write(binary.with_extension("show"), output).unwrap();
        fs::write(binary.with_extension("stderr"), stderr).unwrap();
        let result = external::stop(&policy, &handle, 1);
        let calls = fs::read_to_string(binary.with_extension("calls")).unwrap();
        assert_eq!(calls, "show\n", "incomplete inspection authorized a stop");
        assert_eq!(result.unwrap_err().code, "EXECUTOR_UNAVAILABLE");
        fs::remove_file(binary.with_extension("calls")).unwrap();
    }
}

#[test]
fn failed_container_listing_cannot_prove_a_container_is_absent() {
    use cronharbor::external::ExternalState;
    use std::fs;
    let (directory, policy, handle) = fake_docker();
    fs::write(directory.path().join("docker.inspect-failed"), "").unwrap();
    fs::write(directory.path().join("docker.list-failed"), "").unwrap();
    assert_eq!(
        external::inspect(&policy, &handle).unwrap().state,
        ExternalState::Unknown
    );
    let calls = fs::read_to_string(directory.path().join("docker.calls")).unwrap();
    assert_eq!(calls, "inspect\ncontainer\n");
}

#[test]
fn a_failed_inspection_of_an_existing_container_cannot_confirm_stop() {
    use std::fs;
    let (directory, policy, handle) = fake_docker();
    let cronharbor::external::ExternalHandle::Docker { id, .. } = &handle else {
        panic!("expected a Docker handle");
    };
    fs::write(directory.path().join("docker.inspect-failed"), "").unwrap();
    fs::write(directory.path().join("docker.info"), "healthy").unwrap();
    for listing in [format!("{id}\n"), format!("other,{id}\n")] {
        fs::write(directory.path().join("docker.list"), listing).unwrap();
        assert!(!external::stop(&policy, &handle, 1).unwrap());
        let calls = fs::read_to_string(directory.path().join("docker.calls")).unwrap();
        assert_eq!(calls, "inspect\ncontainer\n");
        fs::remove_file(directory.path().join("docker.calls")).unwrap();
    }
    assert_eq!(
        fs::read_to_string(directory.path().join("docker.list-args")).unwrap(),
        format!("container\nls\n--all\n--filter\nname={id}\n--format\n{{{{.Names}}}}\n")
    );
}

#[test]
fn a_complete_container_listing_proves_only_exact_name_absence() {
    use cronharbor::external::ExternalState;
    use std::fs;
    let (directory, policy, handle) = fake_docker();
    let cronharbor::external::ExternalHandle::Docker { id, .. } = &handle else {
        panic!("expected a Docker handle");
    };
    fs::write(directory.path().join("docker.inspect-failed"), "").unwrap();
    for list in [String::new(), format!("{id}-other\n")] {
        fs::write(directory.path().join("docker.list"), list).unwrap();
        assert_eq!(
            external::inspect(&policy, &handle).unwrap().state,
            ExternalState::Absent
        );
    }
}

#[test]
fn incomplete_or_non_utf8_container_listings_preserve_uncertainty() {
    use cronharbor::external::ExternalState;
    use std::fs;
    let (directory, policy, handle) = fake_docker();
    fs::write(directory.path().join("docker.inspect-failed"), "").unwrap();
    for list in [vec![b'x'; 65_537], vec![0xff]] {
        fs::write(directory.path().join("docker.list"), list).unwrap();
        assert_eq!(
            external::inspect(&policy, &handle).unwrap().state,
            ExternalState::Unknown
        );
    }
}

#[test]
fn docker_logs_require_a_current_verified_owned_inspection() {
    use std::fs;
    let (directory, policy, handle) = fake_docker();
    fs::write(
        directory.path().join("docker.inspect"),
        serde_json::to_vec(&vec![docker_inspection(&handle)]).unwrap(),
    )
    .unwrap();
    assert_eq!(
        external::inspect(&policy, &handle).unwrap().state,
        external::ExternalState::Exited
    );
    fs::write(directory.path().join("docker.inspect-failed"), "").unwrap();
    fs::write(directory.path().join("docker.logs"), "foreign output").unwrap();
    let external::ExternalHandle::Docker { id, .. } = &handle else {
        unreachable!()
    };
    for listing in [format!("{id}\n"), String::new()] {
        fs::write(directory.path().join("docker.list"), listing).unwrap();
        let result = external::logs(&policy, &handle, 4096, &[]);
        let calls = fs::read_to_string(directory.path().join("docker.calls")).unwrap();
        assert!(
            !calls.lines().any(|call| call == "logs"),
            "logs invoked without ownership evidence"
        );
        assert!(result.is_err());
    }
}

#[test]
fn docker_stop_honors_the_full_validated_grace_period() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let (directory, policy, handle) = fake_docker();
    let binary = directory.path().join("docker");
    fs::write(
        &binary,
        r##"#!/bin/sh
shift 2
case "$1" in
inspect) cat "$0.inspect" ;;
stop)
  printf '%s\n' "$@" > "$0.stop-args"
  cat "$0.exited" > "$0.inspect"
  ;;
*) exit 1 ;;
esac
"##,
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let exited = docker_inspection(&handle);
    fs::write(
        binary.with_extension("exited"),
        serde_json::to_vec(&vec![&exited]).unwrap(),
    )
    .unwrap();
    for grace in [0, 120, 300] {
        let mut running = exited.clone();
        running["State"]["Running"] = true.into();
        running["State"]["Status"] = "running".into();
        fs::write(
            binary.with_extension("inspect"),
            serde_json::to_vec(&vec![running]).unwrap(),
        )
        .unwrap();
        assert!(external::stop(&policy, &handle, grace).unwrap());
        assert!(
            fs::read_to_string(binary.with_extension("stop-args"))
                .unwrap()
                .contains(&format!("--time\n{grace}\n"))
        );
    }
}

#[test]
fn systemd_terminal_observations_require_complete_valid_result_evidence() {
    use external::{ExternalHandle, TerminalOutcome};
    use std::{fs, os::unix::fs::PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("systemctl");
    fs::write(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.calls\"\ncat \"$0.show\"\n",
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let policy = ExecutorPolicy {
        systemd_enabled: true,
        systemctl_binary: binary.to_string_lossy().into_owned(),
        units: vec!["worker.service".into()],
        ..ExecutorPolicy::default()
    };
    let handle = ExternalHandle::Systemd {
        unit: "worker.service".into(),
        since_us: Some(1),
    };
    for (active, result, code, status, outcome) in [
        ("failed", "timeout", "1", "0", Some(TerminalOutcome::Failed)),
        (
            "failed",
            "watchdog",
            "0",
            "0",
            Some(TerminalOutcome::Failed),
        ),
        (
            "inactive",
            "success",
            "1",
            "75",
            Some(TerminalOutcome::Succeeded),
        ),
        (
            "inactive",
            "success",
            "2",
            "15",
            Some(TerminalOutcome::Succeeded),
        ),
        ("inactive", "", "1", "0", None),
        ("inactive", "success", "", "0", None),
        ("inactive", "success", "1", "invalid", None),
        ("inactive", "success", "1", "256", None),
        ("inactive", "success", "2", "0", None),
        ("inactive", "success", "2", "255", None),
        ("inactive", "success", "0", "0", None),
        ("failed", "success", "1", "0", None),
        ("inactive", "unrecognized", "1", "0", None),
    ] {
        let properties = format!(
            "LoadState=loaded\nActiveState={active}\nResult={result}\nExecMainCode={code}\nExecMainStatus={status}\n"
        );
        fs::write(binary.with_extension("show"), &properties).unwrap();
        assert_eq!(
            external::inspect(&policy, &handle).unwrap().outcome,
            outcome,
            "{properties}"
        );
        if result.is_empty() || code.is_empty() {
            let missing = properties
                .lines()
                .filter(|line| !line.ends_with('='))
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(binary.with_extension("show"), missing).unwrap();
            assert_eq!(external::inspect(&policy, &handle).unwrap().outcome, None);
        }
    }
    let calls = fs::read_to_string(binary.with_extension("calls")).unwrap();
    assert!(calls.contains("Result,ExecMainCode,ExecMainStatus"));
}
