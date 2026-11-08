use cronharbor::{
    config::Manifest,
    process::{EndState, RunningProcess},
    store::Store,
};
use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Owner(Child);
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn sigkill_at_each_lifecycle_boundary_preserves_evidence_without_replaying_effects() {
    if let Some(directory) = std::env::var_os("CRONHARBOR_RECOVERY_MATRIX_STATE") {
        let directory = PathBuf::from(directory);
        let phase = std::env::var("CRONHARBOR_RECOVERY_MATRIX_PHASE").unwrap();
        let effect = directory.parent().unwrap().join("downstream-effect");
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.spec.execution.command = "/bin/sh".into();
        manifest.spec.execution.args = vec![
            "-c".into(),
            "printf effect >> \"$1\"".into(),
            "effect-fixture".into(),
            effect.to_str().unwrap().into(),
        ];
        let mut store = Store::open(&directory).unwrap();
        store.apply(&manifest, 0).unwrap();
        let launch = store.claim_due(60_000_000).unwrap().unwrap();
        if phase != "intent" {
            store
                .start_requested(&launch.attempt_id, 60_000_000)
                .unwrap();
        }
        if matches!(phase.as_str(), "receipt" | "terminal") {
            let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
            store.running(&launch, process.pid(), 60_000_000).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(outcome) = process.poll(&mut store).unwrap() {
                    assert_eq!(outcome.state, EndState::Succeeded);
                    assert_eq!(fs::read_to_string(&effect).unwrap(), "effect");
                    if phase == "terminal" {
                        store.finish(&launch, &outcome, 61_000_000).unwrap();
                    }
                    break;
                }
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(5));
            }
        }
        fs::write(directory.join("phase.ready"), &phase).unwrap();
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }

    for (phase, expected, has_effect) in [
        ("intent", "CANCELLED", false),
        ("requested", "UNKNOWN", false),
        ("receipt", "UNKNOWN", true),
        ("terminal", "SUCCEEDED", true),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("state");
        let effect = temporary.path().join("downstream-effect");
        let mut owner = Owner(Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "sigkill_at_each_lifecycle_boundary_preserves_evidence_without_replaying_effects", "--nocapture"])
            .env("CRONHARBOR_RECOVERY_MATRIX_STATE", &state)
            .env("CRONHARBOR_RECOVERY_MATRIX_PHASE", phase)
            .stdout(Stdio::null()).stderr(Stdio::inherit()).spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(8);
        while !state.join("phase.ready").exists() {
            assert!(
                owner.0.try_wait().unwrap().is_none(),
                "helper exited before {phase}"
            );
            assert!(Instant::now() < deadline, "helper stalled before {phase}");
            thread::sleep(Duration::from_millis(10));
        }
        owner.0.kill().unwrap();
        owner.0.wait().unwrap();
        let mut recovered = Store::open(&state).unwrap();
        let history = recovered.history().unwrap();
        assert_eq!(history.len(), 1, "{phase}");
        assert_eq!(history[0].state, expected, "{phase}");
        assert!(
            recovered.claim_due(60_000_000).unwrap().is_none(),
            "{phase} occurrence replayed"
        );
        assert_eq!(recovered.history().unwrap().len(), 1);
        if has_effect {
            assert_eq!(fs::read_to_string(&effect).unwrap(), "effect");
        } else {
            assert!(!effect.exists());
        }
    }
}
