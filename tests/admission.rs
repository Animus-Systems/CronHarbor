use cronharbor::{
    config::Manifest,
    store::{Launch, Store},
};
use std::os::unix::fs::PermissionsExt;

fn private_directory() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn scheduled(name: &str, limit: u32) -> Manifest {
    let mut job = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    job.metadata.name = name.into();
    job.spec.concurrency.mode = if limit == 1 { "forbid" } else { "allow" }.into();
    job.spec.concurrency.max_active = limit;
    job
}

fn claim_ready(store: &mut Store, now: i64) -> Vec<Launch> {
    let mut launches = Vec::new();
    // A tick can consume a skipped occurrence without returning a launch.
    for _ in 0..32 {
        if let Some(launch) = store.claim_due(now).unwrap() {
            store.start_requested(&launch.attempt_id, now).unwrap();
            launches.push(launch);
        }
    }
    launches
}

#[test]
fn distinct_workloads_can_exceed_four_active_executions() {
    let directory = private_directory();
    let mut store = Store::open(directory.path()).unwrap();
    for index in 0..9 {
        store
            .apply(&scheduled(&format!("worker-{index}"), 1), 0)
            .unwrap();
    }
    let launches = claim_ready(&mut store, 60_000_000);
    assert_eq!(
        launches.len(),
        9,
        "unrelated workloads must not share a four-slot cap"
    );
}

#[test]
fn per_job_limits_remain_independent_when_total_activity_exceeds_four() {
    let directory = private_directory();
    let mut store = Store::open(directory.path()).unwrap();
    for (name, limit) in [("single", 1), ("double", 2), ("triple", 3)] {
        store.apply(&scheduled(name, limit), 0).unwrap();
    }
    let mut launches = Vec::new();
    for now in [60_000_000, 120_000_000, 180_000_000] {
        launches.extend(claim_ready(&mut store, now));
    }
    for (name, count) in [("single", 1), ("double", 2), ("triple", 3)] {
        assert_eq!(
            launches
                .iter()
                .filter(|launch| launch.manifest.metadata.name == name)
                .count(),
            count
        );
    }
    assert_eq!(launches.len(), 6);
}

#[test]
fn an_unknown_execution_reserves_its_job_without_blocking_unrelated_jobs() {
    let directory = private_directory();
    let path = directory.path();
    let mut store = Store::open(path).unwrap();
    store.apply(&scheduled("uncertain", 1), 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store
        .start_requested(&launch.attempt_id, 60_000_000)
        .unwrap();
    drop(store);
    let mut store = Store::open(path).unwrap();
    assert_eq!(store.status(false).unwrap().unknown, 1);
    for index in 0..7 {
        store
            .apply(&scheduled(&format!("independent-{index}"), 1), 60_000_000)
            .unwrap();
    }
    let launches = claim_ready(&mut store, 120_000_000);
    for next in &launches {
        assert_ne!(next.manifest.metadata.name, "uncertain");
    }
    assert_eq!(launches.len(), 7);
    assert_eq!(store.status(false).unwrap().unknown, 1);
}

fn policy(path: &std::path::Path, pause: u64) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path.join("admission-policy.json"))
        .unwrap();
    write!(
        file,
        "{}",
        serde_json::json!({
            "maxDatabaseBytes": 2_097_152, "warningBytes": 327_680,
            "admissionPauseBytes": pause, "maxWalBytes": 65_536
        })
    )
    .unwrap();
}

fn database(path: &std::path::Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path.join("state.db")).unwrap()
}

#[test]
fn invalid_or_insecure_admission_policy_refuses_startup() {
    for contents in ["{}", "{\"maxDatabaseBytes\":-1}", &" ".repeat(4097)] {
        let directory = private_directory();
        policy(directory.path(), 524_288);
        std::fs::write(directory.path().join("admission-policy.json"), contents).unwrap();
        assert!(
            Store::open(directory.path()).is_err(),
            "invalid policy was accepted"
        );
    }
    let directory = private_directory();
    policy(directory.path(), 524_288);
    let path = directory.path().join("admission-policy.json");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        Store::open(directory.path()).is_err(),
        "public policy was accepted"
    );
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("missing", &path).unwrap();
    assert!(
        Store::open(directory.path()).is_err(),
        "policy symlink was accepted"
    );
}

#[test]
fn database_pressure_stops_allocations_preserving_replay_and_settlement() {
    use cronharbor::{
        application::execute,
        contracts::AppCommand,
        process::{EndState, Outcome},
    };
    let directory = private_directory();
    let path = directory.path();
    policy(path, 524_288);
    let mut store = Store::open(path).unwrap();
    let job = scheduled("pressure", 2);
    store.apply(&job, 0).unwrap();
    let id = store.manifest_for("local/pressure").unwrap().0;
    let run = AppCommand::Run {
        id: id.clone(),
        idempotency_key: "before-pressure".into(),
    };
    let accepted = execute(&mut store, run.clone(), None).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store
        .start_requested(&launch.attempt_id, 60_000_000)
        .unwrap();
    let db = database(path);
    db.execute_batch("CREATE TABLE admission_fixture(payload BLOB); INSERT INTO admission_fixture VALUES(zeroblob(524288));").unwrap();
    let before: i64 = db
        .query_row("SELECT count(*) FROM occurrences", [], |row| row.get(0))
        .unwrap();
    for now in [120_000_000, 180_000_000, 240_000_000] {
        assert!(
            store.claim_due(now).unwrap().is_none(),
            "storage pressure launched new work"
        );
    }
    assert_eq!(
        db.query_row("SELECT count(*) FROM occurrences", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert!(store.apply(&scheduled("rejected", 1), 0).is_err());
    let before_ops: i64 = db
        .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
        .unwrap();
    for index in 0..3 {
        let error = execute(
            &mut store,
            AppCommand::Run {
                id: id.clone(),
                idempotency_key: format!("pressure-{index}"),
            },
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "STORAGE_PRESSURE");
    }
    assert_eq!(
        db.query_row("SELECT count(*) FROM operations", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        before_ops
    );
    assert_eq!(
        execute(&mut store, run, None).unwrap()["id"],
        accepted["id"]
    );
    let status = execute(&mut store, AppCommand::Status, None).unwrap();
    assert_eq!(status["admission"]["paused"], true);
    execute(
        &mut store,
        AppCommand::Cancel {
            execution_id: launch.execution_id.clone(),
            idempotency_key: "cancel-under-pressure".into(),
        },
        None,
    )
    .unwrap();
    assert!(store.cancel_requested(&launch.attempt_id).unwrap());
    store
        .finish(
            &launch,
            &Outcome {
                state: EndState::Cancelled,
                error_code: None,
                exit_code: None,
                log_bytes: 0,
                log_truncated: false,
            },
            120_000_000,
        )
        .unwrap();
    assert_eq!(store.history().unwrap()[0].state, "CANCELLED");
}

#[test]
fn pinned_wal_blocks_new_work_then_recovers_after_checkpoint() {
    let directory = private_directory();
    let path = directory.path();
    policy(path, 1_572_864);
    let mut store = Store::open(path).unwrap();
    store.apply(&scheduled("wal", 1), 0).unwrap();
    let db = database(path);
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); CREATE TABLE admission_fixture(payload BLOB); INSERT INTO admission_fixture VALUES(zeroblob(4096)); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    let reader = database(path);
    reader
        .execute_batch("BEGIN; SELECT * FROM admission_fixture;")
        .unwrap();
    db.execute_batch("PRAGMA wal_autocheckpoint=0;").unwrap();
    for _ in 0..20 {
        db.execute("UPDATE admission_fixture SET payload=randomblob(4096)", [])
            .unwrap();
    }
    assert!(std::fs::metadata(path.join("state.db-wal")).unwrap().len() > 65_536);
    assert!(
        store.claim_due(60_000_000).unwrap().is_none(),
        "pinned WAL must block new starts"
    );
    reader.execute_batch("ROLLBACK").unwrap();
    store.ensure_admission().unwrap();
    assert!(std::fs::metadata(path.join("state.db-wal")).unwrap().len() < 65_536);
    // The newly admitted transaction may itself cross this small test waterline.
    assert!(
        store.claim_due(60_000_000).unwrap().is_some(),
        "released WAL must recover admission"
    );
}

#[test]
fn database_page_ceiling_leaves_existing_history_intact() {
    let directory = private_directory();
    let path = directory.path();
    policy(path, 1_572_864);
    let mut store = Store::open(path).unwrap();
    store.apply(&scheduled("retained", 1), 0).unwrap();
    // This writes through the same guarded daemon connection, beyond its hard page budget.
    let error = store
        .set_setting("too-large", &"x".repeat(3_145_728))
        .unwrap_err();
    assert_eq!(error.code, "PERSISTENCE_UNAVAILABLE");
    assert!(store.setting("too-large").unwrap().is_none());
    assert!(store.manifest_for("local/retained").is_ok());
}

#[test]
fn uncertainty_can_be_reconciled_using_reserved_space() {
    use cronharbor::{application::execute, contracts::AppCommand};
    let directory = private_directory();
    let path = directory.path();
    policy(path, 524_288);
    let mut store = Store::open(path).unwrap();
    store.apply(&scheduled("uncertain-pressure", 1), 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store
        .start_requested(&launch.attempt_id, 60_000_000)
        .unwrap();
    drop(store);
    let mut store = Store::open(path).unwrap();
    database(path).execute_batch("CREATE TABLE admission_fixture(payload BLOB); INSERT INTO admission_fixture VALUES(zeroblob(524288));").unwrap();
    assert_eq!(store.status(false).unwrap().unknown, 1);
    execute(
        &mut store,
        AppCommand::Reconcile {
            execution_id: launch.execution_id,
            state: "SUCCEEDED".into(),
            evidence: "Disposable fixture: verified downstream completion".into(),
            idempotency_key: "reconcile-under-pressure".into(),
        },
        None,
    )
    .unwrap();
    assert_eq!(store.status(false).unwrap().unknown, 0);
    assert_eq!(store.history().unwrap()[0].state, "SUCCEEDED");
}

#[test]
fn read_only_monitor_can_check_policy_limits_without_log_or_control_authority() {
    use cronharbor::{
        application::execute,
        contracts::{AppCommand, Role},
    };
    let directory = private_directory();
    let mut store = Store::open(directory.path()).unwrap();
    let credential = execute(
        &mut store,
        AppCommand::CreateToken {
            role: Role::Observe,
            namespaces: vec!["*".into()],
            capabilities: vec!["instance.read".into()],
            ttl_seconds: 60,
        },
        None,
    )
    .unwrap();
    let token = credential["token"].as_str().unwrap();
    let status = execute(&mut store, AppCommand::Status, Some(token)).unwrap();
    assert_eq!(status["log_policy"]["max_disk_bytes"], 268435456);
    assert_eq!(status["log_policy"]["max_segments"], 1024);
    assert_eq!(status["log_policy"]["retention_seconds"], 604800);
    assert!(status["log_policy"].get("streams").is_none());
    assert!(status["log_policy"].get("files").is_none());
    assert_eq!(
        execute(&mut store, AppCommand::LogStorage, Some(token))
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
}
