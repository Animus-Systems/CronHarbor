use cronharbor::{
    application,
    config::Manifest,
    contracts::AppCommand,
    process::{EndState, Outcome},
    store::{Launch, Store},
};
use std::{io::Write, os::unix::fs::PermissionsExt};

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

fn finish(store: &mut Store, launch: &Launch, state: EndState, bytes: u64, now: i64) {
    store
        .finish(
            launch,
            &Outcome {
                state,
                error_code: match state {
                    EndState::Unknown => Some("TERMINATION_UNCONFIRMED"),
                    EndState::Cancelled => Some("CANCELLED_BY_OPERATOR"),
                    EndState::Failed => Some("EXIT_NONZERO"),
                    EndState::TimedOut => Some("DEADLINE_EXCEEDED"),
                    EndState::Succeeded => None,
                },
                exit_code: match state {
                    EndState::Succeeded => Some(0),
                    EndState::Failed => Some(1),
                    _ => None,
                },
                log_bytes: bytes,
                log_truncated: false,
            },
            now,
        )
        .unwrap();
}

#[test]
fn cancel_during_retry_backoff_finalizes_execution_without_rewriting_failed_attempt() {
    let (directory, mut store, mut manifest) = setup("OneShotJob");
    manifest.spec.retry.policy = "idempotent".into();
    manifest.spec.retry.max_attempts = 2;
    manifest.spec.retry.backoff_seconds = 3;
    store.apply(&manifest, 0).unwrap();
    let operation = call(
        &mut store,
        AppCommand::Run {
            id: "local/hello".into(),
            idempotency_key: "run".into(),
        },
    );
    let launch = store.claim_due(1).unwrap().unwrap();
    store.start_requested(&launch.attempt_id, 1).unwrap();
    finish(&mut store, &launch, EndState::Failed, 0, 2);
    let cancel = AppCommand::Cancel {
        execution_id: launch.execution_id.clone(),
        idempotency_key: "cancel-backoff".into(),
    };
    let result = call(&mut store, cancel.clone());
    assert_eq!(result["state"], "SUCCEEDED");
    drop(store);
    let mut store = Store::open(directory.path()).unwrap();
    assert_eq!(call(&mut store, cancel)["id"], result["id"]);
    assert!(store.claim_due(4_000_000).unwrap().is_none());
    let (execution, attempt, error, count): (String, String, String, i64) = rusqlite::Connection::open(store.directory.join("state.db"))
        .unwrap()
        .query_row(
            "SELECT e.state,a.state,a.error_code,(SELECT count(*) FROM attempts WHERE execution_id=e.id) FROM executions e JOIN attempts a ON a.execution_id=e.id WHERE e.id=?1",
            [&launch.execution_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(execution, "CANCELLED");
    assert_eq!(attempt, "FAILED");
    assert_eq!(error, "EXIT_NONZERO");
    assert_eq!(count, 1);
    let original = call(
        &mut store,
        AppCommand::Operation {
            id: operation["id"].as_str().unwrap().into(),
        },
    );
    assert_eq!(original["state"], "FAILED");
    assert_eq!(original["error_code"], "CANCELLED_BY_OPERATOR");
}

#[test]
fn unresolved_closed_logs_survive_retention_and_still_count_toward_budget() {
    let (directory, mut store, mut manifest) = setup("OneShotJob");
    manifest.spec.logs.max_disk_bytes = 4096;
    manifest.spec.logs.max_segments = 1;
    store.apply(&manifest, 0).unwrap();
    call(
        &mut store,
        AppCommand::Run {
            id: "local/hello".into(),
            idempotency_key: "unknown".into(),
        },
    );
    let unknown = store.claim_due(1).unwrap().unwrap();
    store.start_requested(&unknown.attempt_id, 1).unwrap();
    store.running(&unknown, 1, 1).unwrap();
    let logs = cronharbor::store::private_directory(&directory.path().join("logs")).unwrap();
    let evidence = vec![b'e'; 4096];
    cronharbor::store::secure_file(&logs.join(format!("{}.log", unknown.attempt_id)))
        .unwrap()
        .write_all(&evidence)
        .unwrap();
    finish(&mut store, &unknown, EndState::Unknown, 4096, 2);
    manifest.metadata.name = "other".into();
    store.apply(&manifest, 3).unwrap();
    call(
        &mut store,
        AppCommand::Run {
            id: "local/other".into(),
            idempotency_key: "other".into(),
        },
    );
    let other = store.claim_due(3).unwrap().unwrap();
    store.start_requested(&other.attempt_id, 3).unwrap();
    store.running(&other, 1, 3).unwrap();
    let mut other_log =
        cronharbor::store::secure_file(&logs.join(format!("{}.log", other.attempt_id))).unwrap();
    let (written, truncated) = cronharbor::process::write_log_chunk(
        &mut other_log,
        &logs,
        store.log_disk_limit().unwrap(),
        b"new output",
    )
    .unwrap();
    assert_eq!(written, 0);
    assert!(truncated);
    finish(&mut store, &other, EndState::Succeeded, 0, 4);
    assert_eq!(
        std::fs::read(store.log_path(&unknown.attempt_id).unwrap()).unwrap(),
        evidence
    );
    assert_eq!(
        store.log_path(&other.attempt_id).unwrap_err().code,
        "LOG_UNAVAILABLE"
    );
    assert!(store.attempt_is_unknown(&unknown.attempt_id).unwrap());
    assert_eq!(store.status(false).unwrap().active, 1);
    store.retain_logs().unwrap();
    assert_eq!(
        std::fs::read(store.log_path(&unknown.attempt_id).unwrap()).unwrap(),
        evidence
    );
    call(
        &mut store,
        AppCommand::Reconcile {
            execution_id: unknown.execution_id,
            state: "CANCELLED".into(),
            evidence: "operator verified termination".into(),
            idempotency_key: "reconcile".into(),
        },
    );
    manifest.spec.logs.retention_seconds = 1;
    store.apply(&manifest, 5).unwrap();
    store.retain_logs().unwrap();
    assert_eq!(
        store.log_path(&unknown.attempt_id).unwrap_err().code,
        "LOG_UNAVAILABLE"
    );
}

#[test]
fn retry_cancellation_rolls_back_queue_removal_when_finalization_fails() {
    let (_directory, mut store, mut manifest) = setup("OneShotJob");
    manifest.spec.retry.policy = "idempotent".into();
    manifest.spec.retry.max_attempts = 2;
    store.apply(&manifest, 0).unwrap();
    let run = call(
        &mut store,
        AppCommand::Run {
            id: "local/hello".into(),
            idempotency_key: "run".into(),
        },
    );
    let launch = store.claim_due(1).unwrap().unwrap();
    store.start_requested(&launch.attempt_id, 1).unwrap();
    finish(&mut store, &launch, EndState::Failed, 0, 2);
    let database = rusqlite::Connection::open(store.directory.join("state.db")).unwrap();
    database.execute_batch("CREATE TRIGGER fail_cancel BEFORE UPDATE ON operations WHEN NEW.state='FAILED' AND NEW.error_code='CANCELLED_BY_OPERATOR' BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    let error = application::execute(
        &mut store,
        AppCommand::Cancel {
            execution_id: launch.execution_id.clone(),
            idempotency_key: "cancel-fails".into(),
        },
        None,
    )
    .unwrap_err();
    assert_eq!(error.code, "PERSISTENCE_UNAVAILABLE");
    assert_eq!(
        call(
            &mut store,
            AppCommand::Operation {
                id: run["id"].as_str().unwrap().into()
            }
        )["state"],
        "RUNNING"
    );
    let state: String = database
        .query_row(
            "SELECT state FROM executions WHERE id=?1",
            [&launch.execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "CREATED");
    database.execute_batch("DROP TRIGGER fail_cancel").unwrap();
    let retry = store.claim_due(2_000_000).unwrap().unwrap();
    assert_eq!(retry.execution_id, launch.execution_id);
}

#[test]
fn a_new_revision_retries_a_failed_service_with_an_exhausted_restart_budget() {
    let (_directory, mut store, mut manifest) = setup("Service");
    manifest.spec.service.max_restarts = 0;
    store.apply(&manifest, 0).unwrap();
    let failed = store.claim_due(1).unwrap().unwrap();
    store.start_requested(&failed.attempt_id, 1).unwrap();
    finish(&mut store, &failed, EndState::Failed, 0, 2);
    assert!(store.claim_due(3).unwrap().is_none());
    manifest.spec.execution.command = "/bin/sleep".into();
    manifest.spec.execution.args = vec!["30".into()];
    let revision = store.apply(&manifest, 3).unwrap();
    let replacement = store.claim_due(3).unwrap().unwrap();
    assert_eq!(replacement.manifest.spec.execution.command, "/bin/sleep");
    let actual: String = rusqlite::Connection::open(store.directory.join("state.db"))
        .unwrap()
        .query_row(
            "SELECT revision_id FROM executions WHERE id=?1",
            [&replacement.execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(actual, revision);
    assert_ne!(replacement.execution_id, failed.execution_id);
}

#[test]
fn a_new_service_revision_preserves_unknown_evidence_and_stopped_intent() {
    for state in [EndState::Unknown, EndState::Cancelled] {
        let (_directory, mut store, mut manifest) = setup("Service");
        store.apply(&manifest, 0).unwrap();
        let launch = store.claim_due(1).unwrap().unwrap();
        store.start_requested(&launch.attempt_id, 1).unwrap();
        if state == EndState::Cancelled {
            call(
                &mut store,
                AppCommand::ServiceDesired {
                    id: "local/hello".into(),
                    desired_state: "STOPPED".into(),
                    idempotency_key: "stop".into(),
                    expected_revision: None,
                },
            );
        }
        finish(&mut store, &launch, state, 0, 2);
        manifest.spec.execution.args = vec!["new revision".into()];
        store.apply(&manifest, 3).unwrap();
        assert!(store.claim_due(4_000_000).unwrap().is_none());
        let (desired, observed): (String, String) =
            rusqlite::Connection::open(store.directory.join("state.db"))
                .unwrap()
                .query_row(
                    "SELECT desired_state,observed_state FROM service_instances",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
        if state == EndState::Unknown {
            assert_eq!(observed, "UNKNOWN");
            assert_eq!(store.status(false).unwrap().active, 1);
        } else {
            assert_eq!(desired, "STOPPED");
            assert_eq!(observed, "ABSENT");
        }
    }
}

#[test]
fn recovery_reclaims_only_provably_unlaunched_service_intents() {
    for start_requested in [false, true] {
        let (directory, mut store, mut manifest) = setup("Service");
        manifest.spec.service.restart = "never".into();
        manifest.spec.service.max_restarts = 0;
        store.apply(&manifest, 0).unwrap();
        let interrupted = store.claim_due(1).unwrap().unwrap();
        if start_requested {
            store.start_requested(&interrupted.attempt_id, 1).unwrap();
        }
        drop(store);
        let mut store = Store::open(directory.path()).unwrap();
        let replacement = store.claim_due(2).unwrap();
        if start_requested {
            assert!(replacement.is_none());
            assert!(store.attempt_is_unknown(&interrupted.attempt_id).unwrap());
            assert_eq!(store.status(false).unwrap().active, 1);
        } else {
            let replacement =
                replacement.expect("provably unlaunched service must remain eligible");
            assert_ne!(replacement.execution_id, interrupted.execution_id);
            assert_eq!(
                store
                    .history()
                    .unwrap()
                    .iter()
                    .find(|row| row.execution_id == interrupted.execution_id)
                    .unwrap()
                    .error_code
                    .as_deref(),
                Some("START_INTERRUPTED")
            );
        }
    }
}

#[test]
fn a_capacity_blocked_retry_does_not_starve_an_unrelated_workload() {
    let (_directory, mut store, mut manifest) = setup("OneShotJob");
    manifest.spec.retry.policy = "idempotent".into();
    manifest.spec.retry.max_attempts = 2;
    store.apply(&manifest, 0).unwrap();
    manifest.metadata.name = "other".into();
    store.apply(&manifest, 0).unwrap();
    let run = |store: &mut Store, name: &str, key: &str| {
        call(
            store,
            AppCommand::Run {
                id: format!("local/{name}"),
                idempotency_key: key.into(),
            },
        );
    };
    run(&mut store, "hello", "first");
    let first = store.claim_due(1).unwrap().unwrap();
    store.start_requested(&first.attempt_id, 1).unwrap();
    finish(&mut store, &first, EndState::Failed, 0, 2);
    run(&mut store, "hello", "blocker");
    let blocker = store.claim_due(3).unwrap().unwrap();
    store.start_requested(&blocker.attempt_id, 3).unwrap();
    finish(&mut store, &blocker, EndState::Unknown, 0, 4);
    run(&mut store, "other", "second");
    let second = store.claim_due(5).unwrap().unwrap();
    store.start_requested(&second.attempt_id, 5).unwrap();
    finish(&mut store, &second, EndState::Failed, 0, 6);
    let retried = store
        .claim_due(2_000_000)
        .unwrap()
        .expect("other workload has capacity");
    assert_eq!(retried.execution_id, second.execution_id);
    assert!(store.claim_due(3_000_000).unwrap().is_none());
    call(
        &mut store,
        AppCommand::Reconcile {
            execution_id: blocker.execution_id,
            state: "CANCELLED".into(),
            evidence: "verified stopped effect".into(),
            idempotency_key: "release-capacity".into(),
        },
    );
    assert_eq!(
        store.claim_due(3_000_000).unwrap().unwrap().execution_id,
        first.execution_id
    );
}
