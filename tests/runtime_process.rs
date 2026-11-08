use cronharbor::{
    config::{Manifest, Probe, SecretRef},
    process::{EndState, RunningProcess},
    store::Store,
};
use std::{
    os::unix::fs::PermissionsExt,
    thread,
    time::{Duration, Instant},
};
fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let s = Store::open(d.path()).unwrap();
    (d, s)
}
#[test]
fn referenced_secret_is_injected_and_redacted_from_captured_logs() {
    let (d, mut s) = store();
    let secret = d.path().join("secret");
    std::fs::write(&secret, "sensitive-reference-value").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut m = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    m.spec.execution.command = "/usr/bin/printenv".into();
    m.spec.execution.args = vec!["TEST_SECRET".into()];
    m.spec.execution.secret_refs.insert(
        "TEST_SECRET".into(),
        SecretRef::File {
            path: secret.to_string_lossy().into_owned(),
        },
    );
    s.apply(&m, 0).unwrap();
    let l = s.claim_due(60_000_000).unwrap().unwrap();
    s.start_requested(&l.attempt_id, 60_000_000).unwrap();
    let mut p = RunningProcess::spawn(&l, &mut s).unwrap();
    s.running(&l, p.pid(), 60_000_000).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(o) = p.poll(&mut s).unwrap() {
            assert_eq!(o.state, EndState::Succeeded);
            s.finish(&l, &o, 61_000_000).unwrap();
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let log = std::fs::read_to_string(s.log_path(&l.attempt_id).unwrap()).unwrap();
    assert_eq!(log, "[REDACTED]\n");
}

#[test]
fn nine_simultaneous_native_jobs_retain_their_independent_output() {
    let (_directory, mut store) = store();
    for index in 0..9 {
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.metadata.name = format!("capture-{index}");
        manifest.spec.execution.args = vec![format!("capture-{index}")];
        store.apply(&manifest, 0).unwrap();
    }
    let mut processes = Vec::new();
    for _ in 0..9 {
        let launch = store.claim_due(60_000_000).unwrap().unwrap();
        store
            .start_requested(&launch.attempt_id, 60_000_000)
            .unwrap();
        let process = RunningProcess::spawn(&launch, &mut store).unwrap();
        store.running(&launch, process.pid(), 60_000_000).unwrap();
        processes.push((launch, process));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !processes.is_empty() {
        let mut index = 0;
        while index < processes.len() {
            if let Some(outcome) = processes[index].1.poll(&mut store).unwrap() {
                let (launch, _) = processes.swap_remove(index);
                assert_eq!(outcome.state, EndState::Succeeded);
                assert!(
                    !outcome.log_truncated,
                    "quiet concurrent jobs must retain capture"
                );
                store.finish(&launch, &outcome, 61_000_000).unwrap();
                let response = cronharbor::application::execute(
                    &mut store,
                    cronharbor::contracts::AppCommand::Logs {
                        attempt_id: launch.attempt_id,
                        offset: 0,
                        max_bytes: 65536,
                    },
                    None,
                )
                .unwrap();
                assert_eq!(
                    response["text"],
                    format!("{}\n", launch.manifest.metadata.name)
                );
            } else {
                index += 1;
            }
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn service_readiness_requires_a_passing_bounded_exec_probe() {
    let (_d, mut s) = store();
    let mut m = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    m.kind = "Service".into();
    m.spec.execution.command = "/bin/sleep".into();
    m.spec.execution.args = vec!["30".into()];
    m.spec.service.readiness = Some(Probe {
        command: "/usr/bin/false".into(),
        args: vec![],
        interval_seconds: 1,
        timeout_seconds: 1,
        failure_threshold: 1,
        success_threshold: 1,
    });
    s.apply(&m, 0).unwrap();
    let l = s.claim_due(1).unwrap().unwrap();
    let mut p = RunningProcess::spawn(&l, &mut s).unwrap();
    for _ in 0..20 {
        p.poll(&mut s).unwrap();
        thread::sleep(Duration::from_millis(10));
    }
    assert_ne!(p.service_observation(), "READY");
    p.cancel();
}
#[test]
fn total_log_disk_cap_bounds_simultaneous_processes() {
    let (d, mut s) = store();
    s.set_log_storage_policy(
        &cronharbor::contracts::LogStoragePolicyInput {
            max_disk_bytes: 4096,
            max_segments: 1024,
            retention_seconds: 604800,
        },
        1,
    )
    .unwrap();
    let mut launches = Vec::new();
    for name in ["one", "two"] {
        let mut m = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        m.metadata.name = name.into();
        m.spec.execution.command = "/bin/echo".into();
        m.spec.execution.args = vec!["a".repeat(6000)];
        m.spec.logs.max_run_bytes = 8192;

        s.apply(&m, 0).unwrap();
    }
    for _ in 0..2 {
        let launch = s.claim_due(60_000_000).unwrap().unwrap();
        s.start_requested(&launch.attempt_id, 60_000_000).unwrap();
        let process = RunningProcess::spawn(&launch, &mut s).unwrap();
        s.running(&launch, process.pid(), 60_000_000).unwrap();
        launches.push((launch, process));
    }
    thread::sleep(Duration::from_millis(100));
    let peak: u64 = std::fs::read_dir(d.path().join("logs"))
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(peak <= 4096, "active log bytes {peak}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !launches.is_empty() {
        let mut i = 0;
        while i < launches.len() {
            if let Some(outcome) = launches[i].1.poll(&mut s).unwrap() {
                s.finish(&launches[i].0, &outcome, 61_000_000).unwrap();
                launches.swap_remove(i);
            } else {
                i += 1;
            }
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let bytes: u64 = std::fs::read_dir(d.path().join("logs"))
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(bytes <= 4096, "total bytes {bytes}");
}

#[test]
fn file_secret_resolution_rejects_nonprivate_linked_and_invalid_content() {
    use std::fs;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("secret");
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.secret_refs.insert(
        "TEST_SECRET".into(),
        SecretRef::File {
            path: path.to_str().unwrap().into(),
        },
    );
    fs::write(&path, "private-value\r\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let resolved = cronharbor::process::resolve_environment(&manifest.spec.execution).unwrap();
    assert_eq!(resolved.get("TEST_SECRET").unwrap(), "private-value");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(cronharbor::process::resolve_environment(&manifest.spec.execution).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&path, directory.path().join("alias")).unwrap();
    assert!(cronharbor::process::resolve_environment(&manifest.spec.execution).is_err());
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(directory.path().join("alias"), &path).unwrap();
    assert!(cronharbor::process::resolve_environment(&manifest.spec.execution).is_err());
    fs::remove_file(&path).unwrap();
    for content in [vec![], vec![b'x'; 8193], vec![0], vec![0xff]] {
        fs::write(&path, content).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(cronharbor::process::resolve_environment(&manifest.spec.execution).is_err());
    }
}
