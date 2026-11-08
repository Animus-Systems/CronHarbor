use cronharbor::{
    config::Manifest,
    process::{EndState, RunningProcess},
    store::Store,
};
use std::{
    thread,
    time::{Duration, Instant},
};

fn exercise(command: &str, args: &[&str], timeout: u64, cap: u64) -> cronharbor::process::Outcome {
    let dir = private_tempdir();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.command = command.to_owned();
    manifest.spec.execution.args = args.iter().map(|v| (*v).to_owned()).collect();
    manifest.spec.timeout_seconds = timeout;
    manifest.spec.logs.max_run_bytes = cap;
    let mut store = Store::open(dir.path()).unwrap();
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store
        .start_requested(&launch.attempt_id, 60_000_000)
        .unwrap();
    let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
    store.running(&launch, process.pid(), 60_000_000).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(outcome) = process.poll(&mut store).unwrap() {
            store.finish(&launch, &outcome, 61_000_000).unwrap();
            let bytes = std::fs::read(store.log_path(&launch.attempt_id).unwrap()).unwrap();
            assert!(bytes.len() <= usize::try_from(cap).unwrap());
            return outcome;
        }
        assert!(Instant::now() < deadline, "process never completed");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn captures_output_with_a_shared_byte_cap() {
    let outcome = exercise("/bin/echo", &["abcdefghijk"], 5, 4);
    assert_eq!(outcome.state, EndState::Succeeded);
    assert_eq!(outcome.log_bytes, 4);
    assert!(outcome.log_truncated);
}

#[test]
fn times_out_and_confirms_termination() {
    let outcome = exercise("/bin/sleep", &["30"], 1, 4096);
    assert_eq!(outcome.state, EndState::TimedOut);
}

#[test]
fn a_real_process_with_a_lost_receipt_is_quarantined_on_restart() {
    let dir = private_tempdir();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.command = "/bin/sleep".to_owned();
    manifest.spec.execution.args = vec!["30".to_owned()];
    let mut store = Store::open(dir.path()).unwrap();
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store
        .start_requested(&launch.attempt_id, 60_000_000)
        .unwrap();
    let process = RunningProcess::spawn(&launch, &mut store).unwrap();
    drop(store);
    // Allow bounded owner-lock handover while parallel subprocesses finish spawning.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut reopened = loop {
        match Store::open(dir.path()) {
            Ok(store) => break store,
            Err(error) if error.code == "OWNER_UNAVAILABLE" && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("owner handover failed: {error}"),
        }
    };
    assert_eq!(reopened.history().unwrap()[0].state, "UNKNOWN");
    assert!(reopened.claim_due(120_000_000).unwrap().is_none());
    drop(process);
}

#[test]
fn a_missing_executable_has_a_durable_start_rejected_result() {
    let dir = private_tempdir();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.command = "/does-not-exist/cronharbor-test".to_owned();
    let mut store = Store::open(dir.path()).unwrap();
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store
        .start_requested(&launch.attempt_id, 60_000_000)
        .unwrap();
    assert!(RunningProcess::spawn(&launch, &mut store).is_err());
    store
        .finish(
            &launch,
            &cronharbor::process::Outcome::rejected(),
            60_000_001,
        )
        .unwrap();
    let history = store.history().unwrap();
    assert_eq!(history[0].state, "FAILED");
    assert_eq!(history[0].error_code.as_deref(), Some("START_REJECTED"));
    assert_eq!(store.status(false).unwrap().active, 0);
}

#[test]
fn cancellation_escalates_when_the_child_ignores_term_and_reaps_it() {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    let dir = private_tempdir();
    let ready = dir.path().join("ready");
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.command = "/bin/sh".into();
    manifest.spec.execution.args = vec![
        "-c".into(),
        "trap '' TERM; printf ready > \"$1\"; exec /bin/sleep 30".into(),
        "cronharbor-test".into(),
        ready.to_str().unwrap().into(),
    ];
    let mut store = Store::open(dir.path()).unwrap();
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
    let pid = Pid::from_raw(i32::try_from(process.pid()).unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "child did not install its TERM handler"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let cancelled = Instant::now();
    process.cancel();
    let outcome = loop {
        if let Some(outcome) = process.poll(&mut store).unwrap() {
            break outcome;
        }
        assert!(
            Instant::now() < deadline,
            "cancelled child was not terminated"
        );
        thread::sleep(Duration::from_millis(5));
    };
    assert!(cancelled.elapsed() >= Duration::from_secs(1));
    assert_eq!(outcome.state, EndState::Cancelled);
    assert_eq!(outcome.error_code, Some("CANCELLED_BY_OPERATOR"));
    assert_eq!(kill(pid, None), Err(Errno::ESRCH));
}

#[test]
fn dropping_a_running_process_terminates_and_reaps_its_child() {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    let dir = private_tempdir();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.command = "/bin/sleep".into();
    manifest.spec.execution.args = vec!["30".into()];
    let mut store = Store::open(dir.path()).unwrap();
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    let process = RunningProcess::spawn(&launch, &mut store).unwrap();
    let pid = Pid::from_raw(i32::try_from(process.pid()).unwrap());
    drop(process);
    assert_eq!(kill(pid, None), Err(Errno::ESRCH));
}

fn private_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

#[test]
fn redaction_never_rescans_inserted_markers_and_prefers_longest_original_match() {
    let marker = b"[REDACTED]";
    let mut secrets = vec![
        b"sensitive".to_vec(),
        b"sensitive-value".to_vec(),
        Vec::new(),
    ];
    for start in 0..marker.len() {
        for end in start + 1..=marker.len() {
            secrets.push(marker[start..end].to_vec());
        }
    }
    let redacted = cronharbor::process::redact(b"sensitive-value".to_vec(), &secrets);
    assert_eq!(redacted.len(), marker.len());
    assert_eq!(redacted, marker);
}
