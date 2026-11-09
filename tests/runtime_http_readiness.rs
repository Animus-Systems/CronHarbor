#![cfg(feature = "http-probes")]
use cronharbor::{
    config::Manifest,
    contracts::AppCommand,
    server::{self, Request},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    _root: tempfile::TempDir,
    state: PathBuf,
    properties: PathBuf,
    healthy: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    hold: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    daemon: Option<thread::JoinHandle<cronharbor::Result<()>>>,
    http: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn start(healthy: bool, hold: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let helper = root.path().join("systemctl");
        fs::write(&helper,r##"#!/bin/sh
case "$1" in --user|--system) shift;; esac
case "$1" in --no-block) shift;; esac
case "$1" in
 show) cat "$0.properties";;
 stop) printf 'LoadState=loaded\nActiveState=inactive\nSubState=dead\nResult=success\nExecMainCode=1\nExecMainStatus=0\nInvocationID=11111111111111111111111111111111\nJob=\n' > "$0.properties";;
 start) exit 0;;
 *) exit 1;;
esac
"##).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let properties = root.path().join("systemctl.properties");
        fs::write(&properties,"LoadState=loaded\nActiveState=active\nSubState=running\nResult=success\nExecMainCode=1\nExecMainStatus=0\nInvocationID=11111111111111111111111111111111\nNRestarts=0\nJob=\n").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let healthy = Arc::new(AtomicBool::new(healthy));
        let requests = Arc::new(AtomicUsize::new(0));
        let hold = Arc::new(AtomicBool::new(hold));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (flag, count, gate, done) = (
            healthy.clone(),
            requests.clone(),
            hold.clone(),
            shutdown.clone(),
        );
        let http = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut bytes = [0; 4096];
                        let _ = stream.read(&mut bytes);
                        count.fetch_add(1, Ordering::Relaxed);
                        let bound = Instant::now() + Duration::from_secs(2);
                        while gate.load(Ordering::Relaxed)
                            && !done.load(Ordering::Relaxed)
                            && Instant::now() < bound
                        {
                            thread::sleep(Duration::from_millis(5));
                        }
                        let body = if flag.load(Ordering::Relaxed) {
                            "{\"ok\":true}"
                        } else {
                            "{\"ok\":false}"
                        };
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        let mut value =
            serde_json::to_value(Manifest::parse(include_str!("../examples/hello.yaml")).unwrap())
                .unwrap();
        value["kind"] = json!("Service");
        value["metadata"]["name"] = json!("http-app");
        value["spec"]["execution"]["adapter"] = json!("systemd");
        value["spec"]["execution"]["command"] = json!("");
        value["spec"]["execution"]["args"] = json!([]);
        value["spec"]["execution"]["systemd"] = json!({"unit":"app.service"});
        let probe = json!({"url":format!("http://127.0.0.1:{port}/check"),"status":200,"check":{"type":"json","equals":{"ok":true}},"intervalSeconds":1,"timeoutSeconds":1,"failureThreshold":3,"successThreshold":2});
        value["spec"]["service"]["httpReadiness"] = probe.clone();
        value["spec"]["service"]["desiredState"] = json!("STOPPED");
        value["spec"]["service"]["restart"] = json!("never");
        value["spec"]["service"]["stopGraceSeconds"] = json!(0);
        value["spec"]["service"]["startupGraceSeconds"] = json!(2);
        let mut store = Store::open(&state).unwrap();
        store
            .apply(
                &Manifest::parse(&value.to_string()).unwrap(),
                cronharbor::now_us(),
            )
            .unwrap();
        drop(store);
        let policy = json!({"systemdEnabled":true,"systemctlBinary":helper,"scopedUnits":[{"unit":"app.service","scope":"user"}],"systemdHttpReadiness":[{"unit":"app.service","scope":"user","url":probe["url"],"status":200,"check":probe["check"]}]});
        fs::write(state.join("executors.json"), policy.to_string()).unwrap();
        fs::set_permissions(
            state.join("executors.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let dir = state.clone();
        let stop = shutdown.clone();
        let daemon = thread::spawn(move || server::run_with_shutdown(&dir, &[], false, stop));
        let f = Self {
            _root: root,
            state,
            properties,
            healthy,
            requests,
            hold,
            shutdown,
            daemon: Some(daemon),
            http: Some(http),
        };
        wait(|| f.state.join("control.sock").exists());
        f.call(AppCommand::ServiceDesired {
            id: "local/http-app".into(),
            desired_state: "RUNNING".into(),
            expected_revision: None,
            idempotency_key: "initial-start".into(),
        });
        f
    }
    fn call(&self, command: AppCommand) -> Value {
        server::client(
            &self.state,
            Request::Execute {
                command: Box::new(command),
                token: None,
            },
        )
        .unwrap()
    }
    fn workload(&self) -> Value {
        self.call(AppCommand::Workload {
            id: "local/http-app".into(),
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.hold.store(false, Ordering::Relaxed);
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(task) = self.http.take() {
            task.join().unwrap();
        }
        if let Some(task) = self.daemon.take() {
            task.join().unwrap().unwrap();
        }
    }
}
#[track_caller]
fn wait(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < end, "readiness condition timed out");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn unit_active_is_not_ready_until_http_success_threshold_then_tracks_failure_and_recovery() {
    let f = Fixture::start(true, false);
    wait(|| f.requests.load(Ordering::Relaxed) > 0 || f.workload()["observed_state"] == "READY");
    assert!(
        f.requests.load(Ordering::Relaxed) >= 1,
        "unit was marked READY without an HTTP request"
    );
    assert_ne!(f.workload()["observed_state"], "READY");
    wait(|| f.workload()["observed_state"] == "READY");
    f.healthy.store(false, Ordering::Relaxed);
    wait(|| f.workload()["observed_state"] == "UNHEALTHY");
    assert_eq!(f.workload()["desired_state"], "RUNNING");
    assert_eq!(f.workload()["restart_count"], 0);
    f.healthy.store(true, Ordering::Relaxed);
    wait(|| f.workload()["observed_state"] == "READY");
    let value = f.workload();
    assert!(
        value["probe_history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["healthy"] == false)
    );
}

#[test]
fn a_late_http_success_cannot_override_a_stop_intent() {
    let f = Fixture::start(true, true);
    wait(|| f.requests.load(Ordering::Relaxed) > 0 || f.workload()["observed_state"] == "READY");
    assert!(
        f.requests.load(Ordering::Relaxed) > 0,
        "no readiness probe was made"
    );
    let stop = f.call(AppCommand::ServiceDesired {
        id: "local/http-app".into(),
        desired_state: "STOPPED".into(),
        expected_revision: None,
        idempotency_key: "stop-during-probe".into(),
    });
    f.hold.store(false, Ordering::Relaxed);
    wait(|| {
        f.call(AppCommand::Operation {
            id: stop["id"].as_str().unwrap().into(),
        })["state"]
            == "SUCCEEDED"
    });
    assert_eq!(f.workload()["desired_state"], "STOPPED");
    assert_ne!(f.workload()["observed_state"], "READY");
}

#[test]
fn a_new_os_invocation_discards_prior_success_streak() {
    let f = Fixture::start(true, false);
    wait(|| f.requests.load(Ordering::Relaxed) >= 1 || f.workload()["observed_state"] == "READY");
    assert!(
        f.requests.load(Ordering::Relaxed) > 0,
        "no readiness probe was made"
    );
    let properties = fs::read_to_string(&f.properties).unwrap().replace(
        "11111111111111111111111111111111",
        "22222222222222222222222222222222",
    );
    let staging = f.properties.with_extension("next");
    fs::write(&staging, properties).unwrap();
    fs::rename(staging, &f.properties).unwrap();
    wait(|| f.requests.load(Ordering::Relaxed) >= 2);
    assert_ne!(f.workload()["observed_state"], "READY");
    wait(|| f.workload()["observed_state"] == "READY");
}

#[test]
fn withdrawn_http_policy_preserves_the_unit_and_records_unknown() {
    let f = Fixture::start(true, false);
    wait(|| f.workload()["observed_state"] == "READY");
    let path = f.state.join("executors.json");
    let mut policy: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    policy["systemdHttpReadiness"] = json!([]);
    let staging = path.with_extension("next");
    fs::write(&staging, policy.to_string()).unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(staging, path).unwrap();
    wait(|| f.workload()["observed_state"] == "UNKNOWN");
    assert!(
        fs::read_to_string(&f.properties)
            .unwrap()
            .contains("ActiveState=active")
    );
    let requests = f.requests.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(requests, f.requests.load(Ordering::Relaxed));
}

#[test]
fn inactive_unit_cannot_become_ready_from_a_healthy_http_endpoint() {
    let f = Fixture::start(true, false);
    wait(|| f.workload()["observed_state"] == "READY");
    let stopped = fs::read_to_string(&f.properties)
        .unwrap()
        .replace("ActiveState=active", "ActiveState=inactive")
        .replace("SubState=running", "SubState=dead");
    let staging = f.properties.with_extension("next");
    fs::write(&staging, stopped).unwrap();
    fs::rename(staging, &f.properties).unwrap();
    wait(|| f.workload()["observed_state"] == "FAILED");
    assert_eq!(f.workload()["restart_count"], 0);
}

#[test]
fn startup_grace_expires_without_a_passing_http_sample() {
    let f = Fixture::start(false, false);
    wait(|| f.workload()["observed_state"] == "UNHEALTHY");
    assert_ne!(f.workload()["observed_state"], "READY");
    assert_eq!(f.workload()["restart_count"], 0);
    f.healthy.store(true, Ordering::Relaxed);
    wait(|| f.workload()["observed_state"] == "READY");
}
