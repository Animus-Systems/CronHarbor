use cronharbor::{
    config::Manifest,
    contracts::AppCommand,
    server::{self, Request},
};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
struct Daemon {
    directory: tempfile::TempDir,
    shutdown: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<cronharbor::Result<()>>>,
}
impl Daemon {
    fn new(manifest: Manifest) -> Self {
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().to_owned();
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = shutdown.clone();
        let thread =
            thread::spawn(move || server::run_with_shutdown(&path, &[manifest], false, flag));
        let daemon = Self {
            directory,
            shutdown,
            thread: Some(thread),
        };
        wait(|| server::client(daemon.directory.path(), Request::Status).is_ok());
        daemon
    }
    fn call(&self, command: AppCommand) -> serde_json::Value {
        server::client(
            self.directory.path(),
            Request::Execute {
                command: Box::new(command),
                token: None,
            },
        )
        .unwrap()
    }
    fn path(&self) -> &Path {
        self.directory.path()
    }
    fn restart(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap().unwrap();
        self.shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&self.shutdown);
        let path = self.path().to_owned();
        self.thread = Some(thread::spawn(move || {
            server::run_with_shutdown(&path, &[], false, flag)
        }));
        wait(|| server::client(self.path(), Request::Status).is_ok());
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap().unwrap();
    }
}
#[track_caller]
fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !predicate() {
        assert!(Instant::now() < deadline, "runtime state did not converge");
        thread::sleep(Duration::from_millis(25));
    }
}
fn job() -> Manifest {
    Manifest::parse(include_str!("../examples/hello.yaml")).unwrap()
}
#[test]
fn native_services_resume_after_owner_shutdown_without_spending_restart_budget() {
    for policy in ["on-failure", "never"] {
        let mut manifest = job();
        manifest.kind = "Service".into();
        manifest.spec.service.restart = policy.into();
        manifest.spec.service.max_restarts = 0;
        manifest.spec.execution.command = "/bin/sleep".into();
        manifest.spec.execution.args = vec!["30".into()];
        let mut daemon = Daemon::new(manifest);
        wait(|| {
            daemon.call(AppCommand::Workload {
                id: "local/hello".into(),
            })["observed_state"]
                == "READY"
        });
        daemon.restart();
        wait(|| {
            daemon.call(AppCommand::Workload {
                id: "local/hello".into(),
            })["observed_state"]
                == "READY"
        });
        let workload = daemon.call(AppCommand::Workload {
            id: "local/hello".into(),
        });
        assert_eq!(workload["restart_count"], 0);
        let history = daemon.call(AppCommand::Executions {
            workload_id: None,
            cursor: None,
            limit: 10,
        });
        let items = history["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .any(|row| row["error_code"] == "DAEMON_SHUTDOWN")
        );
        daemon.call(AppCommand::ServiceDesired {
            id: "local/hello".into(),
            desired_state: "STOPPED".into(),
            idempotency_key: "stop".into(),
            expected_revision: None,
        });
        wait(|| {
            daemon.call(AppCommand::Workload {
                id: "local/hello".into(),
            })["observed_state"]
                == "ABSENT"
        });
        daemon.restart();
        assert_eq!(
            daemon.call(AppCommand::Workload {
                id: "local/hello".into()
            })["desired_state"],
            "STOPPED"
        );
        assert_eq!(daemon.call(AppCommand::Status)["active"], 0);
    }
}
#[test]
fn manual_operation_waits_for_real_exit_and_cancel_is_durable() {
    let mut m = job();
    m.kind = "OneShotJob".into();
    m.spec.execution.command = "/bin/sleep".into();
    m.spec.execution.args = vec!["30".into()];
    let d = Daemon::new(m);
    let operation = d.call(AppCommand::Run {
        id: "local/hello".into(),
        idempotency_key: "manual".into(),
    });
    assert_eq!(operation["state"], "ACCEPTED");
    wait(|| {
        d.call(AppCommand::Executions {
            workload_id: None,
            cursor: None,
            limit: 10,
        })["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|row| row["state"] == "RUNNING"))
    });
    let row = d.call(AppCommand::Executions {
        workload_id: None,
        cursor: None,
        limit: 10,
    })["items"][0]
        .clone();
    let cancel = d.call(AppCommand::Cancel {
        execution_id: row["execution_id"].as_str().unwrap().into(),
        idempotency_key: "cancel".into(),
    });
    assert_eq!(cancel["state"], "ACCEPTED");
    wait(|| {
        d.call(AppCommand::Operation {
            id: cancel["id"].as_str().unwrap().into(),
        })["state"]
            == "SUCCEEDED"
    });
    assert_eq!(
        d.call(AppCommand::Execution {
            id: row["execution_id"].as_str().unwrap().into()
        })["state"],
        "CANCELLED"
    );
    assert!(d.path().join("state.db").exists());
}
#[test]
fn service_readiness_stop_and_restart_budget_are_real() {
    let mut m = job();
    m.kind = "Service".into();
    m.spec.execution.command = "/usr/bin/false".into();
    m.spec.execution.args.clear();
    m.spec.service.max_restarts = 1;
    m.spec.service.backoff_seconds = 1;
    let d = Daemon::new(m);
    wait(|| {
        d.call(AppCommand::Workload {
            id: "local/hello".into(),
        })["observed_state"]
            == "FAILED"
    });
    let executions = d.call(AppCommand::Executions {
        workload_id: None,
        cursor: None,
        limit: 10,
    });
    assert_eq!(executions["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        d.call(AppCommand::Workload {
            id: "local/hello".into()
        })["restart_count"],
        1
    );
}
#[test]
fn service_desired_stop_completes_only_after_process_termination() {
    let mut m = job();
    m.kind = "Service".into();
    m.spec.execution.command = "/bin/sleep".into();
    m.spec.execution.args = vec!["30".into()];
    let d = Daemon::new(m);
    wait(|| {
        d.call(AppCommand::Workload {
            id: "local/hello".into(),
        })["observed_state"]
            == "READY"
    });
    let op = d.call(AppCommand::ServiceDesired {
        id: "local/hello".into(),
        desired_state: "STOPPED".into(),
        idempotency_key: "stop".into(),
        expected_revision: None,
    });
    wait(|| {
        d.call(AppCommand::Operation {
            id: op["id"].as_str().unwrap().into(),
        })["state"]
            == "SUCCEEDED"
    });
    assert_eq!(
        d.call(AppCommand::Workload {
            id: "local/hello".into()
        })["observed_state"],
        "ABSENT"
    );
    assert_eq!(d.call(AppCommand::Status)["active"], 0);
}
#[test]
fn service_never_policy_does_not_start_again_after_a_clean_exit() {
    let mut m = job();
    m.kind = "Service".into();
    m.spec.execution.command = "/usr/bin/true".into();
    m.spec.execution.args.clear();
    m.spec.service.restart = "never".into();
    let d = Daemon::new(m);
    wait(|| {
        d.call(AppCommand::Executions {
            workload_id: None,
            cursor: None,
            limit: 10,
        })["items"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["state"] == "SUCCEEDED"))
    });
    thread::sleep(Duration::from_millis(150));
    let rows = d.call(AppCommand::Executions {
        workload_id: None,
        cursor: None,
        limit: 100,
    });
    assert_eq!(rows["items"].as_array().unwrap().len(), 1);
}
#[test]
fn hung_liveness_probe_is_bounded_and_explicit_health_restart_stops_service() {
    let mut m = job();
    m.kind = "Service".into();
    m.spec.execution.command = "/bin/sleep".into();
    m.spec.execution.args = vec!["30".into()];
    m.spec.service.startup_grace_seconds = 0;
    m.spec.service.restart_on_unhealthy = true;
    m.spec.service.max_restarts = 0;
    m.spec.service.liveness = Some(cronharbor::config::Probe {
        command: "/bin/sleep".into(),
        args: vec!["30".into()],
        interval_seconds: 1,
        timeout_seconds: 1,
        failure_threshold: 1,
        success_threshold: 1,
    });
    let d = Daemon::new(m);
    wait(|| {
        d.call(AppCommand::Workload {
            id: "local/hello".into(),
        })["observed_state"]
            == "FAILED"
    });
    let rows = d.call(AppCommand::Executions {
        workload_id: None,
        cursor: None,
        limit: 10,
    });
    assert_eq!(rows["items"][0]["error_code"], "HEALTH_FAILED");
    let detail = d.call(AppCommand::Workload {
        id: "local/hello".into(),
    });
    assert!(
        detail["probe_history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["kind"] == "liveness" && row["healthy"] == false)
    );
    assert_eq!(d.call(AppCommand::Status)["active"], 0);
}
#[test]
fn service_desired_running_noop_completes_and_stop_supersedes_unready_start() {
    let mut m = job();
    m.kind = "Service".into();
    m.spec.execution.command = "/bin/sleep".into();
    m.spec.execution.args = vec!["30".into()];
    let d = Daemon::new(m.clone());
    wait(|| {
        d.call(AppCommand::Workload {
            id: "local/hello".into(),
        })["observed_state"]
            == "READY"
    });
    let noop = d.call(AppCommand::ServiceDesired {
        id: "local/hello".into(),
        desired_state: "RUNNING".into(),
        idempotency_key: "noop".into(),
        expected_revision: None,
    });
    assert_eq!(noop["state"], "SUCCEEDED");
    drop(d);
    m.spec.service.desired_state = "STOPPED".into();
    m.spec.service.readiness = Some(cronharbor::config::Probe {
        command: "/usr/bin/false".into(),
        args: vec![],
        interval_seconds: 1,
        timeout_seconds: 1,
        failure_threshold: 1,
        success_threshold: 1,
    });
    let d = Daemon::new(m);
    let start = d.call(AppCommand::ServiceDesired {
        id: "local/hello".into(),
        desired_state: "RUNNING".into(),
        idempotency_key: "start".into(),
        expected_revision: None,
    });
    wait(|| d.call(AppCommand::Status)["active"] == 1);
    let stop = d.call(AppCommand::ServiceDesired {
        id: "local/hello".into(),
        desired_state: "STOPPED".into(),
        idempotency_key: "stop".into(),
        expected_revision: None,
    });
    let superseded = d.call(AppCommand::Operation {
        id: start["id"].as_str().unwrap().into(),
    });
    assert_eq!(superseded["state"], "FAILED");
    assert_eq!(superseded["error_code"], "OPERATION_SUPERSEDED");
    wait(|| {
        d.call(AppCommand::Operation {
            id: stop["id"].as_str().unwrap().into(),
        })["state"]
            == "SUCCEEDED"
    });
}
#[test]
fn manual_service_restart_works_when_automatic_restart_budget_is_zero() {
    let mut m = job();
    m.kind = "Service".into();
    m.spec.execution.command = "/bin/sleep".into();
    m.spec.execution.args = vec!["30".into()];
    m.spec.service.max_restarts = 0;
    let d = Daemon::new(m);
    wait(|| {
        d.call(AppCommand::Workload {
            id: "local/hello".into(),
        })["observed_state"]
            == "READY"
    });
    let op = d.call(AppCommand::ServiceRestart {
        id: "local/hello".into(),
        idempotency_key: "manual-zero-budget".into(),
        expected_revision: None,
    });
    wait(|| {
        d.call(AppCommand::Operation {
            id: op["id"].as_str().unwrap().into(),
        })["state"]
            == "SUCCEEDED"
    });
    assert_eq!(
        d.call(AppCommand::Executions {
            workload_id: None,
            cursor: None,
            limit: 10
        })["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn installation_policy_bounds_simultaneous_active_writes() {
    let mut a = job();
    a.kind = "OneShotJob".into();
    a.spec.execution.command = "/usr/bin/yes".into();
    a.spec.execution.args = vec!["x".into()];
    a.spec.logs.max_run_bytes = 8192;
    let mut b = a.clone();
    b.metadata.name = "smaller".into();
    let d = Daemon::new(a);
    d.call(AppCommand::SetLogStorage {
        policy: cronharbor::contracts::LogStoragePolicyInput {
            max_disk_bytes: 4096,
            max_segments: 1024,
            retention_seconds: 604800,
        },
        expected_revision: 1,
        idempotency_key: "log-budget".into(),
    });
    d.call(AppCommand::Apply {
        definition: serde_json::to_string(&b).unwrap(),
        source_kind: "FILE".into(),
        expected_revision: None,
        idempotency_key: "lower-before-start".into(),
    });
    for name in ["local/hello", "local/smaller"] {
        d.call(AppCommand::Run {
            id: name.into(),
            idempotency_key: name.into(),
        });
    }
    wait(|| d.call(AppCommand::Status)["active"] == 2);
    thread::sleep(Duration::from_millis(100));
    let bytes: u64 = std::fs::read_dir(d.path().join("logs"))
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(bytes <= 4096, "mixed active log bytes {bytes}");
}
#[test]
fn job_only_operator_cannot_cancel_a_running_service() {
    use cronharbor::contracts::Role;
    let mut m = job();
    m.kind = "Service".into();
    m.spec.execution.command = "/bin/sleep".into();
    m.spec.execution.args = vec!["30".into()];
    let d = Daemon::new(m);
    wait(|| {
        d.call(AppCommand::Workload {
            id: "local/hello".into(),
        })["observed_state"]
            == "READY"
    });
    let token = d.call(AppCommand::CreateToken {
        role: Role::Operator,
        namespaces: vec!["local".into()],
        capabilities: vec!["workload.run_preapproved".into()],
        ttl_seconds: 3600,
    });
    let token = token["token"].as_str().unwrap().to_owned();
    let execution = d.call(AppCommand::Executions {
        workload_id: None,
        cursor: None,
        limit: 10,
    })["items"][0]["execution_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let result = server::client(
        d.path(),
        Request::Execute {
            command: Box::new(AppCommand::Cancel {
                execution_id: execution.clone(),
                idempotency_key: "job-grant-cannot-stop-service".into(),
            }),
            token: Some(token),
        },
    );
    assert_eq!(result.unwrap_err().code, "NOT_AUTHORIZED");
    let job_token = d.call(AppCommand::CreateToken {
        role: Role::Operator,
        namespaces: vec!["local".into()],
        capabilities: vec!["workload.run_preapproved".into()],
        ttl_seconds: 3600,
    });
    let pause = server::client(
        d.path(),
        Request::Execute {
            command: Box::new(AppCommand::Pause {
                id: "local/hello".into(),
                paused: true,
                idempotency_key: "job-grant-cannot-pause-service".into(),
            }),
            token: Some(job_token["token"].as_str().unwrap().into()),
        },
    );
    assert_eq!(pause.unwrap_err().code, "NOT_AUTHORIZED");
    assert_eq!(
        d.call(AppCommand::Execution {
            id: execution.clone()
        })["state"],
        "RUNNING"
    );
    let service_token = d.call(AppCommand::CreateToken {
        role: Role::Operator,
        namespaces: vec!["local".into()],
        capabilities: vec!["service.control_preapproved".into()],
        ttl_seconds: 3600,
    });
    let allowed = server::client(
        d.path(),
        Request::Execute {
            command: Box::new(AppCommand::Cancel {
                execution_id: execution.clone(),
                idempotency_key: "service-grant-can-stop-service".into(),
            }),
            token: Some(service_token["token"].as_str().unwrap().into()),
        },
    )
    .unwrap();
    assert_eq!(allowed["state"], "ACCEPTED");
    wait(|| {
        d.call(AppCommand::Execution {
            id: execution.clone(),
        })["state"]
            == "CANCELLED"
    });
}
#[test]
fn forked_probe_descendants_are_cleaned_before_readiness_and_daemon_shutdown() {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    #[cfg(target_os = "linux")]
    nix::sys::prctl::set_child_subreaper(true).unwrap();
    let files = tempfile::tempdir_in("/tmp").unwrap();
    let pid_file = files.path().join("probe-child.pid");
    let mut m = job();
    m.kind = "Service".into();
    m.spec.execution.command = "/bin/sleep".into();
    m.spec.execution.args = vec!["30".into()];
    m.spec.service.readiness = Some(cronharbor::config::Probe {
        command: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            format!(
                "/bin/sleep 1000 & child=$!; echo $child > '{}'; exit 0",
                pid_file.display()
            ),
        ],
        interval_seconds: 60,
        timeout_seconds: 1,
        failure_threshold: 1,
        success_threshold: 1,
    });
    let d = Daemon::new(m);
    wait(|| pid_file.exists());
    let child = Pid::from_raw(
        std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap(),
    );
    thread::sleep(Duration::from_millis(150));
    let false_ready = d.call(AppCommand::Workload {
        id: "local/hello".into(),
    })["observed_state"]
        == "READY"
        && kill(child, None).is_ok();
    drop(d);
    let deadline = Instant::now() + Duration::from_secs(2);
    while kill(child, None).is_ok() && Instant::now() < deadline {
        #[cfg(target_os = "linux")]
        let _ = nix::sys::wait::waitpid(child, Some(nix::sys::wait::WaitPidFlag::WNOHANG));
        thread::sleep(Duration::from_millis(10));
    }
    let gone = kill(child, None).is_err();
    if !gone {
        let _ = kill(child, Signal::SIGKILL);
        #[cfg(target_os = "linux")]
        let _ = nix::sys::wait::waitpid(child, None);
    }
    assert!(gone, "probe descendant survived daemon shutdown");
    assert!(
        !false_ready,
        "readiness was accepted while a probe descendant survived"
    );
}
#[test]
fn ordinary_successful_exec_probe_becomes_ready_on_the_native_platform() {
    let mut m = job();
    m.kind = "Service".into();
    m.spec.execution.command = "/bin/sleep".into();
    m.spec.execution.args = vec!["30".into()];
    m.spec.service.readiness = Some(cronharbor::config::Probe {
        command: "/usr/bin/true".into(),
        args: vec![],
        interval_seconds: 60,
        timeout_seconds: 1,
        failure_threshold: 1,
        success_threshold: 1,
    });
    let d = Daemon::new(m);
    wait(|| {
        d.call(AppCommand::Workload {
            id: "local/hello".into(),
        })["observed_state"]
            == "READY"
    });
    let detail = d.call(AppCommand::Workload {
        id: "local/hello".into(),
    });
    assert!(
        detail["probe_history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "readiness" && event["healthy"] == true)
    );
}

#[test]
fn start_during_slow_service_stop_replaces_the_cancelled_process() {
    for policy in ["on-failure", "never"] {
        let files = tempfile::tempdir_in("/tmp").unwrap();
        let ready = files.path().join("ready");
        let stopping = files.path().join("stopping");
        let mut manifest = job();
        manifest.kind = "Service".into();
        manifest.spec.service.restart = policy.into();
        manifest.spec.service.max_restarts = 0;
        manifest.spec.service.stop_grace_seconds = 2;
        manifest.spec.execution.command = "/bin/sh".into();
        manifest.spec.execution.args = vec![
            "-c".into(),
            format!(
                "trap 'touch {}; sleep 0.3; exit 0' TERM; touch {}; while :; do sleep 0.05; done",
                stopping.display(),
                ready.display()
            ),
        ];
        let daemon = Daemon::new(manifest);
        wait(|| ready.exists());
        wait(|| {
            daemon.call(AppCommand::Workload {
                id: "local/hello".into(),
            })["observed_state"]
                == "READY"
        });
        let stop = daemon.call(AppCommand::ServiceDesired {
            id: "local/hello".into(),
            desired_state: "STOPPED".into(),
            idempotency_key: "stop".into(),
            expected_revision: None,
        });
        wait(|| stopping.exists());
        let start = daemon.call(AppCommand::ServiceDesired {
            id: "local/hello".into(),
            desired_state: "RUNNING".into(),
            idempotency_key: "start-again".into(),
            expected_revision: None,
        });
        assert_eq!(start["state"], "ACCEPTED");
        wait(|| {
            let operation = daemon.call(AppCommand::Operation {
                id: start["id"].as_str().unwrap().into(),
            });
            assert!(
                !matches!(operation["state"].as_str(), Some("UNKNOWN" | "FAILED")),
                "{policy}: replacement failed: {operation}; executions: {}",
                daemon.call(AppCommand::Executions {
                    workload_id: None,
                    cursor: None,
                    limit: 10,
                })
            );
            operation["state"] == "SUCCEEDED"
        });
        assert_eq!(
            daemon.call(AppCommand::Operation {
                id: stop["id"].as_str().unwrap().into()
            })["error_code"],
            "OPERATION_SUPERSEDED"
        );
        let rows = daemon.call(AppCommand::Executions {
            workload_id: None,
            cursor: None,
            limit: 10,
        });
        let items = rows["items"].as_array().unwrap();
        assert_eq!(items.len(), 2, "{policy}");
        assert_eq!(
            items
                .iter()
                .filter(|row| row["state"] == "CANCELLED")
                .count(),
            1
        );
        assert_eq!(
            items.iter().filter(|row| row["state"] == "RUNNING").count(),
            1
        );
    }
}

#[test]
fn unavailable_aged_restore_logs_do_not_block_startup_or_kill_periodic_owner() {
    use cronharbor::{
        ops,
        process::{EndState, Outcome},
        store::Store,
    };
    use std::io::Write;
    struct Owner {
        shutdown: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<cronharbor::Result<()>>>,
    }
    impl Drop for Owner {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
    for phase in ["startup", "periodic"] {
        for loss in ["missing", "short", "extra"] {
            let root = tempfile::tempdir_in("/tmp").unwrap();
            let source = root.path().join("source");
            let target = root.path().join("restore");
            let mut store = Store::open(&source).unwrap();
            let manifest = job();
            store.apply(&manifest, 0).unwrap();
            let launch = store.claim_due(60_000_000).unwrap().unwrap();
            store.start_requested(&launch.attempt_id, 0).unwrap();
            store.running(&launch, 1, 0).unwrap();
            store.prepare_log_capture(&launch).unwrap();
            store.append_log(&launch, b"evidence").unwrap();
            store
                .finish(
                    &launch,
                    &Outcome {
                        state: EndState::Succeeded,
                        error_code: None,
                        exit_code: Some(0),
                        log_bytes: 8,
                        log_truncated: false,
                    },
                    1,
                )
                .unwrap();
            if phase == "startup" {
                let db = rusqlite::Connection::open(source.join("state.db")).unwrap();
                db.execute("UPDATE log_segments SET created_at_us=1", [])
                    .unwrap();
            }
            let backup = store.backup("restore-evidence").unwrap()["path"]
                .as_str()
                .unwrap()
                .to_owned();
            drop(store);
            ops::restore(
                &target,
                Path::new(&backup),
                None,
                "aged log restore fixture",
                false,
            )
            .unwrap();
            let logs = cronharbor::store::private_directory(&target.join("logs")).unwrap();
            let log = logs.join(format!("{}.log", launch.attempt_id));
            let changed = match loss {
                "missing" => None,
                "short" => Some(b"eve".as_slice()),
                "extra" => Some(b"evidence unexplained tail".as_slice()),
                _ => unreachable!(),
            };
            if phase == "periodic" {
                cronharbor::store::secure_file(&log)
                    .unwrap()
                    .write_all(b"evidence")
                    .unwrap();
            } else if let Some(bytes) = changed {
                cronharbor::store::secure_file(&log)
                    .unwrap()
                    .write_all(bytes)
                    .unwrap();
            }
            let shutdown = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&shutdown);
            let path = target.clone();
            let mut owner = Owner {
                shutdown,
                thread: Some(thread::spawn(move || {
                    server::run_with_shutdown(&path, &[], true, flag)
                })),
            };
            let deadline = Instant::now() + Duration::from_secs(4);
            loop {
                if server::client(&target, Request::Status).is_ok() {
                    break;
                }
                if owner.thread.as_ref().unwrap().is_finished() {
                    let result = owner.thread.take().unwrap().join().unwrap();
                    assert!(
                        result.is_ok(),
                        "{phase}/{loss}: daemon stopped before control socket: {result:?}"
                    );
                    panic!("daemon unexpectedly stopped");
                }
                assert!(
                    Instant::now() < deadline,
                    "{phase}/{loss}: daemon did not start"
                );
                thread::sleep(Duration::from_millis(5));
            }
            if phase == "periodic" {
                match changed {
                    Some(bytes) => std::fs::write(&log, bytes).unwrap(),
                    None => std::fs::remove_file(&log).unwrap(),
                };
                let db = rusqlite::Connection::open(target.join("state.db")).unwrap();
                db.execute("UPDATE log_segments SET created_at_us=1", [])
                    .unwrap();
                thread::sleep(Duration::from_millis(1300));
            }
            assert!(
                server::client(&target, Request::Status).is_ok(),
                "{phase}/{loss}: unavailable evidence killed owner"
            );
            let info = server::client(
                &target,
                Request::Execute {
                    command: Box::new(AppCommand::LogInfo {
                        attempt_id: launch.attempt_id.clone(),
                    }),
                    token: None,
                },
            )
            .unwrap();
            assert_eq!(info["last_error_code"], "LOG_UNAVAILABLE", "{phase}/{loss}");
            assert_eq!(
                info["retained_bytes"], 8,
                "{phase}/{loss}: evidence reservation was released"
            );
            if let Some(bytes) = changed {
                assert_eq!(
                    std::fs::read(&log).unwrap(),
                    bytes,
                    "{phase}/{loss}: unavailable evidence was removed"
                );
            }
            let usage = server::client(
                &target,
                Request::Execute {
                    command: Box::new(AppCommand::LogStorage),
                    token: None,
                },
            )
            .unwrap();
            assert_eq!(usage["usage"]["retained_bytes"], 8);
            assert!(usage["usage"]["capture_error_count"].as_u64().unwrap() > 0);
        }
    }
}
