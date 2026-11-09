use cronharbor::{
    application,
    config::Manifest,
    contracts::AppCommand,
    external::{ExecutorPolicy, ExternalHandle, SystemdSpec},
    server::{self, Request},
    store::{Launch, Store},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn fixture(root: &Path, active: &str) -> (PathBuf, Launch) {
    let state = root.join("state");
    let helper = root.join("systemctl");
    fs::write(&helper, format!(r##"#!/bin/sh
case "$1" in
show)
  seen=0
  if [ -f "$0.count" ]; then seen=$(cat "$0.count"); fi
  seen=$((seen + 1)); printf '%s' "$seen" > "$0.count"
  gate=1
  if [ -f "$0.gate" ]; then gate=$(cat "$0.gate"); fi
  count=0
  if [ "$seen" -ge "$gate" ]; then
    : > "$0.observing"
    while [ ! -f "$0.release" ] && [ "$count" -lt 300 ]; do sleep 0.01; count=$((count + 1)); done
  fi
  printf 'LoadState=loaded\nActiveState={active}\nResult=success\nExecMainCode=1\nExecMainStatus=0\n'
  : > "$0.observed"
  ;;
--no-pager) : > "$0.journal"; printf 'safe evidence\n' ;;
stop) : > "$0.stopped" ;;
*) exit 1 ;;
esac
"##)).unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.kind = "OneShotJob".into();
    manifest.spec.execution.adapter = "systemd".into();
    manifest.spec.execution.command.clear();
    manifest.spec.execution.args.clear();
    manifest.spec.execution.systemd = Some(SystemdSpec {
        unit: "worker.service".into(),
    });
    let mut store = Store::open(&state).unwrap();
    store.apply(&manifest, cronharbor::now_us()).unwrap();
    application::execute(
        &mut store,
        AppCommand::Run {
            id: "local/hello".into(),
            idempotency_key: "run".into(),
        },
        None,
    )
    .unwrap();
    let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    let handle = ExternalHandle::Systemd {
        unit: "worker.service".into(),
        since_us: Some(cronharbor::now_us()),
    };
    store
        .persist_external_intent(&launch.attempt_id, &handle)
        .unwrap();
    store
        .start_requested(&launch.attempt_id, cronharbor::now_us())
        .unwrap();
    let policy = ExecutorPolicy {
        systemd_enabled: true,
        systemctl_binary: helper.to_string_lossy().into_owned(),
        journalctl_binary: helper.to_string_lossy().into_owned(),
        legacy_systemd_scope: Some(cronharbor::external::SystemdScope::System),
        units: vec!["worker.service".into()],
        ..ExecutorPolicy::default()
    };
    let path = state.join("executors.json");
    fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    (state, launch)
}

struct Daemon {
    shutdown: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<cronharbor::Result<()>>>,
    release: PathBuf,
}
impl Daemon {
    fn start(state: &Path, release: PathBuf) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let state = state.to_owned();
        Self {
            shutdown,
            task: Some(thread::spawn(move || {
                server::run_with_shutdown(&state, &[], false, flag)
            })),
            release,
        }
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        fs::write(&self.release, "").unwrap();
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
#[track_caller]
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
}
fn call(state: &Path, command: AppCommand) -> serde_json::Value {
    server::client(
        state,
        Request::Execute {
            command: Box::new(command),
            token: None,
        },
    )
    .unwrap()
}

#[test]
fn reconciliation_retires_inflight_external_observations_without_stopping_the_owner() {
    for active in ["inactive", "active"] {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let (state, launch) = fixture(root.path(), active);
        let release = root.path().join("systemctl.release");
        let _daemon = Daemon::start(&state, release.clone());
        wait(|| root.path().join("systemctl.observing").exists());
        assert_eq!(
            call(
                &state,
                AppCommand::Reconcile {
                    execution_id: launch.execution_id.clone(),
                    state: "SUCCEEDED".into(),
                    evidence: "operator verified final downstream result".into(),
                    idempotency_key: "reconcile".into()
                }
            )["state"],
            "SUCCEEDED"
        );
        fs::write(release, "").unwrap();
        wait(|| root.path().join("systemctl.observed").exists());
        for _ in 0..10 {
            assert_eq!(
                call(
                    &state,
                    AppCommand::Execution {
                        id: launch.execution_id.clone()
                    }
                )["state"],
                "SUCCEEDED"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!root.path().join("systemctl.journal").exists());
    }
}

#[test]
fn reconciliation_retires_a_stop_blocked_inside_its_ownership_inspection() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let (state, launch) = fixture(root.path(), "active");
    rusqlite::Connection::open(state.join("state.db"))
        .unwrap()
        .execute(
            "UPDATE attempts SET started_at_us=?1",
            [cronharbor::now_us() - 400_000_000],
        )
        .unwrap();
    let release = root.path().join("systemctl.release");
    let _daemon = Daemon::start(&state, release.clone());
    wait(|| root.path().join("systemctl.observing").exists());
    call(
        &state,
        AppCommand::Reconcile {
            execution_id: launch.execution_id.clone(),
            state: "CANCELLED".into(),
            evidence: "operator proved containment independently".into(),
            idempotency_key: "reconcile-stop".into(),
        },
    );
    fs::write(release, "").unwrap();
    wait(|| root.path().join("systemctl.observed").exists());
    let final_state = call(
        &state,
        AppCommand::Execution {
            id: launch.execution_id,
        },
    );
    drop(_daemon);
    assert!(!root.path().join("systemctl.stopped").exists());
    assert_eq!(final_state["state"], "CANCELLED");
}

#[test]
fn reconciliation_retires_capture_blocked_inside_its_log_ownership_inspection() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let (state, launch) = fixture(root.path(), "inactive");
    std::fs::write(root.path().join("systemctl.gate"), "2").unwrap();
    let release = root.path().join("systemctl.release");
    let daemon = Daemon::start(&state, release.clone());
    wait(|| root.path().join("systemctl.observing").exists());
    call(
        &state,
        AppCommand::Reconcile {
            execution_id: launch.execution_id.clone(),
            state: "SUCCEEDED".into(),
            evidence: "verified downstream outcome independently".into(),
            idempotency_key: "reconcile-capture".into(),
        },
    );
    fs::write(release, "").unwrap();
    drop(daemon);
    assert!(!root.path().join("systemctl.journal").exists());
}

#[test]
fn restore_quarantine_does_not_replay_external_timeout_controls_before_resume() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let (state, _launch) = fixture(root.path(), "active");
    let mut store = Store::open(&state).unwrap();
    rusqlite::Connection::open(state.join("state.db"))
        .unwrap()
        .execute(
            "UPDATE attempts SET started_at_us=?1",
            [cronharbor::now_us() - 400_000_000],
        )
        .unwrap();
    let snapshot = store.backup("active").unwrap();
    drop(store);
    cronharbor::ops::restore(
        &state,
        Path::new(snapshot["path"].as_str().unwrap()),
        None,
        "old owner stopped; awaiting effect reconciliation",
        true,
    )
    .unwrap();
    let release = root.path().join("systemctl.release");
    fs::write(&release, "").unwrap();
    let _daemon = Daemon::start(&state, release);
    wait(|| server::client(&state, Request::Status).is_ok());
    for _ in 0..10 {
        let status = call(&state, AppCommand::Status);
        assert_eq!(status["restore_required"], true);
        assert_eq!(status["unknown"], 1);
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!root.path().join("systemctl.stopped").exists());
    assert!(!root.path().join("systemctl.observing").exists());
    call(
        &state,
        AppCommand::RestoreResume {
            cutoff_us: cronharbor::now_us(),
            evidence: "exclusive owner and downstream effect reconciled".into(),
        },
    );
    wait(|| root.path().join("systemctl.stopped").exists());
}
