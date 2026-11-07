use cronharbor::{config::Manifest, store::Store};

fn manifest() -> Manifest {
    Manifest::parse(include_str!("../examples/hello.yaml")).unwrap()
}

#[test]
fn cron_uses_dom_dow_or_and_both_sunday_numbers() {
    use chrono::DateTime;
    let mut job = manifest();
    job.spec.schedule.expression = "0 0 1 * 1".to_owned();
    let from = DateTime::parse_from_rfc3339("2026-11-03T00:00:00Z")
        .unwrap()
        .timestamp_micros();
    let next = DateTime::parse_from_rfc3339("2026-11-09T00:00:00Z")
        .unwrap()
        .timestamp_micros();
    assert_eq!(job.next_after(from).unwrap(), next);
    job.spec.schedule.expression = "0 0 * * 0".to_owned();
    let sunday = job.next_after(from).unwrap();
    job.spec.schedule.expression = "0 0 * * 7".to_owned();
    assert_eq!(job.next_after(from).unwrap(), sunday);
}

#[test]
fn rejects_state_directory_and_database_symlinks() {
    use std::os::unix::fs::symlink;
    let dir = private_tempdir();
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let alias = dir.path().join("alias");
    symlink(&target, &alias).unwrap();
    assert!(Store::open(&alias).is_err());
    symlink("/etc/passwd", dir.path().join("state.db")).unwrap();
    assert!(Store::open(dir.path()).is_err());
}

#[test]
fn rejects_manifests_that_expand_beyond_the_stored_size_limit() {
    let mut job = manifest();
    job.spec.execution.args = vec!["\\".repeat(8000); 5];
    assert!(job.validate().is_err());
    assert!(job.canonical().is_err());
}

#[test]
fn an_accepted_canonical_manifest_at_the_size_limit_remains_dispatchable() {
    let mut job = manifest();
    job.spec.execution.args = vec!["a".repeat(8000); 8];
    job.spec.execution.args.push(String::new());
    let base = serde_json::to_vec(&job).unwrap().len();
    job.spec
        .execution
        .args
        .last_mut()
        .unwrap()
        .push_str(&"a".repeat(65_536 - base));
    assert_eq!(job.canonical().unwrap().0.len(), 65_536);
    let dir = private_tempdir();
    let mut store = Store::open(dir.path()).unwrap();
    store.apply(&job, 0).unwrap();
    assert!(store.claim_due(60_000_000).unwrap().is_some());
}

#[test]
fn validates_the_supported_manifest_and_rejects_unknown_or_unsafe_options() {
    let source = include_str!("../examples/hello.yaml");
    assert!(Manifest::parse(source).is_ok());
    assert!(Manifest::parse(&source.replace("owner: file", "owner: api")).is_ok());
    assert!(Manifest::parse(&source.replace("timezone: UTC", "timezone: Europe/Madrid")).is_ok());
    for (from, to) in [
        ("adapter: process", "adapter: process\n    shell: true"),
        ("timezone: UTC", "timezone: Invalid/Zone"),
        ("maxAttempts: 1", "maxAttempts: 2"),
        ("command: /bin/echo", "command: echo"),
        ("expression: \"* * * * *\"", "expression: \"* * * * * *\""),
        ("latenessGraceSeconds: 60", "latenessGraceSeconds: 0"),
    ] {
        assert!(Manifest::parse(&source.replace(from, to)).is_err(), "{to}");
    }
    assert!(Manifest::parse(&"x".repeat(65537)).is_err());
}

#[test]
fn enforces_a_single_owner_and_preserves_identical_revision_on_reapply() {
    let dir = private_tempdir();
    let mut store = Store::open(dir.path()).unwrap();
    assert!(Store::open(dir.path()).is_err());
    let first = store.apply(&manifest(), 0).unwrap();
    let second = store.apply(&manifest(), 1).unwrap();
    assert_eq!(first, second);
    drop(store);
    assert!(Store::open(dir.path()).is_ok());
}

#[test]
fn deduplicates_due_instants_and_counts_unknown_against_concurrency() {
    let dir = private_tempdir();
    let mut store = Store::open(dir.path()).unwrap();
    store.apply(&manifest(), 0).unwrap();
    assert!(store.claim_due(59_999_999).unwrap().is_none());
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    assert!(store.claim_due(60_000_000).unwrap().is_none());
    store
        .start_requested(&launch.attempt_id, 60_000_000)
        .unwrap();
    drop(store);
    let mut store = Store::open(dir.path()).unwrap();
    let history = store.history().unwrap();
    assert_eq!(history[0].state, "UNKNOWN");
    assert!(store.claim_due(120_000_000).unwrap().is_none());
    assert_eq!(store.history().unwrap().len(), 1);
}

#[test]
fn grace_boundary_runs_but_downtime_does_not_replay_a_backlog() {
    let dir = private_tempdir();
    let mut store = Store::open(dir.path()).unwrap();
    store.apply(&manifest(), 0).unwrap();
    assert!(store.claim_due(120_000_000).unwrap().is_some());

    let other = private_tempdir();
    let mut store = Store::open(other.path()).unwrap();
    store.apply(&manifest(), 0).unwrap();
    let now = 30 * 24 * 3600 * 1_000_000;
    assert!(store.claim_due(now).unwrap().is_none());
    let db = rusqlite::Connection::open(other.path().join("state.db")).unwrap();
    let payload: String = db
        .query_row(
            "SELECT payload_json FROM events WHERE event_kind='schedule.misfire_skipped'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let range: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(range["fromDueUtcMicroseconds"], 60_000_000);
    assert!(range["throughUtcMicroseconds"].as_i64().unwrap() >= now - 60_000_001);
    let launch = store.claim_due(now).unwrap().unwrap();
    assert!(launch.due_at_us >= now - 60_000_000);
    assert_eq!(store.history().unwrap().len(), 1);
}

#[test]
fn an_unlaunched_intent_is_cancelled_without_replay() {
    let dir = private_tempdir();
    let mut store = Store::open(dir.path()).unwrap();
    store.apply(&manifest(), 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.history().unwrap()[0].state, "CANCELLED");
    assert_eq!(store.history().unwrap()[0].attempt_id, launch.attempt_id);
}

fn private_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}
