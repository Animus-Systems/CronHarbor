use cronharbor::{
    application,
    config::{Manifest, SecretRef},
    contracts::AppCommand,
    external::{DockerSpec, ExecutorPolicy, ExternalHandle},
    server::{self, Request},
    store::Store,
};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
fn recovered_logs(original_context: bool, lost_log_inspection: bool) {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let state = root.path().join("state");
    let secret = root.path().join("secret");
    fs::write(&secret, "old-reference-secret").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.kind = "OneShotJob".into();
    manifest.spec.timeout_seconds = 300;
    manifest.spec.execution.adapter = "docker".into();
    let image = format!("fixture@sha256:{}", "a".repeat(64));
    manifest.spec.execution.docker = Some(DockerSpec {
        image: image.clone(),
        user: "10001".into(),
        network: "none".into(),
        memory_bytes: 8 * 1024 * 1024,
        pids_limit: 8,
    });
    manifest.spec.execution.secret_refs = BTreeMap::from([(
        "SECRET".into(),
        SecretRef::File {
            path: secret.to_string_lossy().into_owned(),
        },
    )]);
    let mut store = Store::open(&state).unwrap();
    store.apply(&manifest, cronharbor::now_us()).unwrap();
    application::execute(
        &mut store,
        AppCommand::Run {
            id: "local/hello".into(),
            idempotency_key: "one-lost-receipt".into(),
        },
        None,
    )
    .unwrap();
    let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    let identity = store.external_identity(&launch).unwrap();
    let handle = cronharbor::external::docker_handle(&identity);
    store
        .persist_external_intent(&launch.attempt_id, &handle)
        .unwrap();
    store
        .start_requested(&launch.attempt_id, cronharbor::now_us())
        .unwrap();
    let ExternalHandle::Docker { labels, .. } = &handle else {
        unreachable!()
    };
    let container = root.path().join("container.json");
    fs::write(&container,serde_json::json!([{"Config":{"Labels":labels,"Env":if original_context{vec!["SECRET=old-reference-secret"]}else{vec![]}},"State":{"Running":false,"Status":"exited","ExitCode":0}}]).to_string()).unwrap();
    let removed = root.path().join("removed");
    let helper = root.path().join("docker-fixture");
    let ExternalHandle::Docker { id, .. } = &handle else {
        unreachable!()
    };
    let logged = root.path().join("logs-invoked");
    fs::write(
        &helper,
        format!(
            r##"#!/bin/sh
case "$3" in
inspect)
  count=0
  if [ -f "$0.count" ]; then count=$(cat "$0.count"); fi
  count=$((count + 1))
  printf '%s' "$count" > "$0.count"
  if [ '{lost_log_inspection}' = true ] && [ "$count" -ge 3 ]; then exit 1; fi
  cat '{}'
  ;;
container) printf '%s\n' '{id}' ;;
logs) : > '{}'; printf 'old-reference-secret\n' ;;
rm) : > '{}' ;;
info) printf fixture ;;
*) exit 1 ;;
esac
"##,
            container.display(),
            logged.display(),
            removed.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let policy = ExecutorPolicy {
        docker_enabled: true,
        docker_binary: helper.to_string_lossy().into_owned(),
        images: vec![image],
        ..ExecutorPolicy::default()
    };
    let policy_path = state.join("executors.json");
    fs::write(&policy_path, serde_json::to_vec(&policy).unwrap()).unwrap();
    fs::set_permissions(&policy_path, fs::Permissions::from_mode(0o600)).unwrap();
    drop(store);
    fs::write(&secret, "new-reference-secret").unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = shutdown.clone();
    let directory = state.clone();
    let daemon = thread::spawn(move || server::run_with_shutdown(&directory, &[], false, flag));
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut finished = false;
    while Instant::now() < deadline {
        let command = Request::Execute {
            command: Box::new(AppCommand::Execution {
                id: launch.execution_id.clone(),
            }),
            token: None,
        };
        if server::client(&state, command).is_ok_and(|value| value["state"] == "SUCCEEDED") {
            finished = true;
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let result = server::client(
        &state,
        Request::Execute {
            command: Box::new(AppCommand::Logs {
                attempt_id: launch.attempt_id,
                offset: 0,
                max_bytes: 65536,
            }),
            token: None,
        },
    );
    thread::sleep(Duration::from_millis(100));
    shutdown.store(true, Ordering::Relaxed);
    daemon.join().unwrap().unwrap();
    assert!(finished, "recovered external execution did not finish");
    let logs = result.unwrap();
    assert!(
        !logs["text"]
            .as_str()
            .unwrap()
            .contains("old-reference-secret"),
        "original secret leaked after provider rotation"
    );
    if original_context && !lost_log_inspection {
        assert_eq!(logs["text"], "[REDACTED]\n");
    } else {
        assert_eq!(logs["text"], "");
        if lost_log_inspection {
            assert!(!logged.exists(), "unverified Docker logs were requested");
        }
        assert_eq!(logs["log_truncated"], true);
        assert!(
            !removed.exists(),
            "container was removed without reconstructable redaction context"
        );
    }
}
#[test]
fn recovered_docker_logs_use_original_secret_values_after_rotation() {
    recovered_logs(true, false)
}
#[test]
fn recovered_docker_logs_are_withheld_when_original_secret_names_are_missing() {
    recovered_logs(false, false)
}

#[test]
fn systemd_result_controls_recovered_terminal_outcome() {
    use cronharbor::external::SystemdSpec;
    for (active, result, code, status, expected, expected_error) in [
        (
            "failed",
            "timeout",
            "1",
            "0",
            "FAILED",
            Some("EXTERNAL_EXECUTION_FAILED"),
        ),
        (
            "failed",
            "watchdog",
            "0",
            "0",
            "FAILED",
            Some("EXTERNAL_EXECUTION_FAILED"),
        ),
        ("inactive", "success", "1", "75", "SUCCEEDED", None),
        ("inactive", "success", "2", "15", "SUCCEEDED", None),
        (
            "failed",
            "exit-code",
            "1",
            "7",
            "FAILED",
            Some("EXIT_NONZERO"),
        ),
        (
            "inactive",
            "",
            "1",
            "0",
            "UNKNOWN",
            Some("RECONCILIATION_REQUIRED"),
        ),
    ] {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let state = root.path().join("state");
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.kind = "OneShotJob".into();
        manifest.spec.timeout_seconds = 300;
        manifest.spec.execution.adapter = "systemd".into();
        manifest.spec.execution.command.clear();
        manifest.spec.execution.args.clear();
        manifest.spec.execution.env.clear();
        manifest.spec.execution.working_directory = "/tmp".into();
        manifest.spec.execution.systemd = Some(SystemdSpec {
            unit: "worker.service".into(),
        });
        let mut store = Store::open(&state).unwrap();
        store
            .set_log_storage_policy(
                &cronharbor::contracts::LogStoragePolicyInput {
                    max_disk_bytes: 268435456,
                    max_segments: 1024,
                    retention_seconds: 1,
                },
                1,
            )
            .unwrap();
        store.apply(&manifest, cronharbor::now_us()).unwrap();
        application::execute(
            &mut store,
            AppCommand::Run {
                id: "local/hello".into(),
                idempotency_key: "recovered-unit".into(),
            },
            None,
        )
        .unwrap();
        let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
        let handle = ExternalHandle::Systemd {
            unit: "worker.service".into(),
            since_us: Some(1),
        };
        store
            .persist_external_intent(&launch.attempt_id, &handle)
            .unwrap();
        store
            .start_requested(&launch.attempt_id, cronharbor::now_us())
            .unwrap();
        let helper = root.path().join("systemctl");
        fs::write(&helper, format!(r##"#!/bin/sh
case "$1" in
show) printf 'LoadState=loaded\nActiveState={active}\nResult={result}\nExecMainCode={code}\nExecMainStatus={status}\n' ;;
--no-pager) printf 'safe unit evidence\n' ;;
*) exit 1 ;;
esac
"##)).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = ExecutorPolicy {
            systemd_enabled: true,
            systemctl_binary: helper.to_string_lossy().into_owned(),
            journalctl_binary: helper.to_string_lossy().into_owned(),
            units: vec!["worker.service".into()],
            ..ExecutorPolicy::default()
        };
        let policy_path = state.join("executors.json");
        fs::write(&policy_path, serde_json::to_vec(&policy).unwrap()).unwrap();
        fs::set_permissions(&policy_path, fs::Permissions::from_mode(0o600)).unwrap();
        drop(store);
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let directory = state.clone();
        let daemon = thread::spawn(move || server::run_with_shutdown(&directory, &[], false, flag));
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut final_state = None;
        let mut final_error = None;
        while Instant::now() < deadline {
            if let Ok(value) = server::client(
                &state,
                Request::Execute {
                    command: Box::new(AppCommand::Execution {
                        id: launch.execution_id.clone(),
                    }),
                    token: None,
                },
            ) && (value["finished_at_us"].is_i64()
                || (expected == "UNKNOWN" && value["exit_code"] == 0))
            {
                final_state = value["state"].as_str().map(str::to_owned);
                final_error = value["error_code"].as_str().map(str::to_owned);
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        shutdown.store(true, Ordering::Relaxed);
        daemon.join().unwrap().unwrap();
        assert_eq!(
            final_state.as_deref(),
            Some(expected),
            "{active} Result={result} code={code} status={status}"
        );
        assert_eq!(final_error.as_deref(), expected_error);
        let mut store = Store::open(&state).unwrap();
        let path = store.log_path(&launch.attempt_id).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "safe unit evidence\n");
        if expected == "UNKNOWN" {
            let database = rusqlite::Connection::open(state.join("state.db")).unwrap();
            let bytes: i64 = database
                .query_row(
                    "SELECT bytes_written FROM log_segments WHERE stream_id=(SELECT id FROM log_streams WHERE attempt_id=?1)",
                    [&launch.attempt_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(bytes, 19);
            database
                .execute(
                    "UPDATE log_segments SET created_at_us=1 WHERE stream_id=(SELECT id FROM log_streams WHERE attempt_id=?1)",
                    [&launch.attempt_id],
                )
                .unwrap();
            store.retain_logs().unwrap();
            assert!(path.exists());
            application::execute(
                &mut store,
                AppCommand::Reconcile {
                    execution_id: launch.execution_id,
                    state: "CANCELLED".into(),
                    evidence: "verified unit is stopped".into(),
                    idempotency_key: "reconcile-unit".into(),
                },
                None,
            )
            .unwrap();
            store.retain_logs().unwrap();
            assert!(!path.exists());
        }
    }
}

#[test]
fn recovered_docker_logs_are_withheld_when_log_time_ownership_cannot_be_verified() {
    recovered_logs(true, true)
}

fn shutdown_external_stop(observation_inflight: bool) {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let state = root.path().join("state");
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.kind = "OneShotJob".into();
    manifest.spec.timeout_seconds = 300;
    manifest.spec.execution.adapter = "docker".into();
    manifest.spec.service.stop_grace_seconds = 120;
    let image = format!("fixture@sha256:{}", "a".repeat(64));
    manifest.spec.execution.docker = Some(DockerSpec {
        image: image.clone(),
        user: "10001".into(),
        network: "none".into(),
        memory_bytes: 8 * 1024 * 1024,
        pids_limit: 8,
    });
    let mut store = Store::open(&state).unwrap();
    store.apply(&manifest, cronharbor::now_us()).unwrap();
    application::execute(
        &mut store,
        AppCommand::Run {
            id: "local/hello".into(),
            idempotency_key: "stop-before-shutdown".into(),
        },
        None,
    )
    .unwrap();
    let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    let handle = cronharbor::external::docker_handle(&store.external_identity(&launch).unwrap());
    store
        .persist_external_intent(&launch.attempt_id, &handle)
        .unwrap();
    store
        .start_requested(&launch.attempt_id, cronharbor::now_us())
        .unwrap();
    let ExternalHandle::Docker { labels, .. } = &handle else {
        unreachable!()
    };
    let container = root.path().join("container.json");
    let exited = root.path().join("exited.json");
    let mut object = serde_json::json!([{"Config":{"Labels":labels},"State":{"Running":true,"Status":"running","ExitCode":0}}]);
    fs::write(&container, object.to_string()).unwrap();
    object[0]["State"]["Running"] = false.into();
    object[0]["State"]["Status"] = "exited".into();
    fs::write(&exited, object.to_string()).unwrap();
    let helper = root.path().join("docker-fixture");
    fs::write(
        &helper,
        format!(
            r##"#!/bin/sh
case "$3" in
inspect)
  count=0
  if [ -f "$0.count" ]; then count=$(cat "$0.count"); fi
  count=$((count + 1))
  printf '%s' "$count" > "$0.count"
  if [ '{observation_inflight}' = true ] && [ "$count" -eq 2 ]; then
    : > "$0.observing"
    sleep 0.3
  fi
  cat '{}'
  ;;
stop)
  printf '%s\n' "$@" > "$0.stop-args"
  : > "$0.stopping"
  sleep 0.3
  cp '{}' '{}'
  : > "$0.stopped"
  ;;
logs) printf 'finished\n' ;;
rm) exit 0 ;;
*) exit 1 ;;
esac
"##,
            container.display(),
            exited.display(),
            container.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let policy = ExecutorPolicy {
        docker_enabled: true,
        docker_binary: helper.to_string_lossy().into_owned(),
        images: vec![image],
        ..ExecutorPolicy::default()
    };
    let policy_path = state.join("executors.json");
    fs::write(&policy_path, serde_json::to_vec(&policy).unwrap()).unwrap();
    fs::set_permissions(&policy_path, fs::Permissions::from_mode(0o600)).unwrap();
    drop(store);
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&shutdown);
    let directory = state.clone();
    let daemon = thread::spawn(move || server::run_with_shutdown(&directory, &[], false, flag));
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if let Ok(value) = server::client(
            &state,
            Request::Execute {
                command: Box::new(AppCommand::Execution {
                    id: launch.execution_id.clone(),
                }),
                token: None,
            },
        ) && value["state"] == "RUNNING"
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "external identity was not recovered"
        );
        thread::sleep(Duration::from_millis(10));
    }
    if observation_inflight {
        while !helper.with_extension("observing").exists() {
            assert!(Instant::now() < deadline, "observation did not start");
            thread::sleep(Duration::from_millis(10));
        }
    }
    server::client(
        &state,
        Request::Execute {
            command: Box::new(AppCommand::Cancel {
                execution_id: launch.execution_id.clone(),
                idempotency_key: "stop-test".into(),
            }),
            token: None,
        },
    )
    .unwrap();
    if !observation_inflight {
        while !helper.with_extension("stopping").exists() {
            assert!(Instant::now() < deadline, "external stop did not start");
            thread::sleep(Duration::from_millis(10));
        }
    }
    shutdown.store(true, Ordering::Relaxed);
    daemon.join().unwrap().unwrap();
    let drained = helper.with_extension("stopped").exists();
    while !helper.with_extension("stopped").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(drained, "shutdown abandoned the in-flight stop helper");
    let store = Store::open(&state).unwrap();
    assert_eq!(store.history().unwrap()[0].state, "CANCELLED");
    assert!(
        fs::read_to_string(helper.with_extension("stop-args"))
            .unwrap()
            .contains("--time\n120\n")
    );
}

#[test]
fn shutdown_waits_for_an_inflight_external_stop_and_persists_its_result() {
    shutdown_external_stop(false)
}

#[test]
fn shutdown_honors_durable_cancel_while_external_observation_is_inflight() {
    shutdown_external_stop(true)
}
