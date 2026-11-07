use cronharbor::{application, config::Manifest, contracts::AppCommand, store::Store};
use rusqlite::Connection;

fn setup() -> (tempfile::TempDir, Store, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    let store = Store::open(&state).unwrap();
    let database = Connection::open(state.join("state.db")).unwrap();
    (directory, store, database)
}

fn definition(name: &str) -> String {
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.kind = "OneShotJob".into();
    manifest.metadata.name = name.into();
    manifest.spec.owner = "api".into();
    serde_json::to_string(&manifest).unwrap()
}

fn apply(store: &mut Store, name: &str) -> cronharbor::Result<serde_json::Value> {
    application::execute(
        store,
        AppCommand::Apply {
            definition: definition(name),
            expected_revision: None,
            source_kind: "API".into(),
            idempotency_key: name.into(),
        },
        None,
    )
}

#[test]
fn failed_operation_acknowledgement_rolls_back_effects_and_keeps_the_connection_usable() {
    for trigger in [
        "CREATE TRIGGER injected_failure BEFORE UPDATE ON operations WHEN NEW.state='SUCCEEDED' BEGIN SELECT RAISE(ABORT,'injected acknowledgement failure'); END;",
        "CREATE TRIGGER injected_failure BEFORE INSERT ON events WHEN NEW.event_kind='operation.complete' BEGIN SELECT RAISE(ABORT,'injected completion event failure'); END;",
    ] {
        let (_directory, mut store, database) = setup();
        database.execute_batch(trigger).unwrap();
        let error = apply(&mut store, "must-rollback").unwrap_err();
        assert_eq!(error.code, "PERSISTENCE_UNAVAILABLE");
        assert!(
            store.manifest_for("local/must-rollback").is_err(),
            "failed operation left a visible uncommitted definition"
        );
        let failed: i64 = database
            .query_row(
                "SELECT count(*) FROM operations WHERE state='FAILED'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(failed, 1);
        database
            .execute_batch("DROP TRIGGER injected_failure")
            .unwrap();
        assert_eq!(
            apply(&mut store, "next-command").unwrap()["state"],
            "SUCCEEDED"
        );
        assert!(store.manifest_for("local/next-command").is_ok());
    }
}

#[test]
fn failed_launch_writes_leave_no_partial_occurrence_or_attempt() {
    for trigger in [
        "CREATE TRIGGER injected_failure BEFORE INSERT ON attempts BEGIN SELECT RAISE(ABORT,'injected attempt failure'); END;",
        "CREATE TRIGGER injected_failure BEFORE INSERT ON audit_events WHEN NEW.action='execution.launch_intent' BEGIN SELECT RAISE(ABORT,'injected launch audit failure'); END;",
        "CREATE TRIGGER injected_failure BEFORE INSERT ON events WHEN NEW.event_kind='execution.intent' BEGIN SELECT RAISE(ABORT,'injected launch event failure'); END;",
    ] {
        let (_directory, mut store, database) = setup();
        let manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        store.apply(&manifest, 0).unwrap();
        database.execute_batch(trigger).unwrap();
        assert_eq!(
            store.claim_due(60_000_000).unwrap_err().code,
            "PERSISTENCE_UNAVAILABLE"
        );
        for table in ["occurrences", "executions", "attempts"] {
            let count: i64 = database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                count, 0,
                "partial {table} survived a failed launch transaction"
            );
        }
        database
            .execute_batch("DROP TRIGGER injected_failure")
            .unwrap();
        let launch = store.claim_due(60_000_000).unwrap().unwrap();
        assert_eq!(launch.due_at_us, 60_000_000);
        assert!(store.claim_due(60_000_000).unwrap().is_none());
        assert_eq!(store.history().unwrap().len(), 1);
    }
}

#[test]
fn writer_contention_cannot_consume_a_due_occurrence() {
    let (_directory, mut store, database) = setup();
    let manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    store.apply(&manifest, 0).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(
        store.claim_due(60_000_000).unwrap_err().code,
        "PERSISTENCE_UNAVAILABLE"
    );
    assert!(store.history().unwrap().is_empty());
    database.execute_batch("ROLLBACK").unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    assert_eq!(launch.due_at_us, 60_000_000);
    assert!(store.claim_due(60_000_000).unwrap().is_none());
}
