use cronharbor::{
    application,
    config::Manifest,
    contracts::AppCommand,
    process::{EndState, Outcome},
    store::Store,
};
use std::{os::unix::fs::PermissionsExt, path::Path};

fn setup(kind: &str) -> (tempfile::TempDir, Store, Manifest) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = Store::open(directory.path()).unwrap();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.kind = kind.into();
    (directory, store, manifest)
}
fn call(store: &mut Store, command: AppCommand) -> serde_json::Value {
    application::execute(store, command, None).unwrap()
}
fn operation(store: &mut Store, value: &serde_json::Value) -> serde_json::Value {
    call(
        store,
        AppCommand::Operation {
            id: value["id"].as_str().unwrap().into(),
        },
    )
}
fn failure() -> Outcome {
    Outcome {
        state: EndState::Failed,
        error_code: Some("START_REJECTED"),
        exit_code: None,
        log_bytes: 0,
        log_truncated: false,
    }
}

#[test]
fn delayed_readiness_does_not_override_a_stopped_or_replaced_service() {
    for stop in [true, false] {
        let (_directory, mut store, mut manifest) = setup("Service");
        store.apply(&manifest, 1).unwrap();
        let launch = store.claim_due(2).unwrap().unwrap();
        store.start_requested(&launch.attempt_id, 2).unwrap();
        store.running(&launch, 12345, 3).unwrap();
        if stop {
            call(
                &mut store,
                AppCommand::ServiceDesired {
                    id: "local/hello".into(),
                    desired_state: "STOPPED".into(),
                    expected_revision: None,
                    idempotency_key: "stop-before-late-probe".into(),
                },
            );
        } else {
            manifest.spec.service.startup_grace_seconds += 1;
            store.apply(&manifest, 4).unwrap();
        }
        store.service_observation(&launch, "READY", 5).unwrap();
        let state: String = rusqlite::Connection::open(store.directory.join("state.db"))
            .unwrap()
            .query_row("SELECT observed_state FROM service_instances", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_ne!(
            state, "READY",
            "obsolete readiness replaced stop/revision state"
        );
    }
}

#[test]
fn exhausted_service_start_and_restart_operations_fail_with_the_observed_cause() {
    for restart in [false, true] {
        let (_directory, mut store, mut manifest) = setup("Service");
        manifest.spec.service.desired_state = "STOPPED".into();
        manifest.spec.service.max_restarts = 0;
        store.apply(&manifest, 0).unwrap();
        let command = if restart {
            AppCommand::ServiceRestart {
                id: "local/hello".into(),
                idempotency_key: "start".into(),
                expected_revision: None,
            }
        } else {
            AppCommand::ServiceDesired {
                id: "local/hello".into(),
                desired_state: "RUNNING".into(),
                idempotency_key: "start".into(),
                expected_revision: None,
            }
        };
        let accepted = call(&mut store, command.clone());
        let at = cronharbor::now_us();
        let launch = store.claim_due(at).unwrap().unwrap();
        store.start_requested(&launch.attempt_id, at).unwrap();
        store.finish(&launch, &failure(), at + 1).unwrap();
        assert!(store.claim_due(at + 999_000_000).unwrap().is_none());
        let result = operation(&mut store, &accepted);
        assert_eq!(result["state"], "FAILED");
        assert_eq!(result["error_code"], "START_REJECTED");
        assert_eq!(
            application::execute(&mut store, command, None)
                .unwrap_err()
                .code,
            "START_REJECTED"
        );
    }
}

#[test]
fn interrupted_manual_launch_finalizes_its_original_operation_on_recovery() {
    let (directory, mut store, manifest) = setup("OneShotJob");
    store.apply(&manifest, 0).unwrap();
    let accepted = call(
        &mut store,
        AppCommand::Run {
            id: "local/hello".into(),
            idempotency_key: "run".into(),
        },
    );
    store.claim_due(1).unwrap().unwrap();
    drop(store);
    let mut store = Store::open(directory.path()).unwrap();
    let result = operation(&mut store, &accepted);
    assert_eq!(result["state"], "FAILED");
    assert_eq!(result["error_code"], "START_INTERRUPTED");
    assert!(store.claim_due(999_000_000).unwrap().is_none());
}

#[test]
fn restore_quarantine_finalizes_discarded_retries_without_rewriting_attempt_evidence() {
    let (directory, mut store, mut manifest) = setup("OneShotJob");
    manifest.spec.retry.policy = "idempotent".into();
    manifest.spec.retry.max_attempts = 2;
    store.apply(&manifest, 0).unwrap();
    let accepted = call(
        &mut store,
        AppCommand::Run {
            id: "local/hello".into(),
            idempotency_key: "run".into(),
        },
    );
    let launch = store.claim_due(1).unwrap().unwrap();
    store.start_requested(&launch.attempt_id, 1).unwrap();
    store.finish(&launch, &failure(), 2).unwrap();
    let snapshot = store.backup("backoff").unwrap();
    drop(store);
    let restored = directory.path().join("restored");
    cronharbor::ops::restore(
        &restored,
        Path::new(snapshot["path"].as_str().unwrap()),
        None,
        "old owner stopped; effects verified",
        false,
    )
    .unwrap();
    let mut store = Store::open(&restored).unwrap();
    store
        .restore_resume(
            cronharbor::now_us(),
            "downstream effects reconciled",
            "local-owner",
        )
        .unwrap();
    let result = operation(&mut store, &accepted);
    assert_eq!(result["state"], "FAILED");
    assert_eq!(result["error_code"], "RESTORE_QUARANTINE");
    assert!(
        store
            .claim_due(cronharbor::now_us() + 120_000_000)
            .unwrap()
            .is_none()
    );
    let database = rusqlite::Connection::open(restored.join("state.db")).unwrap();
    let states: (String, String, String) = database.query_row("SELECT e.state,a.state,a.error_code FROM executions e JOIN attempts a ON a.execution_id=e.id WHERE e.id=?1", [&launch.execution_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(
        states,
        ("CANCELLED".into(), "FAILED".into(), "START_REJECTED".into())
    );
}
