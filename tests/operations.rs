use cronharbor::{config::Manifest, ops, store::Store};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

fn snapshot(directory: &Path) -> String {
    let mut store = Store::open(directory).unwrap();
    store
        .apply(
            &Manifest::parse(include_str!("../examples/hello.yaml")).unwrap(),
            0,
        )
        .unwrap();
    store.backup("nightly").unwrap()["path"]
        .as_str()
        .unwrap()
        .into()
}

fn resign_snapshot(path: &Path, version: Option<u32>) {
    let sidecar = path.with_file_name(format!(
        "{}.manifest.json",
        path.file_name().unwrap().to_str().unwrap()
    ));
    let mut manifest: ops::BackupManifest =
        serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
    manifest.database_sha256 = format!("{:x}", Sha256::digest(fs::read(path).unwrap()));
    if let Some(version) = version {
        manifest.schema_version = version;
    }
    fs::write(sidecar, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

#[test]
fn backup_restore_is_verified_and_always_quarantined() {
    let temp = tempfile::tempdir().unwrap();
    let backup = snapshot(&temp.path().join("source"));
    let target = temp.path().join("restored");
    ops::restore(&target, Path::new(&backup), None, "restore drill", false).unwrap();
    assert!(target.join("restore-required.json").exists());
    let store = Store::open(&target).unwrap();
    assert_eq!(store.status(false).unwrap().workloads, 1);
    assert!(store.paused().unwrap());
    drop(store);
    assert!(ops::restore(&target, Path::new(&backup), None, "restore drill", false).is_err());
}

#[test]
fn version_two_snapshot_restores_then_upgrades_in_quarantine() {
    let temp = tempfile::tempdir().unwrap();
    let backup = snapshot(&temp.path().join("source"));
    // Build a genuine schema-2 database; lowering user_version leaves newer tables behind.
    let old_path = temp.path().join("schema-two.db");
    let database = Connection::open(&old_path).unwrap();
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
    database
        .execute("ATTACH DATABASE ?1 AS source", [&backup])
        .unwrap();
    database.execute_batch("BEGIN; INSERT INTO workloads SELECT * FROM source.workloads; INSERT INTO workload_revisions SELECT * FROM source.workload_revisions; INSERT INTO schedules SELECT * FROM source.schedules; COMMIT; DETACH DATABASE source; PRAGMA user_version=2;").unwrap();
    drop(database);
    fs::copy(old_path, &backup).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o600)).unwrap();
    }
    resign_snapshot(Path::new(&backup), Some(2));
    let target = temp.path().join("restored");
    ops::restore(
        &target,
        Path::new(&backup),
        None,
        "version two restore drill",
        false,
    )
    .unwrap();
    let store = Store::open(&target).unwrap();
    assert!(store.paused().unwrap());
    assert!(store.restore_required().unwrap());
    let database = Connection::open(target.join("state.db")).unwrap();
    let version: u32 = database
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5);
    assert_eq!(store.status(false).unwrap().workloads, 1);
}

#[test]
fn rejects_modified_snapshots_before_replacing_state() {
    let temp = tempfile::tempdir().unwrap();
    let backup = snapshot(&temp.path().join("source"));
    let modified = Connection::open(&backup).unwrap();
    modified
        .execute_batch("CREATE TABLE changed(value TEXT);")
        .unwrap();
    drop(modified);
    let target = temp.path().join("target");
    assert!(ops::restore(&target, Path::new(&backup), None, "restore drill", false).is_err());
    assert!(!target.join("state.db").exists());
}

#[test]
fn incompatible_schema_or_checksum_preserves_existing_state() {
    for future_version in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let backup = snapshot(&temp.path().join("source"));
        let modified = Connection::open(&backup).unwrap();
        if future_version {
            modified.execute_batch("PRAGMA user_version=99").unwrap();
        } else {
            modified
                .execute(
                    "UPDATE schema_migrations SET checksum='different-binary' WHERE version=2",
                    [],
                )
                .unwrap();
        }
        drop(modified);
        resign_snapshot(Path::new(&backup), future_version.then_some(99));
        let target = temp.path().join("target");
        snapshot(&target);
        let before = fs::read(target.join("state.db")).unwrap();
        assert!(
            ops::restore(
                &target,
                Path::new(&backup),
                None,
                "downgrade restore drill",
                true
            )
            .is_err()
        );
        assert_eq!(fs::read(target.join("state.db")).unwrap(), before);
        assert!(!target.join("restore-required.json").exists());
        assert_eq!(
            Store::open(&target)
                .unwrap()
                .status(false)
                .unwrap()
                .workloads,
            1
        );
    }
}

#[test]
fn restore_can_stage_a_snapshot_inside_the_replacement_directory() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target");
    let backup = snapshot(&target);
    let donor = target.join("state.db");
    fs::copy(&backup, &donor).unwrap();
    fs::copy(
        format!("{backup}.manifest.json"),
        target.join("state.db.manifest.json"),
    )
    .unwrap();
    ops::restore(
        &target,
        &donor,
        None,
        "in-place verified snapshot drill",
        true,
    )
    .unwrap();
    assert_eq!(
        Store::open(&target)
            .unwrap()
            .status(false)
            .unwrap()
            .workloads,
        1
    );
}

#[test]
fn restore_rejects_untrusted_manifest_paths_without_blocking_or_touching_state() {
    use std::{
        os::unix::fs::{PermissionsExt, symlink},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    for kind in ["fifo", "symlink", "hardlink", "world_readable"] {
        let temporary = tempfile::tempdir().unwrap();
        let backup = snapshot(&temporary.path().join("source"));
        let sidecar = format!("{backup}.manifest.json");
        let original = format!("{sidecar}.original");
        fs::rename(&sidecar, &original).unwrap();
        match kind {
            "fifo" => assert!(
                Command::new("mkfifo")
                    .args(["-m", "600", &sidecar])
                    .status()
                    .unwrap()
                    .success()
            ),
            "symlink" => symlink(&original, &sidecar).unwrap(),
            "hardlink" => fs::hard_link(&original, &sidecar).unwrap(),
            "world_readable" => {
                fs::copy(&original, &sidecar).unwrap();
                fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o644)).unwrap();
            }
            _ => unreachable!(),
        }
        let target = temporary.path().join("target");
        let mut child = Command::new(env!("CARGO_BIN_EXE_cronharbor"))
            .arg("--state-dir")
            .arg(&target)
            .arg("restore")
            .arg(&backup)
            .args(["--evidence", "untrusted manifest regression"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("restore blocked on {kind} manifest instead of rejecting it");
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(!status.success(), "restore accepted {kind} manifest");
        assert!(!target.join("state.db").exists());
        assert!(!target.join("restore-installing").exists());
        assert!(!target.join("restore-required.json").exists());
    }
}

#[test]
fn restore_never_replaces_a_database_held_by_an_active_owner() {
    let temporary = tempfile::tempdir().unwrap();
    let backup = snapshot(&temporary.path().join("source"));
    let target = temporary.path().join("target");
    let mut owner = Store::open(&target).unwrap();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.metadata.name = "retained-owner".into();
    owner.apply(&manifest, 0).unwrap();
    let error = ops::restore(
        &target,
        Path::new(&backup),
        None,
        "active owner restore drill",
        true,
    )
    .unwrap_err();
    assert_eq!(error.code, "OWNER_UNAVAILABLE");
    assert!(owner.manifest_for("local/retained-owner").is_ok());
    assert!(!target.join("restore-installing").exists());
    assert!(!target.join("restore-required.json").exists());
}
