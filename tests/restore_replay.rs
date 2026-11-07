use cronharbor::{
    config::Manifest,
    ops,
    process::{EndState, RunningProcess},
    store::Store,
};
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

#[test]
fn restored_snapshot_never_replays_post_backup_effects_under_any_misfire_policy() {
    for policy in ["skip", "latest", "catch_up"] {
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("source");
        let effect = temporary.path().join("downstream-effect");
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.spec.schedule.r#type = "interval".into();
        manifest.spec.schedule.interval_seconds = 1;
        manifest.spec.schedule.anchor_at_us = cronharbor::now_us() + 1_000_000;
        manifest.spec.schedule.misfire.policy = policy.into();
        manifest.spec.schedule.misfire.max_occurrences = 3;
        manifest.spec.execution.command = "/bin/sh".into();
        manifest.spec.execution.args = vec![
            "-c".into(),
            "printf effect >> \"$1\"".into(),
            "effect-fixture".into(),
            effect.to_str().unwrap().into(),
        ];
        let mut store = Store::open(&state).unwrap();
        store.apply(&manifest, cronharbor::now_us()).unwrap();
        let backup = store.backup("before-effect").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let launch = loop {
            if let Some(launch) = store.claim_due(cronharbor::now_us()).unwrap() {
                break launch;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        };
        assert!(launch.due_at_us > backup["manifest"]["backup_at_us"].as_i64().unwrap());
        store
            .start_requested(&launch.attempt_id, cronharbor::now_us())
            .unwrap();
        let mut process = RunningProcess::spawn(&launch, &state).unwrap();
        store
            .running(&launch, process.pid(), cronharbor::now_us())
            .unwrap();
        loop {
            if let Some(outcome) = process.poll().unwrap() {
                assert_eq!(outcome.state, EndState::Succeeded);
                store
                    .finish(&launch, &outcome, cronharbor::now_us())
                    .unwrap();
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fs::read_to_string(&effect).unwrap(), "effect");
        let cutoff = cronharbor::now_us();
        drop(store);
        let restored = temporary.path().join("restored");
        ops::restore(
            &restored,
            Path::new(backup["path"].as_str().unwrap()),
            None,
            "former owner stopped; downstream effect verified",
            false,
        )
        .unwrap();
        let mut store = Store::open(&restored).unwrap();
        assert!(
            store.history().unwrap().is_empty(),
            "snapshot predates the real effect"
        );
        assert!(store.paused().unwrap());
        assert!(store.claim_due(cutoff + 30_000_000).unwrap().is_none());
        store
            .restore_resume(
                cutoff,
                "exclusive owner and downstream effect reconciled",
                "local-owner",
            )
            .unwrap();
        assert!(store.claim_due(cutoff).unwrap().is_none());
        let next = store.claim_due(cutoff + 1_000_000).unwrap().unwrap();
        assert!(next.due_at_us > cutoff);
        assert_eq!(fs::read_to_string(&effect).unwrap(), "effect");
    }
}
