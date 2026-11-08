use cronharbor::{application, config::Manifest, contracts::AppCommand, store::Store};
use std::os::unix::fs::PermissionsExt;

fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = Store::open(dir.path()).unwrap();
    (dir, store)
}

#[test]
fn rotation_is_explicit_and_requires_two_complete_segments() {
    let mut value =
        serde_json::to_value(Manifest::parse(include_str!("../examples/hello.yaml")).unwrap())
            .unwrap();
    assert!(
        Manifest::parse(&serde_json::to_string(&value).unwrap())
            .unwrap()
            .spec
            .logs
            .rotation
            .is_none()
    );
    value["spec"]["logs"]["rotation"] = serde_json::json!({"segmentBytes":65536,"maxSegments":2});
    value["spec"]["logs"]["maxRunBytes"] = serde_json::json!(65536);
    assert_eq!(
        Manifest::parse(&serde_json::to_string(&value).unwrap())
            .unwrap_err()
            .code,
        "CONFIG_INVALID"
    );
}

#[test]
fn policy_is_seeded_once_and_new_legacy_controls_are_rejected() {
    let (_dir, mut store) = fixture();
    let initial = application::execute(&mut store, AppCommand::LogStorage, None).unwrap();
    assert_eq!(initial["policy"]["max_segments"], 1024);
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.logs.max_disk_bytes = Some(65536);
    assert_eq!(
        store.apply(&manifest, 0).unwrap_err().code,
        "LOG_POLICY_MOVED"
    );
    manifest.spec.logs.max_disk_bytes = None;
    manifest.spec.logs.max_segments = None;
    manifest.spec.logs.retention_seconds = None;
    store.apply(&manifest, 0).unwrap();
    assert_eq!(
        application::execute(&mut store, AppCommand::LogStorage, None).unwrap()["policy"],
        initial["policy"]
    );
}

fn launch(store: &mut Store, rotation: bool) -> cronharbor::store::Launch {
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.logs.max_run_bytes = if rotation { 196608 } else { 4_194_304 };
    manifest.spec.logs.rotation = rotation.then_some(cronharbor::config::LogRotation {
        segment_bytes: 65536,
        max_segments: 3,
    });
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store
        .start_requested(&launch.attempt_id, 60_000_000)
        .unwrap();
    store.running(&launch, 1, 60_000_000).unwrap();
    store.prepare_log_capture(&launch).unwrap();
    launch
}

#[test]
fn late_reader_after_prefix_eviction_gets_cursor_expired() {
    let (_dir, mut store) = fixture();
    let launch = launch(&mut store, true);
    let input = (0..524288)
        .map(|i| b'a' + u8::try_from(i % 26).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(store.append_log(&launch, &input).unwrap(), input.len());
    let info = store.log_info(&launch.attempt_id).unwrap();
    assert_eq!(info.written_end_offset, 524288);
    assert_eq!(info.retained_from_offset, 327680);
    assert_eq!(info.retained_bytes, 196608);
    assert_eq!(
        store
            .read_log(&launch.attempt_id, 0, 65536)
            .unwrap_err()
            .code,
        "LOG_CURSOR_EXPIRED"
    );
    assert_eq!(
        store
            .read_log(&launch.attempt_id, 524289, 65536)
            .unwrap_err()
            .code,
        "INPUT_INVALID"
    );
    assert_eq!(
        store
            .read_log(&launch.attempt_id, 524288, 65536)
            .unwrap()
            .text,
        ""
    );
    let mut read = Vec::new();
    let mut offset = info.retained_from_offset;
    while offset < info.written_end_offset {
        let log = store.read_log(&launch.attempt_id, offset, 10001).unwrap();
        read.extend_from_slice(log.text.as_bytes());
        offset = log.next_offset;
    }
    assert_eq!(read, &input[327680..]);
    assert_eq!(store.history().unwrap().len(), 1);
    assert_eq!(
        store.log_path(&launch.attempt_id).unwrap_err().code,
        "LOG_UNAVAILABLE"
    );
}

#[test]
fn queued_append_cannot_cross_unknown_freeze() {
    use cronharbor::process::{EndState, Outcome, RunningProcess};
    use std::{thread, time::Duration};
    let (dir, mut store) = fixture();
    let input = dir.path().join("input");
    std::fs::write(&input, vec![b'x'; 3 * 1024 * 1024]).unwrap();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.command = "/bin/cat".into();
    manifest.spec.execution.args = vec![input.to_string_lossy().into_owned()];
    manifest.spec.logs.max_run_bytes = 4 * 1024 * 1024;
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store.start_requested(&launch.attempt_id, 0).unwrap();
    let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
    store.running(&launch, process.pid(), 0).unwrap();
    thread::sleep(Duration::from_millis(100));
    process.freeze_capture(&store).unwrap();
    store
        .finish(
            &launch,
            &Outcome {
                state: EndState::Unknown,
                error_code: Some("TERMINATION_UNCONFIRMED"),
                exit_code: None,
                log_bytes: 0,
                log_truncated: true,
            },
            1,
        )
        .unwrap();
    let before = store.log_info(&launch.attempt_id).unwrap();
    thread::sleep(Duration::from_millis(50));
    for _ in 0..20 {
        let _ = process.poll(&mut store);
    }
    let after = store.log_info(&launch.attempt_id).unwrap();
    assert_eq!(
        after.capture_state,
        cronharbor::contracts::LogCaptureState::Frozen
    );
    assert_eq!(before.written_end_offset, after.written_end_offset);
    assert_eq!(
        std::fs::metadata(store.log_path(&launch.attempt_id).unwrap())
            .unwrap()
            .len(),
        0
    );
    assert_eq!(store.append_log(&launch, b"late").unwrap(), 0);
}

#[test]
fn full_channel_stops_capture_without_blocking_pipe_drain() {
    use cronharbor::process::{EndState, RunningProcess};
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let (dir, mut store) = fixture();
    let size = 3 * 1024 * 1024;
    let input = dir.path().join("input");
    std::fs::write(&input, vec![b'x'; size]).unwrap();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.command = "/bin/cat".into();
    manifest.spec.execution.args = vec![input.to_string_lossy().into_owned()];
    manifest.spec.logs.max_run_bytes = 4 * 1024 * 1024;
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store.start_requested(&launch.attempt_id, 0).unwrap();
    let start = Instant::now();
    let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
    store.running(&launch, process.pid(), 0).unwrap();
    thread::sleep(Duration::from_millis(100));
    let mut polls = 0;
    let mut max_poll = Duration::ZERO;
    loop {
        let at = Instant::now();
        let outcome = process.poll(&mut store).unwrap();
        max_poll = max_poll.max(at.elapsed());
        polls += 1;
        if let Some(outcome) = outcome {
            assert_eq!(outcome.state, EndState::Succeeded);
            store.finish(&launch, &outcome, 1).unwrap();
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        thread::sleep(Duration::from_millis(1));
    }
    let info = store.log_info(&launch.attempt_id).unwrap();
    assert_eq!(
        info.last_error_code.as_deref(),
        Some("CAPTURE_BACKPRESSURE")
    );
    assert!(info.written_end_offset <= 64 * 16384);
    assert!(info.known_discarded_bytes > 0);
    assert_eq!(
        info.written_end_offset + info.known_discarded_bytes,
        u64::try_from(size).unwrap()
    );
    eprintln!(
        "bounded capture fixture input={size} elapsed_ms={} polls={polls} max_owner_poll_us={} saved={} discarded={} queue_payload_bound={}",
        start.elapsed().as_millis(),
        max_poll.as_micros(),
        info.written_end_offset,
        info.known_discarded_bytes,
        64 * 16384
    );
}

#[test]
fn full_storage_stops_capture_without_changing_process_outcome() {
    use cronharbor::process::{EndState, RunningProcess};
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let (_dir, mut store) = fixture();
    store
        .set_log_storage_policy(
            &cronharbor::contracts::LogStoragePolicyInput {
                max_disk_bytes: 16,
                max_segments: 1024,
                retention_seconds: 604800,
            },
            1,
        )
        .unwrap();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.args = vec!["x".repeat(1000)];
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store.start_requested(&launch.attempt_id, 0).unwrap();
    let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
    store.running(&launch, process.pid(), 0).unwrap();
    let start = Instant::now();
    loop {
        if let Some(outcome) = process.poll(&mut store).unwrap() {
            assert_eq!(outcome.state, EndState::Succeeded);
            store.finish(&launch, &outcome, 1).unwrap();
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(3));
        thread::sleep(Duration::from_millis(1));
    }
    let info = store.log_info(&launch.attempt_id).unwrap();
    assert_eq!(info.written_end_offset, 16);
    assert_eq!(
        info.last_error_code.as_deref(),
        Some("LOG_STORAGE_PRESSURE")
    );
    assert_eq!(info.known_discarded_bytes, 985);
}

#[test]
fn short_flushed_writes_coalesce_without_loss_during_slow_owner_turns() {
    use cronharbor::process::{EndState, RunningProcess};
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let (_dir, mut store) = fixture();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.command = "/usr/bin/python3".into();
    manifest.spec.execution.args=vec!["-u".into(),"-c".into(),"import sys,time\nfor i in range(1200):\n sys.stdout.write(f'{i:06d} '+('x'*72)+'\\n');sys.stdout.flush();time.sleep(.001)".into()];
    manifest.spec.logs.max_run_bytes = 196608;
    manifest.spec.logs.rotation = Some(cronharbor::config::LogRotation {
        segment_bytes: 65536,
        max_segments: 3,
    });
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store.start_requested(&launch.attempt_id, 0).unwrap();
    let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
    store.running(&launch, process.pid(), 0).unwrap();
    let start = Instant::now();
    let mut max_poll = Duration::ZERO;
    let mut polls = 0;
    loop {
        let at = Instant::now();
        let outcome = process.poll(&mut store).unwrap();
        max_poll = max_poll.max(at.elapsed());
        polls += 1;
        let info = store.log_info(&launch.attempt_id).unwrap();
        if info.written_end_offset > info.retained_from_offset {
            store
                .read_log(&launch.attempt_id, info.retained_from_offset, 4096)
                .unwrap();
        }
        if let Some(outcome) = outcome {
            assert_eq!(outcome.state, EndState::Succeeded);
            store.finish(&launch, &outcome, 1).unwrap();
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        thread::sleep(Duration::from_millis(40));
    }
    let info = store.log_info(&launch.attempt_id).unwrap();
    assert_eq!(info.written_end_offset, 96000);
    assert_eq!(info.known_discarded_bytes, 0);
    assert_eq!(info.last_error_code, None);
    let mut actual = String::new();
    let mut offset = 0;
    while offset < info.written_end_offset {
        let log = store.read_log(&launch.attempt_id, offset, 65536).unwrap();
        actual.push_str(&log.text);
        offset = log.next_offset;
    }
    let expected = (0..1200)
        .map(|i| format!("{i:06} {}\n", "x".repeat(72)))
        .collect::<String>();
    assert_eq!(actual, expected);
    eprintln!(
        "short-write fixture lines=1200 bytes=96000 requested_interval_us=1000 owner_sleep_ms=40 elapsed_ms={} polls={polls} max_owner_poll_us={} loss=0",
        start.elapsed().as_millis(),
        max_poll.as_micros()
    );
}

#[test]
fn secret_crossing_rotation_boundary_is_never_written() {
    use cronharbor::{
        config::SecretRef,
        process::{EndState, RunningProcess},
    };
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let (dir, mut store) = fixture();
    let secret = b"long-overlapping-sensitive-value";
    let secret_path = dir.path().join("secret");
    std::fs::write(&secret_path, secret).unwrap();
    std::fs::set_permissions(&secret_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut raw = vec![b'x'; 65532];
    raw.extend_from_slice(secret);
    raw.extend_from_slice("😀\n".as_bytes());
    raw.extend_from_slice(&vec![b'y'; 20000]);
    let mut expected = vec![b'x'; 65532];
    expected.extend_from_slice(b"[REDACTED]");
    expected.extend_from_slice("😀\n".as_bytes());
    expected.extend_from_slice(&vec![b'y'; 20000]);
    let input = dir.path().join("input");
    std::fs::write(&input, raw).unwrap();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.command = "/bin/cat".into();
    manifest.spec.execution.args = vec![input.to_string_lossy().into_owned()];
    manifest.spec.execution.secret_refs.insert(
        "TEST_SECRET".into(),
        SecretRef::File {
            path: secret_path.to_string_lossy().into_owned(),
        },
    );
    manifest.spec.logs.max_run_bytes = 131072;
    manifest.spec.logs.rotation = Some(cronharbor::config::LogRotation {
        segment_bytes: 65536,
        max_segments: 2,
    });
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store.start_requested(&launch.attempt_id, 0).unwrap();
    let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
    store.running(&launch, process.pid(), 0).unwrap();
    let start = Instant::now();
    loop {
        if let Some(outcome) = process.poll(&mut store).unwrap() {
            assert_eq!(outcome.state, EndState::Succeeded);
            store.finish(&launch, &outcome, 1).unwrap();
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(1));
    }
    let mut read = Vec::new();
    let mut offset = 0;
    let info = store.log_info(&launch.attempt_id).unwrap();
    while offset < info.written_end_offset {
        let log = store.read_log(&launch.attempt_id, offset, 65536).unwrap();
        read.extend_from_slice(log.text.as_bytes());
        offset = log.next_offset;
    }
    assert_eq!(read, expected);
    assert_eq!(store.history().unwrap().len(), 1);
    for entry in std::fs::read_dir(dir.path().join("logs")).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(!bytes.windows(secret.len()).any(|w| w == secret));
    }
}

#[test]
fn crash_recovery_protects_prepared_open_and_delete_pending_evidence() {
    use cronharbor::process::{EndState, Outcome};
    for phase in [
        "prepared_missing",
        "prepared_existing",
        "open_tail",
        "delete_present",
        "delete_missing",
        "sealed_missing",
    ] {
        let (dir, mut store) = fixture();
        let launch = launch(&mut store, false);
        store.append_log(&launch, b"safe").unwrap();
        let log = store.log_path(&launch.attempt_id).unwrap();
        let db = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
        match phase {
            "prepared_missing" => {
                db.execute(
                    "UPDATE log_segments SET state='PREPARED',end_offset=0,bytes_written=0",
                    [],
                )
                .unwrap();
                db.execute("UPDATE log_streams SET written_end_offset=0", [])
                    .unwrap();
                std::fs::remove_file(&log).unwrap();
            }
            "prepared_existing" => {
                db.execute(
                    "UPDATE log_segments SET state='PREPARED',end_offset=0,bytes_written=0",
                    [],
                )
                .unwrap();
                db.execute("UPDATE log_streams SET written_end_offset=0", [])
                    .unwrap();
            }
            "open_tail" => {
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&log)
                    .unwrap()
                    .write_all(b" redacted tail")
                    .unwrap();
            }
            "delete_present" | "delete_missing" => {
                db.execute("UPDATE log_segments SET state='DELETE_PENDING'", [])
                    .unwrap();
                if phase == "delete_missing" {
                    std::fs::remove_file(&log).unwrap();
                }
            }
            "sealed_missing" => {
                store
                    .finish(
                        &launch,
                        &Outcome {
                            state: EndState::Succeeded,
                            error_code: None,
                            exit_code: Some(0),
                            log_bytes: 4,
                            log_truncated: false,
                        },
                        1,
                    )
                    .unwrap();
                std::fs::remove_file(&log).unwrap();
            }
            _ => unreachable!(),
        }
        drop(db);
        drop(store);
        let store = Store::open(dir.path()).unwrap();
        let info = store.log_info(&launch.attempt_id).unwrap();
        match phase {
            "prepared_missing" => assert_eq!(info.retained_bytes, 0),
            "prepared_existing" => {
                assert_eq!(info.written_end_offset, 4);
                assert_eq!(
                    info.capture_state,
                    cronharbor::contracts::LogCaptureState::Frozen
                );
            }
            "open_tail" => {
                assert_eq!(
                    store.read_log(&launch.attempt_id, 0, 65536).unwrap().text,
                    "safe redacted tail"
                );
                assert_eq!(info.written_end_offset, 18);
            }
            "delete_present" => {
                assert!(log.exists());
                assert_eq!(info.retained_bytes, 4);
                assert_eq!(
                    info.capture_state,
                    cronharbor::contracts::LogCaptureState::Frozen
                );
            }
            "delete_missing" => {
                assert_eq!(info.retained_from_offset, 4);
                assert_eq!(info.retained_bytes, 0);
                assert!(info.history_evicted);
            }
            "sealed_missing" => {
                assert_eq!(
                    store
                        .read_log(&launch.attempt_id, 0, 65536)
                        .unwrap_err()
                        .code,
                    "LOG_UNAVAILABLE"
                );
                assert_eq!(store.log_storage().unwrap().usage.retained_bytes, 4);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn unexplained_files_are_budgeted_and_never_removed() {
    let (dir, mut store) = fixture();
    store
        .set_log_storage_policy(
            &cronharbor::contracts::LogStoragePolicyInput {
                max_disk_bytes: 16,
                max_segments: 2,
                retention_seconds: 604800,
            },
            1,
        )
        .unwrap();
    let launch = launch(&mut store, false);
    let orphan = dir.path().join("logs/unexplained");
    std::fs::write(&orphan, b"sixteen bytes!!!").unwrap();
    let usage = store.log_storage().unwrap().usage;
    assert_eq!(usage.unexplained_bytes, 16);
    assert_eq!(usage.unexplained_segments, 1);
    assert_eq!(
        store.append_log(&launch, b"blocked").unwrap_err().code,
        "LOG_STORAGE_PRESSURE"
    );
    store.retain_logs().unwrap();
    assert!(orphan.exists());
}

#[test]
fn policy_update_requires_global_admin_revision_and_actor_idempotency() {
    use cronharbor::contracts::{LogStoragePolicyInput, Role};
    let (_dir, mut store) = fixture();
    let policy = LogStoragePolicyInput {
        max_disk_bytes: 536870912,
        max_segments: 2048,
        retention_seconds: 1209600,
    };
    for (role, namespaces, capabilities) in [
        (Role::Admin, vec!["local"], vec!["operations.manage"]),
        (Role::Admin, vec!["*"], vec!["workload.read"]),
        (Role::Editor, vec!["*"], vec!["*"]),
        (Role::Observe, vec!["*"], vec!["*"]),
    ] {
        let token = application::execute(
            &mut store,
            AppCommand::CreateToken {
                role,
                namespaces: namespaces.into_iter().map(str::to_owned).collect(),
                capabilities: capabilities.into_iter().map(str::to_owned).collect(),
                ttl_seconds: 3600,
            },
            None,
        )
        .unwrap();
        let token = token["token"].as_str().unwrap();
        for command in [
            AppCommand::LogStorage,
            AppCommand::SetLogStorage {
                policy: policy.clone(),
                expected_revision: 1,
                idempotency_key: "denied".into(),
            },
        ] {
            assert_eq!(
                application::execute(&mut store, command, Some(token))
                    .unwrap_err()
                    .code,
                "NOT_AUTHORIZED"
            );
        }
    }
    let command = AppCommand::SetLogStorage {
        policy: policy.clone(),
        expected_revision: 1,
        idempotency_key: "same".into(),
    };
    let one = application::execute(&mut store, command.clone(), None).unwrap();
    let replay = application::execute(&mut store, command, None).unwrap();
    assert_eq!(one["id"], replay["id"]);
    assert_eq!(one["state"], "SUCCEEDED");
    assert_eq!(store.log_storage_policy().unwrap().max_segments, 2048);
    assert_eq!(
        store.log_storage_policy().unwrap().retention_seconds,
        1209600
    );
    assert_eq!(
        application::execute(
            &mut store,
            AppCommand::SetLogStorage {
                policy,
                expected_revision: 1,
                idempotency_key: "stale".into()
            },
            None
        )
        .unwrap_err()
        .code,
        "REVISION_CONFLICT"
    );
}

#[test]
fn genuine_schema_three_upgrade_seeds_legacy_policy_without_rewriting_files_or_revisions() {
    use sha2::{Digest, Sha256};
    let (source, mut store) = fixture();
    let launch = launch(&mut store, false);
    let revision = store.manifest_for("local/hello").unwrap().1;
    store.prepare_log_capture(&launch).unwrap();
    store.append_log(&launch, b"legacy evidence").unwrap();
    drop(store);
    let target = tempfile::tempdir().unwrap();
    std::fs::set_permissions(target.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = target.path().join("state.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    for (version, sql) in [
        (1, include_str!("../migrations/0001_initial.sql")),
        (2, include_str!("../migrations/0002_runtime.sql")),
        (3, include_str!("../migrations/0003_auth_rejections.sql")),
    ] {
        db.execute_batch(sql).unwrap();
        db.execute(
            "INSERT INTO schema_migrations(version,checksum,applied_at_us) VALUES(?1,?2,0)",
            rusqlite::params![version, format!("{:x}", Sha256::digest(sql.as_bytes()))],
        )
        .unwrap();
    }
    db.execute(
        "ATTACH DATABASE ?1 AS source",
        [source.path().join("state.db").to_string_lossy().as_ref()],
    )
    .unwrap();
    db.execute_batch("BEGIN").unwrap();
    for table in [
        "workloads",
        "workload_revisions",
        "schedules",
        "occurrences",
        "executions",
        "attempts",
    ] {
        let columns = db
            .prepare(&format!("PRAGMA main.table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join(",");
        db.execute_batch(&format!(
            "INSERT INTO main.{table}({columns}) SELECT {columns} FROM source.{table}"
        ))
        .unwrap();
    }
    db.execute_batch("COMMIT; DETACH DATABASE source").unwrap();
    let mut legacy = launch.manifest.clone();
    legacy.spec.logs.max_disk_bytes = Some(16384);
    legacy.spec.logs.max_segments = Some(4096);
    legacy.spec.logs.retention_seconds = Some(1209600);
    let legacy_json = serde_json::to_string(&legacy).unwrap();
    db.execute(
        "UPDATE workload_revisions SET spec_json=?1 WHERE id=?2",
        rusqlite::params![legacy_json, revision],
    )
    .unwrap();
    let deleted_workload = uuid::Uuid::new_v4().to_string();
    let deleted_revision = uuid::Uuid::new_v4().to_string();
    db.execute_batch("BEGIN").unwrap();
    db.execute("INSERT INTO workloads(id,namespace,name,kind,source_kind,enabled,current_revision_id,deleted_at_us,created_at_us) SELECT ?1,namespace,'deleted-policy',kind,source_kind,enabled,?2,1,created_at_us FROM workloads LIMIT 1",rusqlite::params![deleted_workload,deleted_revision]).unwrap();
    db.execute("INSERT INTO workload_revisions(id,workload_id,revision_no,schema_version,spec_json,spec_hash,created_by,created_at_us) SELECT ?1,?2,1,schema_version,json_set(spec_json,'$.metadata.name','deleted-policy','$.spec.logs.maxDiskBytes',8192),spec_hash,created_by,created_at_us FROM workload_revisions WHERE id=?3",rusqlite::params![deleted_revision,deleted_workload,revision]).unwrap();
    db.execute_batch("COMMIT").unwrap();
    let segment = uuid::Uuid::new_v4().to_string();
    db.execute("INSERT INTO log_segments(id,attempt_id,relative_path,bytes_written,created_at_us) VALUES(?1,?2,?3,0,1)",rusqlite::params![segment,launch.attempt_id,format!("logs/{}.log",launch.attempt_id)]).unwrap();
    db.execute_batch("PRAGMA user_version=3").unwrap();
    drop(db);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let logs = cronharbor::store::private_directory(&target.path().join("logs")).unwrap();
    let log = logs.join(format!("{}.log", launch.attempt_id));
    std::fs::copy(
        source
            .path()
            .join(format!("logs/{}.log", launch.attempt_id)),
        &log,
    )
    .unwrap();
    let mut store = Store::open(target.path()).unwrap();
    let policy = store.log_storage_policy().unwrap();
    assert_eq!(policy.max_disk_bytes, 8192);
    assert_eq!(
        serde_json::to_value(&policy).unwrap()["imported_conservative"],
        true
    );
    assert_eq!(policy.max_segments, 1024);
    assert_eq!(policy.retention_seconds, 604800);
    assert_eq!(std::fs::read(&log).unwrap(), b"legacy evidence");
    let info = store.log_info(&launch.attempt_id).unwrap();
    assert_eq!(info.written_end_offset, 15);
    assert_eq!(
        info.capture_mode,
        cronharbor::contracts::LogCaptureMode::Prefix
    );
    assert_eq!(
        info.capture_state,
        cronharbor::contracts::LogCaptureState::Frozen
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    let after: String = db
        .query_row(
            "SELECT spec_json FROM workload_revisions WHERE id=?1",
            [&revision],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, legacy_json);
    // Explicit legacy values may be retained unchanged or removed after the singleton was imported.
    store.apply(&legacy, 0).unwrap();
    legacy.spec.logs.max_disk_bytes = None;
    legacy.spec.logs.max_segments = None;
    legacy.spec.logs.retention_seconds = None;
    store.apply(&legacy, 0).unwrap();
    assert_eq!(store.log_storage_policy().unwrap().max_disk_bytes, 8192);
}

#[test]
fn private_readonly_retained_logs_remain_readable_after_recovery() {
    use cronharbor::process::{EndState, Outcome};
    let (dir, mut store) = fixture();
    let launch = launch(&mut store, false);
    store.append_log(&launch, b"read only evidence").unwrap();
    store
        .finish(
            &launch,
            &Outcome {
                state: EndState::Succeeded,
                error_code: None,
                exit_code: Some(0),
                log_bytes: 18,
                log_truncated: false,
            },
            1,
        )
        .unwrap();
    let path = store.log_path(&launch.attempt_id).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    assert_eq!(
        store.read_log(&launch.attempt_id, 0, 65536).unwrap().text,
        "read only evidence"
    );
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(
        store.read_log(&launch.attempt_id, 0, 65536).unwrap().text,
        "read only evidence"
    );
}

#[test]
fn capture_path_failure_stops_capture_without_resuming_or_changing_exit() {
    use cronharbor::process::{EndState, RunningProcess};
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let (_dir, mut store) = fixture();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.command = "/usr/bin/python3".into();
    manifest.spec.execution.args=vec!["-u".into(),"-c".into(),"import sys,time;sys.stdout.write('x'*1000);sys.stdout.flush();time.sleep(.2);sys.stdout.write('y'*1000);sys.stdout.flush()".into()];
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store.start_requested(&launch.attempt_id, 0).unwrap();
    let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
    store.running(&launch, process.pid(), 0).unwrap();
    let path = store.log_path(&launch.attempt_id).unwrap();
    let preserved = _dir.path().join("preserved.log");
    std::fs::rename(&path, &preserved).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let start = Instant::now();
    let mut restored = false;
    loop {
        let outcome = process.poll(&mut store).unwrap();
        if store
            .log_info(&launch.attempt_id)
            .unwrap()
            .last_error_code
            .is_some()
            && !restored
        {
            std::fs::remove_dir(&path).unwrap();
            std::fs::rename(&preserved, &path).unwrap();
            restored = true;
        }
        if let Some(outcome) = outcome {
            assert_eq!(outcome.state, EndState::Succeeded);
            store.finish(&launch, &outcome, 1).unwrap();
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(1));
    }
    let info = store.log_info(&launch.attempt_id).unwrap();
    assert!(restored);
    assert_eq!(info.written_end_offset, 0);
    assert_eq!(info.known_discarded_bytes, 2000);
    assert_eq!(
        info.capture_state,
        cronharbor::contracts::LogCaptureState::Stopped
    );
    assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
}

#[test]
fn escaped_descendant_retaining_output_is_unknown_and_frozen() {
    use cronharbor::process::{EndState, RunningProcess};
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    use std::{
        thread,
        time::{Duration, Instant},
    };
    struct Escaped(Pid);
    impl Drop for Escaped {
        fn drop(&mut self) {
            let _ = kill(self.0, Signal::SIGKILL);
        }
    }
    let (dir, mut store) = fixture();
    let pid_file = dir.path().join("escaped.pid");
    let release = dir.path().join("release");
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.command = "/usr/bin/python3".into();
    manifest.spec.execution.args=vec!["-u".into(),"-c".into(),"import os,sys,time\np=os.fork()\nif p==0:\n os.setsid()\n open(sys.argv[1],'w').write(str(os.getpid()))\n while not os.path.exists(sys.argv[2]): time.sleep(.01)\n sys.stdout.write('late escaped output');sys.stdout.flush();time.sleep(30)\nelse:\n while not os.path.exists(sys.argv[1]): time.sleep(.01)".into(),pid_file.to_string_lossy().into_owned(),release.to_string_lossy().into_owned()];
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    store.start_requested(&launch.attempt_id, 0).unwrap();
    let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
    store.running(&launch, process.pid(), 0).unwrap();
    let start = Instant::now();
    let escaped_pid = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|pid| *pid > 0)
        {
            break pid;
        }
        assert!(start.elapsed() < Duration::from_secs(3));
        thread::sleep(Duration::from_millis(5));
    };
    let escaped = Escaped(Pid::from_raw(escaped_pid));
    let outcome = loop {
        if let Some(outcome) = process.poll(&mut store).unwrap() {
            break outcome;
        }
        assert!(start.elapsed() < Duration::from_secs(6));
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(outcome.state, EndState::Unknown);
    assert_eq!(outcome.error_code, Some("TERMINATION_UNCONFIRMED"));
    store.finish(&launch, &outcome, 1).unwrap();
    let before = std::fs::read(store.log_path(&launch.attempt_id).unwrap()).unwrap();
    std::fs::write(release, b"release").unwrap();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        std::fs::read(store.log_path(&launch.attempt_id).unwrap()).unwrap(),
        before
    );
    assert_eq!(
        store.log_info(&launch.attempt_id).unwrap().capture_state,
        cronharbor::contracts::LogCaptureState::Frozen
    );
    drop(escaped);
}

#[test]
fn recovery_does_not_replay_deletion_of_previously_unavailable_evidence() {
    use cronharbor::process::{EndState, Outcome};
    let (dir, mut store) = fixture();
    let launch = launch(&mut store, false);
    store.append_log(&launch, b"evidence").unwrap();
    store
        .finish(
            &launch,
            &Outcome {
                state: EndState::Succeeded,
                error_code: None,
                exit_code: Some(0),
                log_bytes: 8,
                log_truncated: false,
            },
            1,
        )
        .unwrap();
    let log = store.log_path(&launch.attempt_id).unwrap();
    let db = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
    db.execute_batch("UPDATE log_segments SET state='DELETE_PENDING'; UPDATE log_streams SET last_error_code='LOG_UNAVAILABLE'").unwrap();
    drop(db);
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(std::fs::read(log).unwrap(), b"evidence");
    assert_eq!(
        store.log_info(&launch.attempt_id).unwrap().retained_bytes,
        8
    );
}

#[test]
fn unavailable_log_evidence_is_protected_and_blocks_underfit_policy() {
    use cronharbor::{
        contracts::LogStoragePolicyInput,
        process::{EndState, Outcome},
    };
    let (dir, mut store) = fixture();
    let mut paths = Vec::new();
    for name in ["first", "second"] {
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.metadata.name = name.into();
        store.apply(&manifest, 0).unwrap();
        let launch = store.claim_due(60_000_000).unwrap().unwrap();
        store.start_requested(&launch.attempt_id, 0).unwrap();
        store.running(&launch, 1, 0).unwrap();
        store.prepare_log_capture(&launch).unwrap();
        store.append_log(&launch, b"evidence").unwrap();
        store
            .finish(
                &launch,
                &Outcome {
                    state: EndState::Succeeded,
                    error_code: None,
                    exit_code: Some(0),
                    log_bytes: 8,
                    log_truncated: false,
                },
                1,
            )
            .unwrap();
        paths.push(store.log_path(&launch.attempt_id).unwrap());
    }
    drop(store);
    for path in paths {
        std::fs::remove_file(path).unwrap();
    }
    let store = Store::open(dir.path()).unwrap();
    let usage = store.log_storage().unwrap().usage;
    assert_eq!(usage.protected_bytes, 16);
    assert_eq!(usage.protected_segments, 2);
    assert_eq!(usage.retained_bytes, 16);
    assert_eq!(usage.retained_segments, 2);
    for policy in [
        LogStoragePolicyInput {
            max_disk_bytes: 15,
            max_segments: 1024,
            retention_seconds: 604800,
        },
        LogStoragePolicyInput {
            max_disk_bytes: 16,
            max_segments: 1,
            retention_seconds: 604800,
        },
    ] {
        assert_eq!(
            store.set_log_storage_policy(&policy, 1).unwrap_err().code,
            "STATE_CONFLICT"
        );
        assert_eq!(store.log_storage_policy().unwrap().revision, 1);
    }
    store
        .set_log_storage_policy(
            &LogStoragePolicyInput {
                max_disk_bytes: 16,
                max_segments: 2,
                retention_seconds: 604800,
            },
            1,
        )
        .unwrap();
    store.retain_logs().unwrap();
    assert_eq!(store.log_storage().unwrap().usage.protected_bytes, 16);
    assert_eq!(store.log_storage().unwrap().usage.protected_segments, 2);
}

#[test]
fn external_result_rollback_cannot_unlink_logs_before_outer_commit() {
    use cronharbor::{
        contracts::LogStoragePolicyInput,
        external::DockerSpec,
        process::{EndState, Outcome},
    };
    let (dir, mut store) = fixture();
    store
        .set_log_storage_policy(
            &LogStoragePolicyInput {
                max_disk_bytes: 268435456,
                max_segments: 1024,
                retention_seconds: 1,
            },
            1,
        )
        .unwrap();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.spec.execution.adapter = "docker".into();
    manifest.spec.execution.docker = Some(DockerSpec {
        image: format!("fixture@sha256:{}", "a".repeat(64)),
        user: "10001".into(),
        network: "none".into(),
        memory_bytes: 8 * 1024 * 1024,
        pids_limit: 8,
    });
    store.apply(&manifest, 0).unwrap();
    let launch = store.claim_due(60_000_000).unwrap().unwrap();
    let handle = cronharbor::external::docker_handle(&store.external_identity(&launch).unwrap());
    store.start_requested(&launch.attempt_id, 0).unwrap();
    store
        .running_handle(&launch, &serde_json::to_string(&handle).unwrap(), 0)
        .unwrap();
    store.prepare_log_capture(&launch).unwrap();
    store.append_log(&launch, b"snapshot evidence").unwrap();
    let path = store.log_path(&launch.attempt_id).unwrap();
    let database = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
    database.execute_batch("UPDATE log_segments SET created_at_us=1; CREATE TRIGGER injected_cleanup_failure BEFORE INSERT ON external_cleanup BEGIN SELECT RAISE(ABORT,'injected cleanup queue failure'); END;").unwrap();
    let outcome = Outcome {
        state: EndState::Succeeded,
        error_code: None,
        exit_code: Some(0),
        log_bytes: 17,
        log_truncated: false,
    };
    assert_eq!(
        store
            .finish_external(&launch, &outcome, true, &handle, cronharbor::now_us())
            .unwrap_err()
            .code,
        "PERSISTENCE_UNAVAILABLE"
    );
    assert_eq!(store.history().unwrap()[0].state, "RUNNING");
    assert_eq!(std::fs::read(&path).unwrap(), b"snapshot evidence");
    assert_eq!(
        store.log_info(&launch.attempt_id).unwrap().retained_bytes,
        17
    );
    database
        .execute_batch("DROP TRIGGER injected_cleanup_failure")
        .unwrap();
    store
        .finish_external(&launch, &outcome, true, &handle, cronharbor::now_us())
        .unwrap();
    assert_eq!(store.history().unwrap()[0].state, "SUCCEEDED");
    assert!(!path.exists());
    assert_eq!(
        store.log_info(&launch.attempt_id).unwrap().retained_bytes,
        0
    );
    assert_eq!(
        store.next_external_cleanup().unwrap().unwrap().0,
        launch.attempt_id
    );
}

fn latest_command(attempt: &str, max_bytes: u32) -> AppCommand {
    serde_json::from_value(
        serde_json::json!({"action":"latest_logs","attempt_id":attempt,"max_bytes":max_bytes}),
    )
    .unwrap()
}

#[test]
fn atomic_latest_read_survives_repeated_rotation_while_explicit_stale_cursor_fails() {
    use cronharbor::contracts::LogDto;
    let (_dir, mut store) = fixture();
    let launch = launch(&mut store, true);
    let mut expected = vec![b'a'; 262144];
    store.append_log(&launch, &expected).unwrap();
    for turn in 0..12 {
        let stale = store
            .log_info(&launch.attempt_id)
            .unwrap()
            .retained_from_offset;
        let bytes = vec![b'b' + u8::try_from(turn).unwrap(); 131072];
        store.append_log(&launch, &bytes).unwrap();
        expected.extend_from_slice(&bytes);
        assert_eq!(
            application::execute(
                &mut store,
                AppCommand::Logs {
                    attempt_id: launch.attempt_id.clone(),
                    offset: stale,
                    max_bytes: 65536
                },
                None
            )
            .unwrap_err()
            .code,
            "LOG_CURSOR_EXPIRED"
        );
        let log: LogDto = serde_json::from_value(
            application::execute(&mut store, latest_command(&launch.attempt_id, 65536), None)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(log.attempt_id, launch.attempt_id);
        assert!(log.stream.history_evicted);
        assert_eq!(log.text.len(), 65536);
        assert_eq!(log.next_offset, log.stream.retained_from_offset + 65536);
        assert_eq!(
            log.stream.written_end_offset,
            u64::try_from(expected.len()).unwrap()
        );
        let start = usize::try_from(log.stream.retained_from_offset).unwrap();
        let end = usize::try_from(log.next_offset).unwrap();
        assert_eq!(log.text.as_bytes(), &expected[start..end]);
    }
}

#[test]
fn latest_read_preserves_namespace_target_and_log_grant_authorization() {
    use cronharbor::contracts::Role;
    let (_dir, mut store) = fixture();
    let launch = launch(&mut store, false);
    store.append_log(&launch, b"prefix evidence").unwrap();
    let workload = store.manifest_for("local/hello").unwrap().0;
    for (namespaces, capabilities, allowed) in [
        (
            vec!["elsewhere".to_owned()],
            vec!["logs.read".to_owned()],
            false,
        ),
        (vec!["local".to_owned()], Vec::new(), false),
        (
            vec!["local".to_owned()],
            vec!["logs.read:other-workload".to_owned()],
            false,
        ),
        (
            vec!["local".to_owned()],
            vec![format!("logs.read:{workload}")],
            true,
        ),
    ] {
        let credential = application::execute(
            &mut store,
            AppCommand::CreateToken {
                role: Role::Viewer,
                namespaces,
                capabilities,
                ttl_seconds: 3600,
            },
            None,
        )
        .unwrap();
        let credential = credential["token"].as_str().unwrap();
        let result = application::execute(
            &mut store,
            latest_command(&launch.attempt_id, 65536),
            Some(credential),
        );
        if allowed {
            let log = result.unwrap();
            assert_eq!(log["text"], "prefix evidence");
            assert_eq!(log["stream"]["retained_from_offset"], 0);
            assert_eq!(log["next_offset"], 15);
        } else {
            assert_eq!(result.unwrap_err().code, "NOT_AUTHORIZED");
        }
    }
    for max_bytes in [0, 65537] {
        assert_eq!(
            application::execute(
                &mut store,
                latest_command(&launch.attempt_id, max_bytes),
                None
            )
            .unwrap_err()
            .code,
            "INPUT_INVALID"
        );
    }
}
