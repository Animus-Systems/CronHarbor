use cronharbor::{application, contracts::AppCommand, store::Store};
use std::os::unix::fs::PermissionsExt;
fn setup() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let s = Store::open(d.path()).unwrap();
    (d, s)
}
fn call(s: &mut Store, c: AppCommand) -> serde_json::Value {
    application::execute(s, c, None).unwrap()
}
fn raw() -> String {
    include_str!("../examples/hello.yaml")
        .replace("owner: file", "owner: api")
        .replace("ScheduledJob", "OneShotJob")
}
#[test]
fn revision_cas_file_ownership_and_manual_idempotency() {
    let (_d, mut s) = setup();
    let file = cronharbor::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    s.apply(&file, 0).unwrap();
    assert_eq!(
        application::execute(
            &mut s,
            AppCommand::Apply {
                definition: raw(),
                expected_revision: None,
                source_kind: "API".into(),
                idempotency_key: "ownership".into()
            },
            None
        )
        .unwrap_err()
        .code,
        "SOURCE_CONFLICT"
    );
    let definition = raw().replace("name: hello", "name: manual");
    let result = call(
        &mut s,
        AppCommand::Apply {
            definition: definition.clone(),
            expected_revision: None,
            source_kind: "API".into(),
            idempotency_key: "create".into(),
        },
    );
    assert_eq!(result["state"], "SUCCEEDED");
    let first = call(
        &mut s,
        AppCommand::Run {
            id: "local/manual".into(),
            idempotency_key: "run".into(),
        },
    );
    let second = call(
        &mut s,
        AppCommand::Run {
            id: "local/manual".into(),
            idempotency_key: "run".into(),
        },
    );
    assert_eq!(first["id"], second["id"]);
    assert_eq!(
        application::execute(
            &mut s,
            AppCommand::Pause {
                id: "local/manual".into(),
                paused: true,
                idempotency_key: "run".into()
            },
            None
        )
        .unwrap_err()
        .code,
        "IDEMPOTENCY_CONFLICT"
    );
    assert!(s.claim_due(cronharbor::now_us()).unwrap().is_some());
    assert!(s.claim_due(cronharbor::now_us()).unwrap().is_none());
    assert_eq!(
        application::execute(
            &mut s,
            AppCommand::Apply {
                definition,
                expected_revision: None,
                source_kind: "API".into(),
                idempotency_key: "update".into()
            },
            None
        )
        .unwrap_err()
        .code,
        "REVISION_CONFLICT"
    );
}
#[test]
fn scoped_token_is_hashed_revocable_and_cannot_mutate() {
    use cronharbor::contracts::Role;
    let (d, mut s) = setup();
    let token = call(
        &mut s,
        AppCommand::CreateToken {
            role: Role::Viewer,
            namespaces: vec!["local".into()],
            capabilities: vec![],
            ttl_seconds: 3600,
        },
    );
    let secret = token["token"].as_str().unwrap();
    assert!(application::execute(&mut s, AppCommand::Status, Some(secret)).is_ok());
    assert_eq!(
        application::execute(
            &mut s,
            AppCommand::GlobalPause {
                paused: true,
                evidence: "test".into()
            },
            Some(secret)
        )
        .unwrap_err()
        .code,
        "NOT_AUTHORIZED"
    );
    let data = std::fs::read(d.path().join("state.db")).unwrap();
    assert!(!data.windows(secret.len()).any(|v| v == secret.as_bytes()));
    call(
        &mut s,
        AppCommand::RevokeToken {
            id: token["metadata"]["id"].as_str().unwrap().into(),
        },
    );
    assert_eq!(
        application::execute(&mut s, AppCommand::Status, Some(secret))
            .unwrap_err()
            .code,
        "TOKEN_INVALID"
    );
}
#[test]
fn global_pause_survives_restart_and_restore_requires_cutoff() {
    let (d, mut s) = setup();
    s.apply(
        &cronharbor::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap(),
        0,
    )
    .unwrap();
    call(
        &mut s,
        AppCommand::GlobalPause {
            paused: true,
            evidence: "maintenance".into(),
        },
    );
    drop(s);
    let mut s = Store::open(d.path()).unwrap();
    assert!(s.claim_due(60_000_000).unwrap().is_none());
    drop(s);
    let marker = d.path().join("restore-required.json");
    std::fs::write(
        &marker,
        r#"{"backup_at_us":0,"cutoff_us":120000000,"evidence":"former owner stopped"}"#,
    )
    .unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut s = Store::open(d.path()).unwrap();
    assert!(s.paused().unwrap());
    assert_eq!(
        application::execute(
            &mut s,
            AppCommand::GlobalPause {
                paused: false,
                evidence: "resume".into()
            },
            None
        )
        .unwrap_err()
        .code,
        "RESTORE_REQUIRED"
    );
    call(
        &mut s,
        AppCommand::RestoreResume {
            cutoff_us: 120000000,
            evidence: "external effects reconciled; owner stopped".into(),
        },
    );
    assert!(!s.paused().unwrap());
    assert!(s.claim_due(120000000).unwrap().is_none());
    assert!(s.claim_due(180000000).unwrap().is_some());
}
#[test]
fn config_batch_rolls_back_every_definition_on_ownership_conflict() {
    let (_d, mut s) = setup();
    let file = cronharbor::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    s.apply(&file, 0).unwrap();
    let definition = raw().replace("name: hello", "name: must-rollback");
    let error = application::execute(
        &mut s,
        AppCommand::ApplyBatch {
            definitions: vec![definition, raw()],
            expected_revisions: vec![None, None],
            source_kind: "API".into(),
            idempotency_key: "batch".into(),
        },
        None,
    )
    .unwrap_err();
    assert_eq!(error.code, "SOURCE_CONFLICT");
    assert!(s.manifest_for("local/must-rollback").is_err());
}
#[test]
fn namespace_and_log_grants_apply_inside_shared_queries() {
    use cronharbor::contracts::Role;
    let (_d, mut s) = setup();
    s.apply(
        &cronharbor::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap(),
        0,
    )
    .unwrap();
    let token = call(
        &mut s,
        AppCommand::CreateToken {
            role: Role::Observe,
            namespaces: vec!["elsewhere".into()],
            capabilities: vec!["workload.read".into(), "execution.read".into()],
            ttl_seconds: 3600,
        },
    );
    let secret = token["token"].as_str().unwrap();
    let list = application::execute(
        &mut s,
        AppCommand::Workloads {
            namespace: None,
            cursor: None,
            limit: 10,
        },
        Some(secret),
    )
    .unwrap();
    assert_eq!(list["items"].as_array().unwrap().len(), 0);
    assert_eq!(
        application::execute(
            &mut s,
            AppCommand::Workload {
                id: "local/hello".into()
            },
            Some(secret)
        )
        .unwrap_err()
        .code,
        "NOT_AUTHORIZED"
    );
    let audit = call(
        &mut s,
        AppCommand::Audit {
            cursor: None,
            limit: 100,
        },
    );
    assert!(
        audit["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["action"] == "mcp.workloads")
    );
}
#[test]
fn idempotent_manual_replay_survives_a_later_global_pause() {
    let (_d, mut s) = setup();
    call(
        &mut s,
        AppCommand::Apply {
            definition: raw(),
            expected_revision: None,
            source_kind: "API".into(),
            idempotency_key: "create".into(),
        },
    );
    let op = call(
        &mut s,
        AppCommand::Run {
            id: "local/hello".into(),
            idempotency_key: "run-once".into(),
        },
    );
    call(
        &mut s,
        AppCommand::GlobalPause {
            paused: true,
            evidence: "maintenance".into(),
        },
    );
    let replay = call(
        &mut s,
        AppCommand::Run {
            id: "local/hello".into(),
            idempotency_key: "run-once".into(),
        },
    );
    assert_eq!(op["id"], replay["id"]);
}
#[test]
fn cancellation_before_start_proves_no_external_effect() {
    let (_d, mut s) = setup();
    s.apply(
        &cronharbor::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap(),
        0,
    )
    .unwrap();
    let l = s.claim_due(60_000_000).unwrap().unwrap();
    call(
        &mut s,
        AppCommand::Cancel {
            execution_id: l.execution_id.clone(),
            idempotency_key: "prestart".into(),
        },
    );
    assert_eq!(
        call(&mut s, AppCommand::Execution { id: l.execution_id })["state"],
        "CANCELLED"
    );
    assert!(s.start_requested(&l.attempt_id, 61_000_000).is_err());
}
#[test]
fn editor_cannot_replace_service_probe_commands_with_safe_config_grant() {
    use cronharbor::contracts::Role;
    let (_d, mut s) = setup();
    let mut m =
        cronharbor::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    m.kind = "Service".into();
    m.spec.owner = "api".into();
    let revision = s.apply_scoped(&m, "API", None, "local-owner", 0).unwrap();
    let token = call(
        &mut s,
        AppCommand::CreateToken {
            role: Role::Editor,
            namespaces: vec!["local".into()],
            capabilities: vec!["workload.edit_safe_config".into()],
            ttl_seconds: 3600,
        },
    );
    let secret = token["token"].as_str().unwrap();
    m.spec.service.readiness = Some(cronharbor::config::Probe {
        command: "/usr/bin/true".into(),
        args: vec![],
        interval_seconds: 1,
        timeout_seconds: 1,
        failure_threshold: 1,
        success_threshold: 1,
    });
    let definition = serde_json::to_string(&m).unwrap();
    assert_eq!(
        application::execute(
            &mut s,
            AppCommand::Apply {
                definition,
                expected_revision: Some(revision.clone()),
                source_kind: "API".into(),
                idempotency_key: "probe-rce".into()
            },
            Some(secret)
        )
        .unwrap_err()
        .code,
        "NOT_AUTHORIZED"
    );
    m.spec.service.readiness = None;
    m.spec.timeout_seconds += 1;
    assert!(
        application::execute(
            &mut s,
            AppCommand::Apply {
                definition: serde_json::to_string(&m).unwrap(),
                expected_revision: Some(revision),
                source_kind: "API".into(),
                idempotency_key: "timing-only".into()
            },
            Some(secret)
        )
        .is_ok()
    );
}
#[test]
fn restored_tokens_are_invalidated_once_before_any_remote_read() {
    use cronharbor::contracts::Role;
    let (d, mut s) = setup();
    let old = call(
        &mut s,
        AppCommand::CreateToken {
            role: Role::Admin,
            namespaces: vec!["*".into()],
            capabilities: vec!["*".into()],
            ttl_seconds: 3600,
        },
    );
    let backup = call(
        &mut s,
        AppCommand::Backup {
            name: "token-snapshot".into(),
        },
    );
    call(
        &mut s,
        AppCommand::RevokeToken {
            id: old["metadata"]["id"].as_str().unwrap().into(),
        },
    );
    drop(s);
    cronharbor::ops::restore(
        d.path(),
        std::path::Path::new(backup["path"].as_str().unwrap()),
        Some(cronharbor::now_us()),
        "former owner stopped; effects inspected",
        true,
    )
    .unwrap();
    let mut s = Store::open(d.path()).unwrap();
    assert_eq!(
        application::execute(
            &mut s,
            AppCommand::Identity,
            Some(old["token"].as_str().unwrap())
        )
        .unwrap_err()
        .code,
        "TOKEN_INVALID"
    );
    let fresh = call(
        &mut s,
        AppCommand::CreateToken {
            role: Role::Viewer,
            namespaces: vec!["local".into()],
            capabilities: vec![],
            ttl_seconds: 3600,
        },
    );
    assert!(
        application::execute(
            &mut s,
            AppCommand::Status,
            Some(fresh["token"].as_str().unwrap())
        )
        .is_ok()
    );
    drop(s);
    let mut s = Store::open(d.path()).unwrap();
    assert!(s.restore_required().unwrap());
    assert!(
        application::execute(
            &mut s,
            AppCommand::Identity,
            Some(fresh["token"].as_str().unwrap())
        )
        .is_ok()
    );
}
#[test]
fn scoped_token_admin_cannot_list_or_revoke_broader_authority() {
    use cronharbor::contracts::Role;
    let (_d, mut s) = setup();
    let broad = call(
        &mut s,
        AppCommand::CreateToken {
            role: Role::Admin,
            namespaces: vec!["*".into()],
            capabilities: vec!["*".into()],
            ttl_seconds: 3600,
        },
    );
    let narrow = call(
        &mut s,
        AppCommand::CreateToken {
            role: Role::Admin,
            namespaces: vec!["local".into()],
            capabilities: vec!["tokens.manage".into()],
            ttl_seconds: 3600,
        },
    );
    let token = narrow["token"].as_str().unwrap();
    let listed = application::execute(
        &mut s,
        AppCommand::Tokens {
            cursor: None,
            limit: 100,
        },
        Some(token),
    )
    .unwrap();
    assert!(
        !listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == broad["metadata"]["id"])
    );
    assert_eq!(
        application::execute(
            &mut s,
            AppCommand::RevokeToken {
                id: broad["metadata"]["id"].as_str().unwrap().into()
            },
            Some(token)
        )
        .unwrap_err()
        .code,
        "NOT_AUTHORIZED"
    );
}
#[test]
fn unused_adapter_status_reports_unverified_and_disabled() {
    let (_d, mut s) = setup();
    let status = call(&mut s, AppCommand::Status);
    let health = status["adapter_health"].as_array().unwrap();
    assert!(
        health
            .iter()
            .any(|row| row["adapter"] == "process" && row["state"] == "UNVERIFIED")
    );
    assert!(
        health
            .iter()
            .any(|row| row["adapter"] == "docker" && row["state"] == "DISABLED")
    );
}
#[test]
fn operator_external_grant_is_limited_to_its_preapproved_target() {
    use cronharbor::contracts::{Principal, Role};
    let p = Principal {
        actor_id: "operator".into(),
        role: Role::Operator,
        namespaces: vec!["local".into()],
        capabilities: vec!["docker.manage:approved".into()],
    };
    assert!(cronharbor::policy::require(&p, "docker.manage", Some("approved")).is_ok());
    assert!(cronharbor::policy::require(&p, "docker.manage", Some("other")).is_err());
    assert!(cronharbor::policy::require(&p, "executor.create_command", None).is_err());
}
#[test]
fn global_log_budget_cannot_shrink_while_an_attempt_is_active_or_unknown() {
    let (d, mut s) = setup();
    let mut m =
        cronharbor::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    m.spec.logs.max_disk_bytes = 16384;
    s.apply(&m, 0).unwrap();
    let l = s.claim_due(60_000_000).unwrap().unwrap();
    s.start_requested(&l.attempt_id, 60_000_000).unwrap();
    let mut smaller = m.clone();
    smaller.metadata.name = "smaller".into();
    smaller.spec.logs.max_disk_bytes = 4096;
    assert_eq!(
        s.apply(&smaller, 61_000_000).unwrap_err().code,
        "STATE_CONFLICT"
    );
    drop(s);
    let mut s = Store::open(d.path()).unwrap();
    assert_eq!(
        s.apply(&smaller, 62_000_000).unwrap_err().code,
        "STATE_CONFLICT"
    );
}
#[test]
fn namespace_scoped_admin_cannot_use_instance_wide_operations() {
    use cronharbor::contracts::Role;
    let (d, mut s) = setup();
    let scoped = call(
        &mut s,
        AppCommand::CreateToken {
            role: Role::Admin,
            namespaces: vec!["unrelated".into()],
            capabilities: vec!["operations.manage".into()],
            ttl_seconds: 3600,
        },
    );
    let token = scoped["token"].as_str().unwrap();
    for command in [
        AppCommand::GlobalPause {
            paused: true,
            evidence: "scope is not instance ownership".into(),
        },
        AppCommand::Audit {
            cursor: None,
            limit: 10,
        },
        AppCommand::Backup {
            name: "must-not-create".into(),
        },
        AppCommand::RestoreResume {
            cutoff_us: 0,
            evidence: "not authorized".into(),
        },
    ] {
        assert_eq!(
            application::execute(&mut s, command, Some(token))
                .unwrap_err()
                .code,
            "NOT_AUTHORIZED"
        );
    }
    assert!(!s.paused().unwrap());
    assert!(!d.path().join("backups/must-not-create.db").exists());
}
#[test]
fn namespace_scoped_admin_can_reconcile_its_own_unknown_execution() {
    use cronharbor::contracts::Role;
    let (d, mut s) = setup();
    let m = cronharbor::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    s.apply(&m, 0).unwrap();
    let launch = s.claim_due(60_000_000).unwrap().unwrap();
    s.start_requested(&launch.attempt_id, 60_000_000).unwrap();
    drop(s);
    let mut s = Store::open(d.path()).unwrap();
    let scoped = call(
        &mut s,
        AppCommand::CreateToken {
            role: Role::Admin,
            namespaces: vec!["local".into()],
            capabilities: vec!["operations.manage".into()],
            ttl_seconds: 3600,
        },
    );
    let result = application::execute(
        &mut s,
        AppCommand::Reconcile {
            execution_id: launch.execution_id,
            state: "CANCELLED".into(),
            evidence: "operator proved the external effect is stopped".into(),
            idempotency_key: "scoped-reconciliation".into(),
        },
        Some(scoped["token"].as_str().unwrap()),
    )
    .unwrap();
    assert_eq!(result["state"], "SUCCEEDED");
}
fn log_fixture(bytes: &[u8], terminal: bool) -> (tempfile::TempDir, Store, String) {
    use std::io::Write;
    let (d, mut s) = setup();
    let mut m =
        cronharbor::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    m.spec.logs.max_run_bytes = 100000;
    s.apply(&m, 0).unwrap();
    let launch = s.claim_due(60_000_000).unwrap().unwrap();
    s.start_requested(&launch.attempt_id, 60_000_000).unwrap();
    s.running(&launch, 1, 60_000_000).unwrap();
    let logs = cronharbor::store::private_directory(&d.path().join("logs")).unwrap();
    cronharbor::store::secure_file(&logs.join(format!("{}.log", launch.attempt_id)))
        .unwrap()
        .write_all(bytes)
        .unwrap();
    if terminal {
        s.finish(
            &launch,
            &cronharbor::process::Outcome {
                state: cronharbor::process::EndState::Succeeded,
                error_code: None,
                exit_code: Some(0),
                log_bytes: u64::try_from(bytes.len()).unwrap(),
                log_truncated: false,
            },
            61_000_000,
        )
        .unwrap();
    }
    (d, s, launch.attempt_id)
}
#[test]
fn log_byte_cursor_preserves_emoji_at_the_response_boundary() {
    let mut bytes = vec![b'a'; 65535];
    bytes.extend_from_slice("😀TAIL".as_bytes());
    let (_d, mut s, attempt) = log_fixture(&bytes, true);
    let first = call(
        &mut s,
        AppCommand::Logs {
            attempt_id: attempt.clone(),
            offset: 0,
            max_bytes: 65536,
        },
    );
    assert_eq!(first["text"].as_str().unwrap().len(), 65535);
    assert!(first["text"].as_str().unwrap().bytes().all(|b| b == b'a'));
    assert_eq!(first["next_offset"], 65535);
    let second = call(
        &mut s,
        AppCommand::Logs {
            attempt_id: attempt,
            offset: first["next_offset"].as_u64().unwrap(),
            max_bytes: 65536,
        },
    );
    assert_eq!(second["text"], "😀TAIL");
    assert_eq!(second["next_offset"], 65543);
}
#[test]
fn raw_log_cursor_handles_invalid_utf8_and_small_windows_without_skipping() {
    let (_d, mut s, attempt) = log_fixture(b"LEFT\xffRIGHT", true);
    let mut cursor = 0;
    let mut text = String::new();
    loop {
        let page = call(
            &mut s,
            AppCommand::Logs {
                attempt_id: attempt.clone(),
                offset: cursor,
                max_bytes: 2,
            },
        );
        let next = page["next_offset"].as_u64().unwrap();
        text.push_str(page["text"].as_str().unwrap());
        if !page["response_truncated"].as_bool().unwrap() {
            assert_eq!(next, 10);
            break;
        }
        assert!(next > cursor);
        cursor = next;
    }
    assert_eq!(text, "LEFT�RIGHT");
}
#[test]
fn active_log_waits_for_trailing_utf8_and_terminal_partial_bytes_progress() {
    use std::io::Write;
    let (d, mut s, attempt) = log_fixture(b"A\xf0\x9f", false);
    let first = call(
        &mut s,
        AppCommand::Logs {
            attempt_id: attempt.clone(),
            offset: 0,
            max_bytes: 65536,
        },
    );
    assert_eq!(first["text"], "A");
    assert_eq!(first["next_offset"], 1);
    assert_eq!(first["response_truncated"], false);
    std::fs::OpenOptions::new()
        .append(true)
        .open(d.path().join(format!("logs/{attempt}.log")))
        .unwrap()
        .write_all(b"\x98\x80TAIL")
        .unwrap();
    let second = call(
        &mut s,
        AppCommand::Logs {
            attempt_id: attempt,
            offset: 1,
            max_bytes: 65536,
        },
    );
    assert_eq!(second["text"], "😀TAIL");
    let (_d, mut s, attempt) = log_fixture(b"\xf0\x9f", true);
    let terminal = call(
        &mut s,
        AppCommand::Logs {
            attempt_id: attempt,
            offset: 0,
            max_bytes: 65536,
        },
    );
    assert_eq!(terminal["text"], "�");
    assert_eq!(terminal["next_offset"], 2);
}
#[test]
fn restore_marker_with_missing_or_empty_database_never_creates_a_fresh_state() {
    for empty in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let marker = directory.path().join("restore-required.json");
        std::fs::write(&marker,r#"{"backup_at_us":0,"cutoff_us":0,"evidence":"incomplete restore must stay quarantined"}"#).unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
        let db = directory.path().join("state.db");
        if empty {
            std::fs::write(&db, []).unwrap();
            std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let error = match Store::open(directory.path()) {
            Ok(_) => panic!("restore created an empty scheduling database"),
            Err(error) => error,
        };
        assert_eq!(error.code, "RESTORE_REQUIRED");
        assert_eq!(db.exists(), empty);
        if empty {
            assert_eq!(std::fs::metadata(db).unwrap().len(), 0);
        }
    }
}
#[test]
fn incomplete_restore_installation_blocks_even_an_existing_valid_database() {
    let (d, s) = setup();
    drop(s);
    let db = d.path().join("state.db");
    let before = std::fs::read(&db).unwrap();
    let installing = d.path().join("restore-installing");
    std::fs::write(&installing, []).unwrap();
    std::fs::set_permissions(&installing, std::fs::Permissions::from_mode(0o600)).unwrap();
    let error = match Store::open(d.path()) {
        Ok(_) => panic!("incomplete restore installation was opened"),
        Err(error) => error,
    };
    assert_eq!(error.code, "RESTORE_REQUIRED");
    assert_eq!(std::fs::read(db).unwrap(), before);
}
