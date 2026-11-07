use cronharbor::{config::Manifest, process::RunningProcess, store::Store};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{
    fs,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn owner_sigkill_after_spawn_never_replays_an_uncertain_effect() {
    if let Some(directory) = std::env::var_os("CRONHARBOR_CRASH_TEST_STATE") {
        let directory = std::path::PathBuf::from(directory);
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.spec.execution.command = "/bin/sleep".to_owned();
        manifest.spec.execution.args = vec!["30".to_owned()];
        let mut store = Store::open(&directory).unwrap();
        store.apply(&manifest, 0).unwrap();
        let launch = store.claim_due(60_000_000).unwrap().unwrap();
        store
            .start_requested(&launch.attempt_id, 60_000_000)
            .unwrap();
        let process = RunningProcess::spawn(&launch, &directory).unwrap();
        fs::write(directory.join("spawned.pid"), process.pid().to_string()).unwrap();
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }
    #[cfg(target_os = "linux")]
    nix::sys::prctl::set_child_subreaper(true).unwrap();
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let state = temp.path().join("state");
    let mut owner = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "owner_sigkill_after_spawn_never_replays_an_uncertain_effect",
            "--nocapture",
        ])
        .env("CRONHARBOR_CRASH_TEST_STATE", &state)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !state.join("spawned.pid").exists() {
        if Instant::now() >= deadline || owner.try_wait().unwrap().is_some() {
            let _ = owner.kill();
            let _ = owner.wait();
            panic!("crash helper did not start a process");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let pid = Pid::from_raw(
        fs::read_to_string(state.join("spawned.pid"))
            .unwrap()
            .parse()
            .unwrap(),
    );
    owner.kill().unwrap();
    owner.wait().unwrap();
    let mut store = Store::open(&state).unwrap();
    let history = store.history().unwrap();
    let quarantined = history.len() == 1 && history[0].state == "UNKNOWN";
    let no_replay =
        store.claim_due(120_000_000).unwrap().is_none() && store.history().unwrap().len() == 1;
    let _ = killpg(pid, Signal::SIGKILL);
    #[cfg(target_os = "linux")]
    nix::sys::wait::waitpid(pid, None).unwrap();
    assert!(quarantined);
    assert!(no_replay);
}
