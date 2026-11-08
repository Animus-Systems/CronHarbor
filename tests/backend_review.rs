use cronharbor::{
    application,
    config::Manifest,
    contracts::{AppCommand, Role},
    external::{DockerSpec, ExternalHandle, SystemdSpec},
    process::{EndState, Outcome},
    store::{Launch, Store},
};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;

fn fixture(kind: &str) -> (tempfile::TempDir, Store, Manifest) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = Store::open(directory.path()).unwrap();
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.kind = kind.into();
    manifest.spec.owner = "api".into();
    manifest.spec.service.desired_state = "STOPPED".into();
    (directory, store, manifest)
}
fn call(store: &mut Store, command: AppCommand) -> Value {
    application::execute(store, command, None).unwrap()
}
fn operation(store: &mut Store, accepted: &Value) -> Value {
    call(
        store,
        AppCommand::Operation {
            id: accepted["id"].as_str().unwrap().into(),
        },
    )
}
fn install(store: &mut Store, manifest: &Manifest) -> String {
    let identity = format!("{}/{}", manifest.metadata.namespace, manifest.metadata.name);
    let revision = store.manifest_for(&identity).ok().map(|row| row.1);
    store
        .apply_scoped(
            manifest,
            "API",
            revision.as_deref(),
            "local-owner",
            cronharbor::now_us(),
        )
        .unwrap()
}
fn run(store: &mut Store, id: &str, key: &str) -> Value {
    call(
        store,
        AppCommand::Run {
            id: id.into(),
            idempotency_key: key.into(),
        },
    )
}
fn desired(id: &str, goal: &str, key: &str, revision: Option<String>) -> AppCommand {
    AppCommand::ServiceDesired {
        id: id.into(),
        desired_state: goal.into(),
        idempotency_key: key.into(),
        expected_revision: revision,
    }
}
fn configure_adapter(manifest: &mut Manifest, adapter: &str) {
    manifest.spec.execution.adapter = adapter.into();
    manifest.spec.execution.docker = None;
    manifest.spec.execution.systemd = None;
    if adapter == "docker" {
        manifest.spec.execution.docker = Some(DockerSpec {
            image: format!("fixture@sha256:{}", "a".repeat(64)),
            user: "10001".into(),
            network: "none".into(),
            memory_bytes: 8 * 1024 * 1024,
            pids_limit: 8,
        });
    } else if adapter == "systemd" {
        manifest.spec.execution.command.clear();
        manifest.spec.execution.args.clear();
        manifest.spec.execution.systemd = Some(SystemdSpec {
            unit: "worker.service".into(),
        });
    } else {
        manifest.spec.execution.command = "/bin/echo".into();
        manifest.spec.execution.args.clear();
    }
}
fn running(store: &mut Store, launch: &Launch) {
    let now = cronharbor::now_us();
    store.start_requested(&launch.attempt_id, now).unwrap();
    let handle = match launch.manifest.spec.execution.adapter.as_str() {
        "docker" => cronharbor::external::docker_handle(&store.external_identity(launch).unwrap()),
        "systemd" => ExternalHandle::Systemd {
            unit: "worker.service".into(),
            since_us: Some(now),
        },
        _ => {
            store.running(launch, 1, now).unwrap();
            return;
        }
    };
    store
        .running_handle(launch, &serde_json::to_string(&handle).unwrap(), now)
        .unwrap();
}
fn credential(store: &mut Store, grants: &[&str]) -> String {
    call(
        store,
        AppCommand::CreateToken {
            role: Role::Admin,
            namespaces: vec!["*".into()],
            capabilities: grants.iter().map(|s| (*s).into()).collect(),
            ttl_seconds: 3600,
        },
    )["token"]
        .as_str()
        .unwrap()
        .into()
}
fn unknown(store: &mut Store, launch: &Launch) {
    store
        .finish(
            launch,
            &Outcome {
                state: EndState::Unknown,
                error_code: Some("EXECUTOR_UNAVAILABLE"),
                exit_code: None,
                log_bytes: 0,
                log_truncated: false,
            },
            cronharbor::now_us(),
        )
        .unwrap();
}

#[test]
fn manual_queue_admits_eligible_pinned_revision_beyond_256_blocked_requests() {
    for newer_limit in [1, 2] {
        let (_directory, mut store, mut manifest) = fixture("OneShotJob");
        install(&mut store, &manifest);
        let mut scoped_run = |namespace: &str| {
            call(
                &mut store,
                AppCommand::CreateToken {
                    role: Role::Operator,
                    namespaces: vec![namespace.into()],
                    capabilities: vec!["workload.run_preapproved".into()],
                    ttl_seconds: 3600,
                },
            )["token"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let token_a = scoped_run("local");
        let token_b = scoped_run("other");
        application::execute(
            &mut store,
            AppCommand::Run {
                id: "local/hello".into(),
                idempotency_key: "active".into(),
            },
            Some(&token_a),
        )
        .unwrap();
        let active = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
        running(&mut store, &active);
        unknown(&mut store, &active);
        for index in 0..256 {
            application::execute(
                &mut store,
                AppCommand::Run {
                    id: "local/hello".into(),
                    idempotency_key: format!("blocked-{index}"),
                },
                Some(&token_a),
            )
            .unwrap();
        }
        manifest.spec.concurrency.max_active = newer_limit;
        manifest.spec.concurrency.mode = "allow".into();
        install(&mut store, &manifest);
        manifest.metadata.namespace = "other".into();
        install(&mut store, &manifest);
        application::execute(
            &mut store,
            AppCommand::Run {
                id: "other/hello".into(),
                idempotency_key: "eligible".into(),
            },
            Some(&token_b),
        )
        .unwrap();
        let eligible = store
            .claim_due(cronharbor::now_us())
            .unwrap()
            .expect("blocked queue prefix starved another namespace");
        assert_eq!(eligible.manifest.metadata.namespace, "other");
        assert_eq!(eligible.manifest.spec.concurrency.max_active, newer_limit);
        call(
            &mut store,
            AppCommand::Reconcile {
                execution_id: active.execution_id,
                state: "SUCCEEDED".into(),
                evidence: "verified original effect completed".into(),
                idempotency_key: "release-a".into(),
            },
        );
        let queued_a = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
        assert_eq!(queued_a.manifest.metadata.namespace, "local");
        assert_eq!(queued_a.manifest.spec.concurrency.max_active, 1);
        let retained = application::execute(
            &mut store,
            AppCommand::Run {
                id: "local/hello".into(),
                idempotency_key: "blocked-0".into(),
            },
            Some(&token_a),
        )
        .unwrap();
        assert_eq!(retained["state"], "RUNNING");
    }
}

#[test]
fn reconcile_finishes_originating_run_and_waiting_stop_without_reviving_superseded_intent() {
    let (_directory, mut store, manifest) = fixture("OneShotJob");
    install(&mut store, &manifest);
    let accepted = run(&mut store, "local/hello", "original");
    let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    running(&mut store, &launch);
    unknown(&mut store, &launch);
    call(
        &mut store,
        AppCommand::Reconcile {
            execution_id: launch.execution_id,
            state: "SUCCEEDED".into(),
            evidence: "verified downstream completion".into(),
            idempotency_key: "reconcile-run".into(),
        },
    );
    assert_eq!(operation(&mut store, &accepted)["state"], "SUCCEEDED");
    assert!(operation(&mut store, &accepted)["error_code"].is_null());

    let (_directory, mut store, manifest) = fixture("Service");
    install(&mut store, &manifest);
    let start = call(&mut store, desired("local/hello", "RUNNING", "start", None));
    let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    running(&mut store, &launch);
    unknown(&mut store, &launch);
    let stop = call(&mut store, desired("local/hello", "STOPPED", "stop", None));
    call(
        &mut store,
        AppCommand::Reconcile {
            execution_id: launch.execution_id,
            state: "CANCELLED".into(),
            evidence: "verified unit stopped".into(),
            idempotency_key: "reconcile-stop".into(),
        },
    );
    assert_eq!(operation(&mut store, &stop)["state"], "SUCCEEDED");
    assert!(operation(&mut store, &stop)["error_code"].is_null());
    assert_eq!(
        operation(&mut store, &start)["error_code"],
        "OPERATION_SUPERSEDED"
    );
}

#[test]
fn cancel_authorizes_the_pinned_adapter_in_both_revision_directions() {
    for (old, current, grants, allowed) in [
        (
            "docker",
            "process",
            vec!["execution.read", "workload.run_preapproved"],
            false,
        ),
        (
            "systemd",
            "process",
            vec!["execution.read", "workload.run_preapproved"],
            false,
        ),
        (
            "process",
            "docker",
            vec!["execution.read", "workload.run_preapproved"],
            true,
        ),
        (
            "process",
            "systemd",
            vec!["execution.read", "workload.run_preapproved"],
            true,
        ),
        (
            "docker",
            "process",
            vec![
                "execution.read",
                "workload.run_preapproved",
                "docker.manage",
            ],
            true,
        ),
    ] {
        let (_directory, mut store, mut manifest) = fixture("OneShotJob");
        configure_adapter(&mut manifest, old);
        install(&mut store, &manifest);
        run(&mut store, "local/hello", "run");
        let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
        running(&mut store, &launch);
        configure_adapter(&mut manifest, current);
        install(&mut store, &manifest);
        let token = credential(&mut store, &grants);
        let result = application::execute(
            &mut store,
            AppCommand::Cancel {
                execution_id: launch.execution_id.clone(),
                idempotency_key: "cancel".into(),
            },
            Some(&token),
        );
        if allowed {
            assert!(result.is_ok(), "{old}->{current}: {result:?}");
        } else {
            assert_eq!(
                result.unwrap_err().code,
                "NOT_AUTHORIZED",
                "{old}->{current}"
            );
            assert!(!store.cancel_requested(&launch.attempt_id).unwrap());
        }
    }
}

#[test]
fn service_controls_and_revision_apply_require_every_active_pinned_adapter() {
    for adapter in ["docker", "systemd"] {
        for attempt_state in [
            "LAUNCH_INTENT_RECORDED",
            "START_REQUESTED",
            "RUNNING",
            "UNKNOWN",
        ] {
            for action in ["stop", "start", "restart", "apply", "batch"] {
                let (directory, mut store, mut manifest) = fixture("Service");
                configure_adapter(&mut manifest, adapter);
                install(&mut store, &manifest);
                call(
                    &mut store,
                    desired("local/hello", "RUNNING", "start-original", None),
                );
                let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
                if attempt_state == "START_REQUESTED" {
                    store
                        .start_requested(&launch.attempt_id, cronharbor::now_us())
                        .unwrap();
                } else if attempt_state != "LAUNCH_INTENT_RECORDED" {
                    running(&mut store, &launch);
                    if attempt_state == "UNKNOWN" {
                        unknown(&mut store, &launch);
                    }
                }
                configure_adapter(&mut manifest, "process");
                let revision = install(&mut store, &manifest);
                let token = credential(
                    &mut store,
                    &[
                        "service.control_preapproved",
                        "workload.edit_safe_config",
                        "executor.create_command",
                    ],
                );
                manifest.spec.timeout_seconds += 1;
                let command = match action {
                    "stop" | "start" => desired(
                        "local/hello",
                        if action == "stop" {
                            "STOPPED"
                        } else {
                            "RUNNING"
                        },
                        "control",
                        Some(revision.clone()),
                    ),
                    "restart" => AppCommand::ServiceRestart {
                        id: "local/hello".into(),
                        expected_revision: Some(revision.clone()),
                        idempotency_key: "control".into(),
                    },
                    "apply" => AppCommand::Apply {
                        definition: serde_json::to_string(&manifest).unwrap(),
                        expected_revision: Some(revision.clone()),
                        source_kind: "API".into(),
                        idempotency_key: "control".into(),
                    },
                    _ => {
                        let mut other = manifest.clone();
                        other.metadata.name = "must-rollback".into();
                        AppCommand::ApplyBatch {
                            definitions: vec![
                                serde_json::to_string(&other).unwrap(),
                                serde_json::to_string(&manifest).unwrap(),
                            ],
                            expected_revisions: vec![None, Some(revision.clone())],
                            source_kind: "API".into(),
                            idempotency_key: "control".into(),
                        }
                    }
                };
                let database = Connection::open(directory.path().join("state.db")).unwrap();
                let before: (String, bool) = database
                    .query_row(
                        "SELECT desired_state,restart_requested FROM service_instances",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                let result = application::execute(&mut store, command, Some(&token));
                assert_eq!(
                    result.unwrap_err().code,
                    "NOT_AUTHORIZED",
                    "{adapter}/{attempt_state}/{action}"
                );
                assert!(!store.cancel_requested(&launch.attempt_id).unwrap());
                assert_eq!(store.manifest_for("local/hello").unwrap().1, revision);
                assert!(store.manifest_for("local/must-rollback").is_err());
                let after: (String, bool) = database
                    .query_row(
                        "SELECT desired_state,restart_requested FROM service_instances",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(before, after);
            }
        }
    }
}

#[test]
fn local_file_apply_replays_before_current_legacy_policy_comparison() {
    for batch in [false, true] {
        let (directory, mut store, mut manifest) = fixture("OneShotJob");
        manifest.spec.owner = "file".into();
        store.apply(&manifest, 0).unwrap();
        // Model a previously accepted legacy definition; new explicit legacy fields are prohibited.
        manifest.spec.logs.max_disk_bytes = Some(268435456);
        let database = Connection::open(directory.path().join("state.db")).unwrap();
        let revision = store.manifest_for("local/hello").unwrap().1;
        let (json, hash) = manifest.canonical().unwrap();
        database
            .execute(
                "UPDATE workload_revisions SET spec_json=?1,spec_hash=?2 WHERE id=?3",
                params![json, hash, revision],
            )
            .unwrap();
        let definition = serde_json::to_string(&manifest).unwrap();
        let command = if batch {
            AppCommand::ApplyBatch {
                definitions: vec![definition],
                expected_revisions: vec![None],
                source_kind: "FILE".into(),
                idempotency_key: "original-file".into(),
            }
        } else {
            AppCommand::Apply {
                definition,
                expected_revision: None,
                source_kind: "FILE".into(),
                idempotency_key: "original-file".into(),
            }
        };
        let accepted = call(&mut store, command.clone());
        manifest.spec.logs.max_disk_bytes = None;
        manifest.spec.timeout_seconds += 1;
        let newer = store.apply(&manifest, 1).unwrap();
        let replay = call(&mut store, command.clone());
        assert_eq!(accepted["id"], replay["id"]);
        assert_eq!(store.manifest_for("local/hello").unwrap().1, newer);
        let token = credential(&mut store, &["*"]);
        assert_eq!(
            application::execute(&mut store, command, Some(&token))
                .unwrap_err()
                .code,
            "NOT_AUTHORIZED"
        );
        let changed = if batch {
            AppCommand::ApplyBatch {
                definitions: vec![serde_json::to_string(&manifest).unwrap()],
                expected_revisions: vec![None],
                source_kind: "FILE".into(),
                idempotency_key: "original-file".into(),
            }
        } else {
            AppCommand::Apply {
                definition: serde_json::to_string(&manifest).unwrap(),
                expected_revision: None,
                source_kind: "FILE".into(),
                idempotency_key: "original-file".into(),
            }
        };
        assert_eq!(
            application::execute(&mut store, changed, None)
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
    }
}

#[test]
fn terminal_reconciliation_finishes_cancel_and_run_with_their_own_outcomes() {
    for state in ["SUCCEEDED", "FAILED", "CANCELLED", "TIMED_OUT"] {
        let (_directory, mut store, manifest) = fixture("OneShotJob");
        install(&mut store, &manifest);
        let original = run(&mut store, "local/hello", "run");
        let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
        running(&mut store, &launch);
        let cancel = call(
            &mut store,
            AppCommand::Cancel {
                execution_id: launch.execution_id.clone(),
                idempotency_key: "cancel".into(),
            },
        );
        unknown(&mut store, &launch);
        assert_eq!(operation(&mut store, &cancel)["state"], "UNKNOWN");
        call(
            &mut store,
            AppCommand::Reconcile {
                execution_id: launch.execution_id,
                state: state.into(),
                evidence: "verified terminal outcome".into(),
                idempotency_key: "reconcile".into(),
            },
        );
        let result = operation(&mut store, &original);
        assert_eq!(
            result["state"],
            if state == "SUCCEEDED" {
                "SUCCEEDED"
            } else {
                "FAILED"
            }
        );
        assert_eq!(
            result["error_code"],
            if state == "SUCCEEDED" {
                Value::Null
            } else {
                Value::String("OPERATOR_RECONCILED".into())
            }
        );
        let result = operation(&mut store, &cancel);
        assert_eq!(result["state"], "SUCCEEDED");
        assert!(result["error_code"].is_null());
    }
}

#[test]
fn reconciliation_operation_failure_rolls_back_execution_and_original_operation() {
    let (directory, mut store, manifest) = fixture("OneShotJob");
    install(&mut store, &manifest);
    let original = run(&mut store, "local/hello", "run");
    let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    running(&mut store, &launch);
    unknown(&mut store, &launch);
    let database = Connection::open(directory.path().join("state.db")).unwrap();
    database.execute_batch("CREATE TRIGGER reject_original_completion BEFORE UPDATE OF state ON operations WHEN OLD.operation_kind='run' AND NEW.state='SUCCEEDED' BEGIN SELECT RAISE(ABORT,'injected completion failure'); END;").unwrap();
    assert!(
        application::execute(
            &mut store,
            AppCommand::Reconcile {
                execution_id: launch.execution_id.clone(),
                state: "SUCCEEDED".into(),
                evidence: "verified completion".into(),
                idempotency_key: "reconcile".into()
            },
            None
        )
        .is_err()
    );
    assert_eq!(operation(&mut store, &original)["state"], "UNKNOWN");
    let result = call(
        &mut store,
        AppCommand::Execution {
            id: launch.execution_id,
        },
    );
    assert_eq!(result["state"], "UNKNOWN");
    assert_eq!(
        store.log_info(&launch.attempt_id).unwrap().capture_state,
        cronharbor::contracts::LogCaptureState::Frozen
    );
}

#[test]
fn current_and_active_adapter_grants_allow_service_controls_and_updates() {
    for action in ["stop", "start", "restart", "apply", "batch"] {
        let (_directory, mut store, mut manifest) = fixture("Service");
        configure_adapter(&mut manifest, "docker");
        install(&mut store, &manifest);
        call(
            &mut store,
            desired("local/hello", "RUNNING", "start-original", None),
        );
        let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
        running(&mut store, &launch);
        configure_adapter(&mut manifest, "systemd");
        let revision = install(&mut store, &manifest);
        manifest.spec.timeout_seconds += 1;
        let command = match action {
            "stop" | "start" => desired(
                "local/hello",
                if action == "stop" {
                    "STOPPED"
                } else {
                    "RUNNING"
                },
                "control",
                Some(revision.clone()),
            ),
            "restart" => AppCommand::ServiceRestart {
                id: "local/hello".into(),
                expected_revision: Some(revision.clone()),
                idempotency_key: "control".into(),
            },
            "apply" => AppCommand::Apply {
                definition: serde_json::to_string(&manifest).unwrap(),
                expected_revision: Some(revision.clone()),
                source_kind: "API".into(),
                idempotency_key: "control".into(),
            },
            _ => AppCommand::ApplyBatch {
                definitions: vec![serde_json::to_string(&manifest).unwrap()],
                expected_revisions: vec![Some(revision.clone())],
                source_kind: "API".into(),
                idempotency_key: "control".into(),
            },
        };
        let token = credential(
            &mut store,
            &[
                "service.control_preapproved",
                "workload.edit_safe_config",
                "executor.create_command",
                "docker.manage",
                "systemd.manage",
            ],
        );
        assert!(
            application::execute(&mut store, command.clone(), Some(&token)).is_ok(),
            "{action}"
        );
    }
}

#[test]
fn terminal_adapter_history_does_not_restrict_future_service_controls() {
    let (_directory, mut store, mut manifest) = fixture("Service");
    configure_adapter(&mut manifest, "docker");
    install(&mut store, &manifest);
    call(
        &mut store,
        desired("local/hello", "RUNNING", "start-original", None),
    );
    let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    running(&mut store, &launch);
    store
        .finish(
            &launch,
            &Outcome {
                state: EndState::Succeeded,
                error_code: None,
                exit_code: Some(0),
                log_bytes: 0,
                log_truncated: false,
            },
            cronharbor::now_us(),
        )
        .unwrap();
    configure_adapter(&mut manifest, "process");
    let revision = install(&mut store, &manifest);
    let token = credential(&mut store, &["service.control_preapproved"]);
    assert!(
        application::execute(
            &mut store,
            desired("local/hello", "STOPPED", "stop", Some(revision)),
            Some(&token)
        )
        .is_ok()
    );
}

#[test]
fn frozen_native_and_existing_external_snapshot_evidence_cannot_resume_capture() {
    for adapter in ["process", "docker"] {
        for prior_bytes in [false, true] {
            let (directory, mut store, mut manifest) = fixture("OneShotJob");
            configure_adapter(&mut manifest, adapter);
            install(&mut store, &manifest);
            run(&mut store, "local/hello", "run");
            let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
            running(&mut store, &launch);
            if prior_bytes {
                store.prepare_log_capture(&launch).unwrap();
                assert_eq!(store.append_log(&launch, b"original evidence").unwrap(), 17);
            }
            drop(store);
            let store = Store::open(directory.path()).unwrap();
            assert_eq!(
                store.log_info(&launch.attempt_id).unwrap().capture_state,
                cronharbor::contracts::LogCaptureState::Frozen
            );
            // Only a fresh external snapshot may reopen an empty, error-free recovery stream.
            if adapter == "process" || prior_bytes {
                store.prepare_external_log_capture(&launch).unwrap();
                assert_eq!(store.append_log(&launch, b"replacement").unwrap(), 0);
                assert_eq!(
                    store.log_info(&launch.attempt_id).unwrap().capture_state,
                    cronharbor::contracts::LogCaptureState::Frozen
                );
            }
        }
    }
}

#[test]
fn stale_ready_observation_cannot_complete_superseded_or_new_revision_operations() {
    for changed_revision in [false, true] {
        let (directory, mut store, mut manifest) = fixture("Service");
        configure_adapter(&mut manifest, "docker");
        install(&mut store, &manifest);
        let original = call(
            &mut store,
            desired("local/hello", "RUNNING", "original", None),
        );
        let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
        running(&mut store, &launch);
        drop(store);
        let mut store = Store::open(directory.path()).unwrap();
        assert_eq!(operation(&mut store, &original)["state"], "UNKNOWN");
        let replacement = if changed_revision {
            manifest.spec.timeout_seconds += 1;
            install(&mut store, &manifest);
            call(
                &mut store,
                desired("local/hello", "RUNNING", "replacement", None),
            )
        } else {
            call(
                &mut store,
                desired("local/hello", "STOPPED", "replacement", None),
            )
        };
        let (recovered, handle) = store.recoverable_external().unwrap().pop().unwrap();
        store
            .running_handle(
                &recovered,
                &serde_json::to_string(&handle).unwrap(),
                cronharbor::now_us(),
            )
            .unwrap();
        store
            .service_observation(&recovered, "READY", cronharbor::now_us())
            .unwrap();
        assert_eq!(operation(&mut store, &original)["state"], "FAILED");
        assert_eq!(
            operation(&mut store, &original)["error_code"],
            if changed_revision {
                "REVISION_SUPERSEDED"
            } else {
                "OPERATION_SUPERSEDED"
            }
        );
        assert_eq!(operation(&mut store, &replacement)["state"], "ACCEPTED");
    }
}

#[test]
fn frozen_empty_external_stream_with_capture_failure_cannot_resume() {
    let (directory, mut store, mut manifest) = fixture("OneShotJob");
    configure_adapter(&mut manifest, "docker");
    install(&mut store, &manifest);
    run(&mut store, "local/hello", "run");
    let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    running(&mut store, &launch);
    store
        .stop_log_capture(&launch.attempt_id, "LOG_UNAVAILABLE", 0)
        .unwrap();
    drop(store);
    let store = Store::open(directory.path()).unwrap();
    store.prepare_external_log_capture(&launch).unwrap();
    assert_eq!(
        store.append_log(&launch, b"unproven replacement").unwrap(),
        0
    );
    let info = store.log_info(&launch.attempt_id).unwrap();
    assert_eq!(
        info.capture_state,
        cronharbor::contracts::LogCaptureState::Frozen
    );
    assert_eq!(info.last_error_code.as_deref(), Some("LOG_UNAVAILABLE"));
}

#[test]
fn manual_queue_uses_its_pinned_limit_when_current_revision_lowers_capacity() {
    let (_directory, mut store, mut manifest) = fixture("OneShotJob");
    manifest.spec.concurrency.mode = "allow".into();
    manifest.spec.concurrency.max_active = 2;
    install(&mut store, &manifest);
    run(&mut store, "local/hello", "active");
    let active = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    running(&mut store, &active);
    unknown(&mut store, &active);
    run(&mut store, "local/hello", "queued");
    manifest.spec.concurrency.max_active = 1;
    install(&mut store, &manifest);
    let queued = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    assert_eq!(queued.manifest.spec.concurrency.max_active, 2);
}

#[test]
fn scheduled_allow_backlog_cannot_hide_another_namespace_or_lose_pinned_intent() {
    let (directory, mut store, mut manifest) = fixture("ScheduledJob");
    let base = cronharbor::now_us();
    manifest.spec.schedule.r#type = "interval".into();
    manifest.spec.schedule.interval_seconds = 1;
    manifest.spec.schedule.anchor_at_us = base;
    manifest.spec.schedule.misfire.policy = "catch_up".into();
    manifest.spec.schedule.misfire.max_occurrences = 1000;
    manifest.spec.concurrency.mode = "allow".into();
    store
        .apply_scoped(&manifest, "API", None, "local-owner", base)
        .unwrap();
    let active = store.claim_due(base + 1_000_000).unwrap().unwrap();
    running(&mut store, &active);
    unknown(&mut store, &active);
    let mut other = manifest.clone();
    other.metadata.namespace = "other".into();
    other.spec.schedule.interval_seconds = 600;
    other.spec.schedule.anchor_at_us = base + 300_000_000;
    other.spec.schedule.misfire.policy = "skip".into();
    store
        .apply_scoped(&other, "API", None, "local-owner", base)
        .unwrap();
    let now = base + 301_000_000;
    assert!(store.claim_due(now).unwrap().is_none());
    let database = Connection::open(directory.path().join("state.db")).unwrap();
    let queued: i64 = database
        .query_row(
            "SELECT count(*) FROM occurrences WHERE state='QUEUED'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(queued > 256, "fixture did not produce the blocked prefix");
    let eligible = store
        .claim_due(now)
        .unwrap()
        .expect("scheduled blocked prefix starved another namespace");
    assert_eq!(eligible.manifest.metadata.namespace, "other");
    manifest.spec.concurrency.max_active = 2;
    let old_revision = store.manifest_for("local/hello").unwrap().1;
    store
        .apply_scoped(&manifest, "API", Some(&old_revision), "local-owner", now)
        .unwrap();
    assert!(
        store.claim_due(now).unwrap().is_none(),
        "current capacity must not replace queued revision policy"
    );
    call(
        &mut store,
        AppCommand::Reconcile {
            execution_id: active.execution_id,
            state: "SUCCEEDED".into(),
            evidence: "verified original occurrence completed".into(),
            idempotency_key: "release-scheduled".into(),
        },
    );
    let retained = store.claim_due(now).unwrap().unwrap();
    assert_eq!(retained.manifest.metadata.namespace, "local");
    assert_eq!(retained.manifest.spec.concurrency.max_active, 1);
    assert_eq!(retained.due_at_us, base + 2_000_000);
}

#[test]
fn verified_external_terminal_recovery_finishes_unknown_cancel_and_stop_controls() {
    for service in [true, false] {
        let (directory, mut store, mut manifest) =
            fixture(if service { "Service" } else { "OneShotJob" });
        configure_adapter(&mut manifest, "docker");
        install(&mut store, &manifest);
        let start = if service {
            call(&mut store, desired("local/hello", "RUNNING", "start", None))
        } else {
            run(&mut store, "local/hello", "run")
        };
        let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
        running(&mut store, &launch);
        let control = if service {
            call(&mut store, desired("local/hello", "STOPPED", "stop", None))
        } else {
            call(
                &mut store,
                AppCommand::Cancel {
                    execution_id: launch.execution_id.clone(),
                    idempotency_key: "cancel".into(),
                },
            )
        };
        unknown(&mut store, &launch);
        assert_eq!(operation(&mut store, &control)["state"], "UNKNOWN");
        drop(store);
        let mut store = Store::open(directory.path()).unwrap();
        let (recovered, handle) = store.recoverable_external().unwrap().pop().unwrap();
        store
            .finish_external(
                &recovered,
                &Outcome {
                    state: EndState::Cancelled,
                    error_code: Some("CANCELLED_BY_OPERATOR"),
                    exit_code: None,
                    log_bytes: 0,
                    log_truncated: false,
                },
                false,
                &handle,
                cronharbor::now_us(),
            )
            .unwrap();
        let result = operation(&mut store, &control);
        assert_eq!(result["state"], "SUCCEEDED", "service={service}");
        assert!(
            result["error_code"].is_null(),
            "verified control retained stale uncertainty: {result}"
        );
        if service {
            assert_eq!(
                operation(&mut store, &start)["error_code"],
                "OPERATION_SUPERSEDED"
            );
        }
    }
}

#[test]
fn recovered_start_and_restart_fail_when_terminal_evidence_exhausts_the_goal() {
    for restart in [false, true] {
        for (policy, budget) in [("never", 5), ("on-failure", 0)] {
            for success in [false, true] {
                let (directory, mut store, mut manifest) = fixture("Service");
                configure_adapter(&mut manifest, "docker");
                manifest.spec.service.restart = policy.into();
                manifest.spec.service.max_restarts = budget;
                install(&mut store, &manifest);
                let command = if restart {
                    AppCommand::ServiceRestart {
                        id: "local/hello".into(),
                        expected_revision: None,
                        idempotency_key: "goal".into(),
                    }
                } else {
                    desired("local/hello", "RUNNING", "goal", None)
                };
                let accepted = call(&mut store, command);
                let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
                running(&mut store, &launch);
                drop(store);
                let mut store = Store::open(directory.path()).unwrap();
                let (recovered, handle) = store.recoverable_external().unwrap().pop().unwrap();
                assert_eq!(operation(&mut store, &accepted)["state"], "UNKNOWN");
                store
                    .finish_external(
                        &recovered,
                        &Outcome {
                            state: if success {
                                EndState::Succeeded
                            } else {
                                EndState::Failed
                            },
                            error_code: if success { None } else { Some("EXIT_NONZERO") },
                            exit_code: Some(if success { 0 } else { 1 }),
                            log_bytes: 0,
                            log_truncated: false,
                        },
                        false,
                        &handle,
                        cronharbor::now_us(),
                    )
                    .unwrap();
                let result = operation(&mut store, &accepted);
                assert_eq!(
                    result["state"], "FAILED",
                    "restart={restart}, policy={policy}, success={success}"
                );
                assert_eq!(
                    result["error_code"],
                    if success {
                        "SERVICE_NOT_READY"
                    } else {
                        "EXIT_NONZERO"
                    }
                );
                assert!(
                    store
                        .claim_due(cronharbor::now_us() + 999_000_000)
                        .unwrap()
                        .is_none()
                );
            }
        }
    }
}

fn enqueue_occurrence(database: &Connection, namespace: &str, due: i64, now: i64) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    database.execute("INSERT INTO occurrences(id,schedule_id,workload_id,revision_id,scheduled_at_us,state,created_at_us) SELECT ?1,s.id,s.workload_id,s.revision_id,?2,'QUEUED',?3 FROM schedules s JOIN workloads w ON w.id=s.workload_id WHERE w.namespace=?4 AND s.active=1", params![id,due,now,namespace]).unwrap();
    id
}

#[test]
fn scheduled_cleanup_is_bounded_and_does_not_hide_eligible_work() {
    for forbid in [false, true] {
        let (directory, mut store, mut manifest) = fixture("ScheduledJob");
        let now = cronharbor::now_us();
        manifest.spec.schedule.r#type = "interval".into();
        manifest.spec.schedule.interval_seconds = 86400;
        manifest.spec.schedule.anchor_at_us = now;
        store
            .apply_scoped(&manifest, "API", None, "local-owner", now)
            .unwrap();
        if forbid {
            run(&mut store, "local/hello", "active");
            let active = store.claim_due(now).unwrap().unwrap();
            running(&mut store, &active);
            unknown(&mut store, &active);
        }
        manifest.metadata.namespace = "other".into();
        store
            .apply_scoped(&manifest, "API", None, "local-owner", now)
            .unwrap();
        let database = Connection::open(directory.path().join("state.db")).unwrap();
        for index in 0..300 {
            enqueue_occurrence(
                &database,
                "local",
                if forbid {
                    now - index
                } else {
                    now - 61_000_000 - index
                },
                now,
            );
        }
        enqueue_occurrence(&database, "other", now, now);
        let eligible = store
            .claim_due(now)
            .unwrap()
            .expect("bounded skip bookkeeping hid admissible work");
        assert_eq!(eligible.manifest.metadata.namespace, "other");
        let counts: (i64, i64) = database.query_row("SELECT SUM(state='SKIPPED'),SUM(state='QUEUED') FROM occurrences o JOIN workloads w ON w.id=o.workload_id WHERE w.namespace='local'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(counts, (256, 44));
        let reason: String = database
            .query_row(
                "SELECT DISTINCT reason FROM occurrences WHERE state='SKIPPED'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            reason,
            if forbid {
                "OVERLAP_OR_UNKNOWN"
            } else {
                "MISFIRE_EXPIRED_AT_ADMISSION"
            }
        );
    }
}

#[test]
fn scheduled_skip_grace_is_inclusive_and_expiry_precedes_forbid_reason() {
    for active in [false, true] {
        let (directory, mut store, mut manifest) = fixture("ScheduledJob");
        let now = cronharbor::now_us();
        manifest.spec.schedule.r#type = "interval".into();
        manifest.spec.schedule.interval_seconds = 86400;
        manifest.spec.schedule.anchor_at_us = now;
        store
            .apply_scoped(&manifest, "API", None, "local-owner", now)
            .unwrap();
        if active {
            run(&mut store, "local/hello", "active");
            let launch = store.claim_due(now).unwrap().unwrap();
            running(&mut store, &launch);
            unknown(&mut store, &launch);
        }
        let database = Connection::open(directory.path().join("state.db")).unwrap();
        let expired = enqueue_occurrence(&database, "local", now - 60_000_001, now);
        let inclusive = enqueue_occurrence(&database, "local", now - 60_000_000, now);
        let admitted = store.claim_due(now).unwrap();
        assert_eq!(admitted.is_some(), !active);
        let expired_reason: String = database
            .query_row(
                "SELECT reason FROM occurrences WHERE id=?1",
                [&expired],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(expired_reason, "MISFIRE_EXPIRED_AT_ADMISSION");
        let result: (String, Option<String>) = database
            .query_row(
                "SELECT state,reason FROM occurrences WHERE id=?1",
                [&inclusive],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            result,
            if active {
                ("SKIPPED".into(), Some("OVERLAP_OR_UNKNOWN".into()))
            } else {
                ("DISPATCHED".into(), None)
            }
        );
    }
}

#[test]
fn recovered_running_goal_stays_unknown_through_backoff_until_replacement_ready() {
    let (directory, mut store, mut manifest) = fixture("Service");
    configure_adapter(&mut manifest, "docker");
    manifest.spec.service.restart = "on-failure".into();
    manifest.spec.service.max_restarts = 1;
    install(&mut store, &manifest);
    let accepted = call(&mut store, desired("local/hello", "RUNNING", "start", None));
    let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    running(&mut store, &launch);
    drop(store);
    let mut store = Store::open(directory.path()).unwrap();
    let (recovered, handle) = store.recoverable_external().unwrap().pop().unwrap();
    let unknown_outcome = Outcome {
        state: EndState::Unknown,
        error_code: Some("EXECUTOR_UNAVAILABLE"),
        exit_code: None,
        log_bytes: 0,
        log_truncated: false,
    };
    store
        .finish_external(
            &recovered,
            &unknown_outcome,
            false,
            &handle,
            cronharbor::now_us(),
        )
        .unwrap();
    assert_eq!(operation(&mut store, &accepted)["state"], "UNKNOWN");
    assert!(
        store
            .claim_due(cronharbor::now_us() + 10_000_000)
            .unwrap()
            .is_none()
    );
    store
        .finish_external(
            &recovered,
            &Outcome {
                state: EndState::Failed,
                error_code: Some("EXIT_NONZERO"),
                exit_code: Some(1),
                log_bytes: 0,
                log_truncated: false,
            },
            false,
            &handle,
            cronharbor::now_us(),
        )
        .unwrap();
    assert_eq!(operation(&mut store, &accepted)["state"], "UNKNOWN");
    let replacement = store
        .claim_due(cronharbor::now_us() + 10_000_000)
        .unwrap()
        .unwrap();
    assert_ne!(replacement.execution_id, recovered.execution_id);
    running(&mut store, &replacement);
    store
        .service_observation(&replacement, "READY", cronharbor::now_us())
        .unwrap();
    let result = operation(&mut store, &accepted);
    assert_eq!(result["state"], "SUCCEEDED");
    assert!(result["error_code"].is_null());
}

#[test]
fn unknown_stop_waits_for_every_uncertain_attempt_and_preserves_superseded_failure() {
    let (directory, mut store, mut manifest) = fixture("Service");
    configure_adapter(&mut manifest, "docker");
    install(&mut store, &manifest);
    let start = call(&mut store, desired("local/hello", "RUNNING", "start", None));
    let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    running(&mut store, &launch);
    let stop = call(&mut store, desired("local/hello", "STOPPED", "stop", None));
    unknown(&mut store, &launch);
    // Model additional uncertain evidence; normal admission never bypasses the first reservation.
    let other = Launch {
        execution_id: uuid::Uuid::new_v4().to_string(),
        attempt_id: uuid::Uuid::new_v4().to_string(),
        launch_nonce: uuid::Uuid::new_v4().to_string(),
        ..launch.clone()
    };
    let database = Connection::open(directory.path().join("state.db")).unwrap();
    database.execute("INSERT INTO executions(id,workload_id,revision_id,trigger_kind,created_at_us,state) SELECT ?1,workload_id,revision_id,'INTERNAL',created_at_us,'CREATED' FROM executions WHERE id=?2", params![other.execution_id,launch.execution_id]).unwrap();
    database.execute("INSERT INTO attempts(id,execution_id,attempt_index,launch_nonce,state,executor_kind,created_at_us) VALUES(?1,?2,1,?3,'LAUNCH_INTENT_RECORDED','docker',?4)", params![other.attempt_id,other.execution_id,other.launch_nonce,cronharbor::now_us()]).unwrap();
    running(&mut store, &other);
    unknown(&mut store, &other);
    drop(store);
    let mut store = Store::open(directory.path()).unwrap();
    let outcome = Outcome {
        state: EndState::Cancelled,
        error_code: Some("CANCELLED_BY_OPERATOR"),
        exit_code: None,
        log_bytes: 0,
        log_truncated: false,
    };
    let attempts = store.recoverable_external().unwrap();
    for (index, (recovered, handle)) in attempts.iter().enumerate() {
        store
            .finish_external(recovered, &outcome, false, handle, cronharbor::now_us())
            .unwrap();
        assert_eq!(
            operation(&mut store, &stop)["state"],
            if index == 0 { "UNKNOWN" } else { "SUCCEEDED" }
        );
    }
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        operation(&mut store, &start)["error_code"],
        "OPERATION_SUPERSEDED"
    );
}

#[test]
fn old_revision_failure_cannot_finish_a_new_running_goal() {
    let (directory, mut store, mut manifest) = fixture("Service");
    configure_adapter(&mut manifest, "docker");
    manifest.spec.service.restart = "never".into();
    install(&mut store, &manifest);
    let original = call(
        &mut store,
        desired("local/hello", "RUNNING", "original", None),
    );
    let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    running(&mut store, &launch);
    drop(store);
    let mut store = Store::open(directory.path()).unwrap();
    let (recovered, handle) = store.recoverable_external().unwrap().pop().unwrap();
    manifest.spec.timeout_seconds += 1;
    install(&mut store, &manifest);
    let replacement_goal = call(
        &mut store,
        desired("local/hello", "RUNNING", "replacement", None),
    );
    store
        .finish_external(
            &recovered,
            &Outcome {
                state: EndState::Failed,
                error_code: Some("EXIT_NONZERO"),
                exit_code: Some(1),
                log_bytes: 0,
                log_truncated: false,
            },
            false,
            &handle,
            cronharbor::now_us(),
        )
        .unwrap();
    assert_eq!(
        operation(&mut store, &original)["error_code"],
        "REVISION_SUPERSEDED"
    );
    assert_eq!(
        operation(&mut store, &replacement_goal)["state"],
        "ACCEPTED"
    );
    let replacement = store
        .claim_due(cronharbor::now_us() + 10_000_000)
        .unwrap()
        .unwrap();
    assert_ne!(replacement.execution_id, recovered.execution_id);
    running(&mut store, &replacement);
    store
        .service_observation(&replacement, "READY", cronharbor::now_us())
        .unwrap();
    assert_eq!(
        operation(&mut store, &replacement_goal)["state"],
        "SUCCEEDED"
    );
}

#[test]
fn scheduled_catch_up_expiry_uses_the_pinned_inclusive_age_limit() {
    let (directory, mut store, mut manifest) = fixture("ScheduledJob");
    let now = cronharbor::now_us();
    manifest.spec.schedule.r#type = "interval".into();
    manifest.spec.schedule.interval_seconds = 86400;
    manifest.spec.schedule.anchor_at_us = now;
    manifest.spec.schedule.misfire.policy = "catch_up".into();
    manifest.spec.schedule.misfire.max_age_seconds = 100;
    let revision = store
        .apply_scoped(&manifest, "API", None, "local-owner", now)
        .unwrap();
    let database = Connection::open(directory.path().join("state.db")).unwrap();
    let expired = enqueue_occurrence(&database, "local", now - 100_000_001, now);
    enqueue_occurrence(&database, "local", now - 100_000_000, now);
    manifest.spec.schedule.misfire.max_age_seconds = 1;
    store
        .apply_scoped(&manifest, "API", Some(&revision), "local-owner", now)
        .unwrap();
    let admitted = store.claim_due(now).unwrap().unwrap();
    assert_eq!(admitted.due_at_us, now - 100_000_000);
    assert_eq!(admitted.manifest.spec.schedule.misfire.max_age_seconds, 100);
    let reason: String = database
        .query_row(
            "SELECT reason FROM occurrences WHERE id=?1",
            [&expired],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reason, "MISFIRE_EXPIRED_AT_ADMISSION");
}
