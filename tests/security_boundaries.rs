use cronharbor::{
    application,
    config::Manifest,
    contracts::{AppCommand, Role},
    store::Store,
};
use std::os::unix::fs::PermissionsExt;

fn setup() -> (tempfile::TempDir, Store, Manifest) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.kind = "OneShotJob".into();
    manifest.spec.owner = "api".into();
    store
        .apply_scoped(&manifest, "API", None, "local-owner", 0)
        .unwrap();
    (directory, store, manifest)
}

fn token(store: &mut Store, role: Role, namespaces: &[&str], capabilities: &[&str]) -> String {
    application::execute(
        store,
        AppCommand::CreateToken {
            role,
            namespaces: namespaces.iter().map(|s| (*s).into()).collect(),
            capabilities: capabilities.iter().map(|s| (*s).into()).collect(),
            ttl_seconds: 3600,
        },
        None,
    )
    .unwrap()["token"]
        .as_str()
        .unwrap()
        .into()
}

fn apply(
    store: &mut Store,
    manifest: &Manifest,
    credential: &str,
    batch: bool,
) -> cronharbor::Result<serde_json::Value> {
    let revision = store
        .manifest_for(&format!(
            "{}/{}",
            manifest.metadata.namespace, manifest.metadata.name
        ))
        .ok()
        .map(|r| r.1);
    let definition = serde_json::to_string(manifest).unwrap();
    let command = if batch {
        AppCommand::ApplyBatch {
            definitions: vec![definition],
            expected_revisions: vec![revision],
            source_kind: "API".into(),
            idempotency_key: uuid::Uuid::new_v4().to_string(),
        }
    } else {
        AppCommand::Apply {
            definition,
            expected_revision: revision,
            source_kind: "API".into(),
            idempotency_key: uuid::Uuid::new_v4().to_string(),
        }
    };
    application::execute(store, command, Some(credential))
}

#[test]
fn installation_log_policy_requires_global_administration_for_single_and_batch_apply() {
    for batch in [false, true] {
        for (role, namespaces, grants) in [
            (
                Role::Editor,
                vec!["local"],
                vec!["workload.edit_safe_config"],
            ),
            (Role::Editor, vec!["*"], vec!["*"]),
            (Role::Admin, vec!["local"], vec!["*"]),
            (Role::Admin, vec!["*"], vec!["workload.edit_safe_config"]),
        ] {
            let (_directory, mut store, manifest) = setup();
            let credential = token(&mut store, role, &namespaces, &grants);
            let revision = store.manifest_for("local/hello").unwrap().1;
            for field in 0..3 {
                let mut changed = manifest.clone();
                match field {
                    0 => changed.spec.logs.max_disk_bytes = 1,
                    1 => changed.spec.logs.max_segments = 1,
                    _ => changed.spec.logs.retention_seconds = 1,
                }
                assert_eq!(
                    apply(&mut store, &changed, &credential, batch)
                        .unwrap_err()
                        .code,
                    "NOT_AUTHORIZED",
                    "batch={batch}, field={field}"
                );
                assert_eq!(store.manifest_for("local/hello").unwrap().1, revision);
            }
            let mut safe = manifest;
            safe.spec.timeout_seconds += 1;
            safe.spec.logs.max_run_bytes -= 1;
            assert!(apply(&mut store, &safe, &credential, batch).is_ok());
        }
    }
}

#[test]
fn scoped_creation_cannot_reduce_existing_global_limits() {
    for batch in [false, true] {
        let (_directory, mut store, mut manifest) = setup();
        let credential = token(
            &mut store,
            Role::Editor,
            &["local"],
            &["workload.edit_safe_config", "executor.create_command"],
        );
        manifest.metadata.name = "new-job".into();
        assert!(apply(&mut store, &manifest, &credential, batch).is_ok());
        for field in 0..3 {
            let mut changed = manifest.clone();
            changed.metadata.name = format!("restricted-{field}");
            match field {
                0 => changed.spec.logs.max_disk_bytes = 1,
                1 => changed.spec.logs.max_segments = 1,
                _ => changed.spec.logs.retention_seconds = 1,
            }
            assert_eq!(
                apply(&mut store, &changed, &credential, batch)
                    .unwrap_err()
                    .code,
                "NOT_AUTHORIZED"
            );
            assert!(
                store
                    .manifest_for(&format!("local/restricted-{field}"))
                    .is_err()
            );
        }
    }
}

#[test]
fn default_creation_cannot_lower_an_existing_larger_disk_budget() {
    let (_directory, mut store, mut manifest) = setup();
    let revision = store.manifest_for("local/hello").unwrap().1;
    manifest.spec.logs.max_disk_bytes *= 2;
    store
        .apply_scoped(&manifest, "API", Some(&revision), "local-owner", 1)
        .unwrap();
    let credential = token(
        &mut store,
        Role::Editor,
        &["local"],
        &["workload.edit_safe_config", "executor.create_command"],
    );
    manifest.metadata.name = "default-budget".into();
    manifest.spec.logs.max_disk_bytes /= 2;
    assert_eq!(
        apply(&mut store, &manifest, &credential, false)
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
}

#[test]
fn instance_admin_and_local_file_apply_can_manage_retention() {
    let (_directory, mut store, mut manifest) = setup();
    let credential = token(
        &mut store,
        Role::Admin,
        &["*"],
        &["workload.edit_safe_config", "operations.manage"],
    );
    manifest.spec.logs.max_segments = 1;
    assert!(apply(&mut store, &manifest, &credential, false).is_ok());
    manifest.spec.logs.retention_seconds = 1;
    assert!(apply(&mut store, &manifest, &credential, true).is_ok());
    manifest.metadata.name = "file".into();
    manifest.spec.owner = "file".into();
    assert!(store.apply(&manifest, 0).is_ok());
}

#[test]
fn rejected_authentication_is_coalesced_and_keeps_bounded_history() {
    let (directory, mut store, _) = setup();
    let credential = token(&mut store, Role::Viewer, &["local"], &[]);
    let started = std::time::Instant::now();
    for index in 0..300 {
        let invalid = if index % 2 == 0 {
            "x".into()
        } else {
            format!("invented-credential-{index}")
        };
        assert_eq!(
            application::execute(&mut store, AppCommand::Identity, Some(&invalid))
                .unwrap_err()
                .code,
            "TOKEN_INVALID"
        );
    }
    let database = rusqlite::Connection::open(directory.path().join("state.db")).unwrap();
    let count: i64 = database
        .query_row(
            "SELECT count(*) FROM audit_events WHERE action='auth.reject'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        u64::try_from(count).unwrap() <= started.elapsed().as_secs() + 1,
        "one durable sample per second, including short and rotating invalid tokens; got {count}"
    );
    assert!(application::execute(&mut store, AppCommand::Status, Some(&credential)).is_ok());
    let trusted: i64 = database
        .query_row(
            "SELECT count(*) FROM audit_events WHERE action!='auth.reject'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    database.execute_batch("WITH RECURSIVE n(v) AS(SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v<1500) INSERT INTO audit_events(actor_id,action,target_id,result,request_id,created_at_us) SELECT 'anonymous','auth.reject','token','DENIED','synthetic',v FROM n;").unwrap();
    drop(store);
    let mut store = Store::open(directory.path()).unwrap();
    assert_eq!(
        application::execute(
            &mut store,
            AppCommand::Identity,
            Some("another-invalid-credential")
        )
        .unwrap_err()
        .code,
        "TOKEN_INVALID"
    );
    let count: i64 = database
        .query_row(
            "SELECT count(*) FROM audit_events WHERE action='auth.reject'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1000);
    let after: i64 = database
        .query_row(
            "SELECT count(*) FROM audit_events WHERE action!='auth.reject'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        after >= trusted,
        "trusted audit history must never be evicted by rejection traffic"
    );
}

#[test]
fn failed_rejection_audit_remains_fail_closed_and_can_be_retried() {
    let (directory, mut store, _) = setup();
    let database = rusqlite::Connection::open(directory.path().join("state.db")).unwrap();
    database.execute_batch("CREATE TRIGGER reject_auth_audit BEFORE INSERT ON audit_events WHEN NEW.action='auth.reject' BEGIN SELECT RAISE(ABORT,'injected audit failure'); END;").unwrap();
    assert_eq!(
        application::execute(&mut store, AppCommand::Identity, Some("invalid-credential"))
            .unwrap_err()
            .code,
        "PERSISTENCE_UNAVAILABLE"
    );
    database
        .execute_batch("DROP TRIGGER reject_auth_audit")
        .unwrap();
    assert_eq!(
        application::execute(&mut store, AppCommand::Identity, Some("invalid-credential"))
            .unwrap_err()
            .code,
        "TOKEN_INVALID"
    );
    let count: i64 = database
        .query_row(
            "SELECT count(*) FROM audit_events WHERE action='auth.reject'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn version_two_upgrade_bounds_old_rejections_without_deleting_trusted_audits() {
    let (directory, store, _) = setup();
    drop(store);
    let database = rusqlite::Connection::open(directory.path().join("state.db")).unwrap();
    database.execute_batch("DROP INDEX IF EXISTS audit_auth_rejections; DELETE FROM schema_migrations WHERE version=3; PRAGMA user_version=2; WITH RECURSIVE n(v) AS(SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v<1500) INSERT INTO audit_events(actor_id,action,target_id,result,request_id,created_at_us) SELECT 'anonymous','auth.reject','token','DENIED','synthetic',v FROM n;").unwrap();
    let trusted: i64 = database
        .query_row(
            "SELECT count(*) FROM audit_events WHERE action!='auth.reject'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let store = Store::open(directory.path()).unwrap();
    let version: i64 = database
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let rejects: i64 = database
        .query_row(
            "SELECT count(*) FROM audit_events WHERE action='auth.reject'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rejects, 1000);
    let after: i64 = database
        .query_row(
            "SELECT count(*) FROM audit_events WHERE action!='auth.reject'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(after >= trusted);
    let backup = std::fs::read_dir(directory.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("pre-migration-3-")
        });
    assert!(backup);
    drop(store);
    database
        .execute(
            "UPDATE schema_migrations SET checksum='tampered' WHERE version=3",
            [],
        )
        .unwrap();
    assert_eq!(
        Store::open(directory.path()).err().unwrap().code,
        "SCHEMA_UNSUPPORTED"
    );
}
