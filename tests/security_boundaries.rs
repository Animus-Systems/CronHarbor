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
                    0 => changed.spec.logs.max_disk_bytes = Some(1),
                    1 => changed.spec.logs.max_segments = Some(1),
                    _ => changed.spec.logs.retention_seconds = Some(1),
                }
                assert_eq!(
                    apply(&mut store, &changed, &credential, batch)
                        .unwrap_err()
                        .code,
                    "LOG_POLICY_MOVED",
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
                0 => changed.spec.logs.max_disk_bytes = Some(1),
                1 => changed.spec.logs.max_segments = Some(1),
                _ => changed.spec.logs.retention_seconds = Some(1),
            }
            assert_eq!(
                apply(&mut store, &changed, &credential, batch)
                    .unwrap_err()
                    .code,
                "LOG_POLICY_MOVED"
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
fn omitted_legacy_controls_cannot_reset_global_policy() {
    let (_directory, mut store, mut manifest) = setup();
    store
        .set_log_storage_policy(
            &cronharbor::contracts::LogStoragePolicyInput {
                max_disk_bytes: 536870912,
                max_segments: 4096,
                retention_seconds: 1209600,
            },
            1,
        )
        .unwrap();
    let credential = token(
        &mut store,
        Role::Editor,
        &["local"],
        &["workload.edit_safe_config", "executor.create_command"],
    );
    manifest.metadata.name = "default-budget".into();
    assert!(apply(&mut store, &manifest, &credential, false).is_ok());
    assert_eq!(
        store.log_storage_policy().unwrap().max_disk_bytes,
        536870912
    );
}
#[test]
fn even_global_admin_uses_explicit_policy_command() {
    let (_directory, mut store, mut manifest) = setup();
    let credential = token(
        &mut store,
        Role::Admin,
        &["*"],
        &["workload.edit_safe_config", "operations.manage"],
    );
    manifest.spec.logs.max_segments = Some(1);
    assert_eq!(
        apply(&mut store, &manifest, &credential, false)
            .unwrap_err()
            .code,
        "LOG_POLICY_MOVED"
    );
    let result = application::execute(
        &mut store,
        AppCommand::SetLogStorage {
            policy: cronharbor::contracts::LogStoragePolicyInput {
                max_disk_bytes: 268435456,
                max_segments: 2048,
                retention_seconds: 1209600,
            },
            expected_revision: 1,
            idempotency_key: "storage".into(),
        },
        Some(&credential),
    )
    .unwrap();
    assert_eq!(result["state"], "SUCCEEDED");
    assert_eq!(store.log_storage_policy().unwrap().max_segments, 2048);
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
    use sha2::{Digest, Sha256};
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.path().join("state.db");
    let database = rusqlite::Connection::open(&path).unwrap();
    for (version, sql) in [
        (1, include_str!("../migrations/0001_initial.sql")),
        (2, include_str!("../migrations/0002_runtime.sql")),
    ] {
        database.execute_batch(sql).unwrap();
        database
            .execute(
                "INSERT INTO schema_migrations(version,checksum,applied_at_us) VALUES(?1,?2,0)",
                rusqlite::params![version, format!("{:x}", Sha256::digest(sql.as_bytes()))],
            )
            .unwrap();
    }
    database.execute_batch("PRAGMA user_version=2; INSERT INTO audit_events(actor_id,action,target_id,result,request_id,created_at_us) VALUES('local-owner','workload.apply','workload','SUCCEEDED','trusted',0); WITH RECURSIVE n(v) AS(SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v<1500) INSERT INTO audit_events(actor_id,action,target_id,result,request_id,created_at_us) SELECT 'anonymous','auth.reject','token','DENIED','synthetic',v FROM n;").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
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
    assert_eq!(version, 5);
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
