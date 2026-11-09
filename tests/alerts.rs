use cronharbor::{application, contracts::AppCommand, store::Store};
use serde_json::json;

#[test]
fn alert_schema_is_present_and_disabled_installations_do_not_capture_history() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(&directory.path().join("state")).unwrap();
    let db = rusqlite::Connection::open(directory.path().join("state/state.db")).unwrap();
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5, "outbound alert state needs its own migration");
    let command = serde_json::from_value::<AppCommand>(json!({"action":"alert_status"})).unwrap();
    let status = application::execute(&mut store, command, None).unwrap();
    assert_eq!(status["configuration_state"], "disabled");
    assert_eq!(status["outstanding"], 0);
}

#[cfg(feature = "webhooks")]
mod enabled {
    use super::*;
    use cronharbor::{
        alerts::{AlertReceipt, AlertResult, DELIVERY_LIFETIME_US},
        config::{AlertConfig, Manifest, SecretRef},
        contracts::{AlertDeliveryState, AlertErrorCode, AlertKind},
        process::{EndState, Outcome},
        store::Launch,
    };
    use rusqlite::{Connection, params};
    fn config() -> AlertConfig {
        AlertConfig {
            endpoint: "https://example.com/alerts".into(),
            token_ref: SecretRef::Env {
                name: "CRONHARBOR_ALERT_TEST_TOKEN".into(),
            },
            allowed_private_addresses: vec![],
            ca_file: None,
        }
    }
    fn setup(kind: &str) -> (tempfile::TempDir, Store, Manifest, i64) {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            Store::open_with_alerts(&dir.path().join("state"), Some(&config())).unwrap();
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.kind = kind.into();
        if kind == "Service" {
            manifest.spec.service.desired_state = "RUNNING".into();
        }
        let at = cronharbor::now_us();
        store.apply(&manifest, at).unwrap();
        (dir, store, manifest, at)
    }
    fn db(store: &Store) -> Connection {
        Connection::open(store.directory.join("state.db")).unwrap()
    }
    fn launch(store: &mut Store, at: i64) -> Launch {
        application::execute(
            store,
            AppCommand::Run {
                id: "local/hello".into(),
                idempotency_key: uuid::Uuid::new_v4().to_string(),
            },
            None,
        )
        .unwrap();
        let launch = store.claim_due(at).unwrap().unwrap();
        store.start_requested(&launch.attempt_id, at).unwrap();
        launch
    }
    fn result(store: &mut Store, launch: &Launch, state: EndState, at: i64) {
        store.finish(launch, &outcome(state), at).unwrap();
    }
    fn outcome(state: EndState) -> Outcome {
        Outcome {
            state,
            error_code: match state {
                EndState::Failed => Some("EXIT_NONZERO"),
                EndState::TimedOut => Some("DEADLINE_EXCEEDED"),
                EndState::Unknown => Some("EXECUTOR_UNAVAILABLE"),
                _ => None,
            },
            exit_code: None,
            log_bytes: 0,
            log_truncated: false,
        }
    }
    fn items(store: &Store) -> Vec<cronharbor::contracts::AlertDeliveryDto> {
        store.alert_deliveries(None, 100).unwrap().items
    }
    fn receipt(job: cronharbor::alerts::AlertDispatch, receipt: AlertReceipt) -> AlertResult {
        AlertResult {
            alert_id: job.alert_id,
            nonce: job.nonce,
            destination_hash: job.destination_hash,
            receipt,
        }
    }

    #[test]
    fn final_job_failure_is_captured_after_retry_exhaustion_without_secret_or_log_data() {
        let (_dir, mut store, mut manifest, at) = setup("OneShotJob");
        manifest.spec.retry.policy = "idempotent".into();
        manifest.spec.retry.max_attempts = 2;
        manifest
            .spec
            .execution
            .env
            .insert("VISIBLE_CANARY".into(), "do-not-export".into());
        store.apply(&manifest, at).unwrap();
        let first = launch(&mut store, at + 1);
        result(&mut store, &first, EndState::Failed, at + 2);
        assert!(items(&store).is_empty());
        let second = store.claim_due(at + 1_000_003).unwrap().unwrap();
        store
            .start_requested(&second.attempt_id, at + 1_000_003)
            .unwrap();
        result(&mut store, &second, EndState::Failed, at + 1_000_004);
        let rows = items(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, AlertKind::JobFailed);
        let payload: String = db(&store)
            .query_row("SELECT payload_json FROM alert_deliveries", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!payload.contains("do-not-export"));
        assert!(!payload.contains("VISIBLE_CANARY"));
        assert!(!payload.contains("/bin/"));
        let parsed: cronharbor::contracts::AlertEnvelope = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            parsed.execution_id.as_deref(),
            Some(second.execution_id.as_str())
        );
        assert_eq!(parsed.schema_version, 1);
        assert!(parsed.summary.len() <= 512);
    }
    #[test]
    fn timeout_and_unknown_are_distinct_and_startup_unknown_is_captured_once() {
        let (dir, mut store, _, at) = setup("OneShotJob");
        let timed = launch(&mut store, at);
        result(&mut store, &timed, EndState::TimedOut, at + 1);
        let unknown = launch(&mut store, at + 2);
        result(&mut store, &unknown, EndState::Unknown, at + 3);
        assert_eq!(items(&store).len(), 2);
        store.scan_current_alerts(at + 4).unwrap();
        assert_eq!(items(&store).len(), 2);
        drop(store);
        let mut store =
            Store::open_with_alerts(&dir.path().join("state"), Some(&config())).unwrap();
        store.scan_current_alerts(at + 5).unwrap();
        assert_eq!(items(&store).len(), 2);
        assert!(
            store.claim_due(at + 6).unwrap().is_none(),
            "UNKNOWN retains capacity"
        );
        let rows = items(&store);
        assert!(rows.iter().any(|r| r.kind == AlertKind::JobTimedOut));
        assert!(rows.iter().any(|r| r.kind == AlertKind::ExecutionUnknown));
    }
    #[test]
    fn enabling_scans_current_unknown_but_never_historical_terminal_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        let mut store = Store::open(&path).unwrap();
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.kind = "OneShotJob".into();
        let at = cronharbor::now_us();
        store.apply(&manifest, at).unwrap();
        let failed = launch(&mut store, at);
        result(&mut store, &failed, EndState::Failed, at + 1);
        let unknown = launch(&mut store, at + 2);
        drop(store);
        let mut store = Store::open_with_alerts(&path, Some(&config())).unwrap();
        store.scan_current_alerts(at + 3).unwrap();
        let rows = items(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, AlertKind::ExecutionUnknown);
        let marker: String = db(&store)
            .query_row(
                "SELECT alert_failure_outcome FROM executions WHERE id=?1",
                [failed.execution_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(marker, "NONE");
        let state: String = db(&store)
            .query_row(
                "SELECT state FROM attempts WHERE id=?1",
                [unknown.attempt_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "UNKNOWN");
    }
    #[test]
    fn service_incident_survives_revisions_and_stop_and_ready_closes_the_same_incident() {
        let (_dir, mut store, mut manifest, at) = setup("Service");
        let first = store.claim_due(at).unwrap().unwrap();
        store.start_requested(&first.attempt_id, at).unwrap();
        store.running(&first, 12345, at).unwrap();
        store
            .service_observation(&first, "STARTING", at + 1)
            .unwrap();
        assert!(items(&store).is_empty());
        store
            .service_observation(&first, "UNHEALTHY", at + 2)
            .unwrap();
        store
            .service_observation(&first, "UNHEALTHY", at + 3)
            .unwrap();
        assert_eq!(items(&store).len(), 1);
        let incident = items(&store)[0].incident_id.clone();
        manifest.spec.timeout_seconds += 1;
        store.apply(&manifest, at + 4).unwrap();
        application::execute(
            &mut store,
            AppCommand::ServiceDesired {
                id: "local/hello".into(),
                desired_state: "STOPPED".into(),
                expected_revision: None,
                idempotency_key: "stop".into(),
            },
            None,
        )
        .unwrap();
        result(&mut store, &first, EndState::Cancelled, at + 5);
        assert_eq!(items(&store).len(), 1);
        let persisted: String = db(&store)
            .query_row("SELECT alert_incident_id FROM service_instances", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(persisted, incident);
        application::execute(
            &mut store,
            AppCommand::ServiceDesired {
                id: "local/hello".into(),
                desired_state: "RUNNING".into(),
                expected_revision: None,
                idempotency_key: "start".into(),
            },
            None,
        )
        .unwrap();
        let second = store.claim_due(at + 6).unwrap().unwrap();
        store.start_requested(&second.attempt_id, at + 6).unwrap();
        store.running(&second, 12346, at + 6).unwrap();
        store.service_observation(&second, "READY", at + 7).unwrap();
        let rows = items(&store);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.incident_id == incident));
        assert_eq!(rows[0].incident_version, 2);
        assert_eq!(rows[0].kind, AlertKind::ServiceRecovered);
        let payloads: Vec<String> = db(&store)
            .prepare("SELECT payload_json FROM alert_deliveries ORDER BY incident_version")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        let parsed: Vec<cronharbor::contracts::AlertEnvelope> = payloads
            .iter()
            .map(|p| serde_json::from_str(p).unwrap())
            .collect();
        assert_eq!(
            parsed[0].incident_opened_at_us,
            parsed[1].incident_opened_at_us
        );
    }
    #[test]
    fn capture_failure_rolls_back_verified_source_transition_instead_of_losing_the_alert() {
        let (_dir, mut store, _, at) = setup("OneShotJob");
        let launched = launch(&mut store, at);
        db(&store).execute_batch("CREATE TRIGGER fail_alert BEFORE INSERT ON alert_deliveries BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        assert_eq!(
            store
                .finish(&launched, &outcome(EndState::Failed), at + 1)
                .unwrap_err()
                .code,
            "PERSISTENCE_UNAVAILABLE"
        );
        let state: String = db(&store)
            .query_row(
                "SELECT state FROM attempts WHERE id=?1",
                [&launched.attempt_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "START_REQUESTED");
        db(&store).execute_batch("DROP TRIGGER fail_alert").unwrap();
        result(&mut store, &launched, EndState::Failed, at + 2);
        assert_eq!(items(&store).len(), 1);
    }
    fn seed(store: &Store, count: u32, terminal: bool, at: i64) {
        let connection = db(store);
        let (workload, revision) = store
            .manifest_for("local/hello")
            .map(|(id, rev, _)| (id, rev))
            .unwrap();
        let destination = cronharbor::webhooks::destination_hash(&config()).unwrap();
        for index in 0..count {
            let id = uuid::Uuid::new_v4().to_string();
            let incident = uuid::Uuid::new_v4().to_string();
            let payload=json!({"schema_version":1,"alert_id":id,"instance_id":"fixture","incident_id":incident,"incident_version":1,"kind":"job_failed","severity":"error","occurred_at_us":at,"incident_opened_at_us":at,"workload_id":workload,"namespace":"local","workload_name":"hello","revision_id":revision,"execution_id":null,"attempt_id":null,"error_code":null,"summary":"Job failed after its permitted attempts."}).to_string();
            connection.execute("INSERT INTO alert_deliveries(id,source_key,incident_id,incident_version,kind,destination_hash,workload_id,payload_json,state,created_at_us,updated_at_us,expires_at_us,next_attempt_at_us,finished_at_us) VALUES(?1,?2,?3,1,'job_failed',?4,?5,?6,?7,?8,?8,?9,?8,?10)",params![id,format!("fixture:{index}"),incident,destination,workload,payload,if terminal{"FAILED"}else{"PENDING"},at,at+DELIVERY_LIFETIME_US,if terminal{Some(at)}else{None}]).unwrap();
        }
    }
    #[test]
    fn full_queue_suppresses_once_without_rolling_back_the_source_result() {
        let (_dir, mut store, _, at) = setup("OneShotJob");
        seed(&store, 1000, false, at);
        let launched = launch(&mut store, at + 1);
        result(&mut store, &launched, EndState::Failed, at + 2);
        let status = store.alert_status().unwrap();
        assert_eq!(status.outstanding, 1000);
        assert_eq!(status.queue_overflow_count, 1);
        let (state, marker): (String, String) = db(&store)
            .query_row(
                "SELECT state,alert_failure_outcome FROM executions WHERE id=?1",
                [launched.execution_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "FAILED");
        assert_eq!(marker, "SUPPRESSED");
        store.scan_current_alerts(at + 3).unwrap();
        assert_eq!(store.alert_status().unwrap().queue_overflow_count, 1);
    }
    #[test]
    fn terminal_transition_enforces_history_bound_in_the_same_transaction() {
        let (_dir, mut store, _, at) = setup("OneShotJob");
        seed(&store, 1000, true, at);
        let launched = launch(&mut store, at + 1);
        result(&mut store, &launched, EndState::Failed, at + 2);
        let job = store.claim_alert(at + 3).unwrap().unwrap();
        store
            .complete_alert(
                receipt(job, AlertReceipt::Accepted { http_status: 202 }),
                at + 4,
            )
            .unwrap();
        let count: u32 = db(&store)
            .query_row("SELECT count(*) FROM alert_deliveries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1000);
        assert_eq!(store.alert_status().unwrap().history_eviction_count, 1);
    }
    #[test]
    fn late_completion_is_rejected_and_automatic_retries_keep_exact_payload() {
        let (_dir, mut store, _, at) = setup("OneShotJob");
        let launched = launch(&mut store, at);
        result(&mut store, &launched, EndState::Failed, at + 1);
        let first = store.claim_alert(at + 2).unwrap().unwrap();
        let stale = receipt(first.clone(), AlertReceipt::Accepted { http_status: 200 });
        store
            .complete_alert(
                receipt(
                    first.clone(),
                    AlertReceipt::Retry {
                        code: AlertErrorCode::RequestTimeout,
                        http_status: None,
                        retry_after_seconds: Some(100),
                    },
                ),
                at + 3,
            )
            .unwrap();
        assert!(store.claim_alert(at + 99_000_003).unwrap().is_none());
        let second = store.claim_alert(at + 100_000_003).unwrap().unwrap();
        assert_eq!(first.payload_json, second.payload_json);
        assert_ne!(first.nonce, second.nonce);
        assert!(!store.complete_alert(stale, at + 100_000_004).unwrap());
        assert_eq!(items(&store)[0].state, AlertDeliveryState::InFlight);
        store
            .complete_alert(
                receipt(second, AlertReceipt::Accepted { http_status: 204 }),
                at + 100_000_005,
            )
            .unwrap();
        assert_eq!(items(&store)[0].state, AlertDeliveryState::Delivered);
    }
    #[test]
    fn manual_retry_is_idempotent_and_allows_exactly_one_extra_send() {
        let (_dir, mut store, _, at) = setup("OneShotJob");
        let launched = launch(&mut store, at);
        result(&mut store, &launched, EndState::Failed, at + 1);
        let id = items(&store)[0].alert_id.clone();
        db(&store).execute("UPDATE alert_deliveries SET state='EXPIRED',attempt_count=12,finished_at_us=?1,next_attempt_at_us=NULL,expires_at_us=?1 WHERE id=?2",params![at,id]).unwrap();
        let command = AppCommand::RetryAlert {
            id: id.clone(),
            idempotency_key: "one-send".into(),
        };
        let operation = application::execute(&mut store, command.clone(), None).unwrap();
        assert_eq!(operation["state"], "ACCEPTED");
        assert_eq!(
            application::execute(&mut store, command.clone(), None).unwrap()["id"],
            operation["id"]
        );
        let job = store
            .claim_alert(cronharbor::now_us() + 1)
            .unwrap()
            .unwrap();
        store
            .complete_alert(
                receipt(
                    job,
                    AlertReceipt::Retry {
                        code: AlertErrorCode::TransportUnavailable,
                        http_status: None,
                        retry_after_seconds: None,
                    },
                ),
                at + 3,
            )
            .unwrap();
        assert_eq!(items(&store)[0].attempt_count, 13);
        assert_eq!(items(&store)[0].state, AlertDeliveryState::Failed);
        assert!(store.claim_alert(at + 999_000_000).unwrap().is_none());
        let replay = application::execute(&mut store, command, None).unwrap();
        assert_eq!(replay["id"], operation["id"]);
        assert_eq!(replay["state"], "FAILED");
    }
    #[test]
    fn accepted_alert_retry_without_a_delivery_link_is_failed_on_recovery() {
        use sha2::{Digest, Sha256};
        let (dir, mut store, _, at) = setup("OneShotJob");
        let launched = launch(&mut store, at);
        result(&mut store, &launched, EndState::Failed, at + 1);
        let job = store.claim_alert(at + 2).unwrap().unwrap();
        store
            .complete_alert(
                receipt(
                    job,
                    AlertReceipt::Failed {
                        code: AlertErrorCode::HttpRejected,
                        http_status: Some(400),
                    },
                ),
                at + 3,
            )
            .unwrap();
        let id = items(&store)[0].alert_id.clone();
        let key = "interrupted-before-retry-link";
        let command = AppCommand::RetryAlert {
            id: id.clone(),
            idempotency_key: key.into(),
        };
        let operation_id = uuid::Uuid::new_v4().to_string();
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&command).unwrap())
        );
        let key_hash = format!("{:x}", Sha256::digest(key.as_bytes()));
        // This is the committed operation() acceptance boundary, before operation_effect links a delivery.
        db(&store).execute("INSERT INTO operations(id,actor_id,request_key_hash,request_digest,operation_kind,target_id,state,created_at_us,updated_at_us) VALUES(?1,'local-owner',?2,?3,'alert_retry',?4,'ACCEPTED',?5,?5)", params![operation_id,key_hash,digest,id,at+4]).unwrap();
        drop(store);
        let mut store =
            Store::open_with_alerts(&dir.path().join("state"), Some(&config())).unwrap();
        let operation = application::execute(
            &mut store,
            AppCommand::Operation {
                id: operation_id.clone(),
            },
            None,
        )
        .unwrap();
        assert_eq!(operation["state"], "FAILED");
        assert_eq!(operation["error_code"], "START_INTERRUPTED");
        let replay = application::execute(&mut store, command, None).unwrap();
        assert_eq!(replay["id"], operation_id);
        assert_eq!(replay["state"], "FAILED");
        assert_eq!(items(&store)[0].attempt_count, 1);
        assert_eq!(items(&store)[0].state, AlertDeliveryState::Failed);
        assert!(store.claim_alert(cronharbor::now_us()).unwrap().is_none());
    }
    #[test]
    fn pending_manual_intent_survives_restart_without_consuming_its_one_send() {
        let (dir, mut store, _, at) = setup("OneShotJob");
        let launched = launch(&mut store, at);
        result(&mut store, &launched, EndState::Failed, at + 1);
        let id = items(&store)[0].alert_id.clone();
        db(&store).execute("UPDATE alert_deliveries SET state='EXPIRED',attempt_count=12,finished_at_us=?1,next_attempt_at_us=NULL,expires_at_us=?1 WHERE id=?2",params![at,id]).unwrap();
        let accepted = application::execute(
            &mut store,
            AppCommand::RetryAlert {
                id,
                idempotency_key: "pending-restart".into(),
            },
            None,
        )
        .unwrap();
        drop(store);
        let mut store =
            Store::open_with_alerts(&dir.path().join("state"), Some(&config())).unwrap();
        let operation = application::execute(
            &mut store,
            AppCommand::Operation {
                id: accepted["id"].as_str().unwrap().into(),
            },
            None,
        )
        .unwrap();
        assert_eq!(operation["state"], "ACCEPTED");
        assert_eq!(items(&store)[0].attempt_count, 12);
        assert_eq!(items(&store)[0].state, AlertDeliveryState::Pending);
        let job = store.claim_alert(cronharbor::now_us()).unwrap().unwrap();
        store
            .complete_alert(
                receipt(job, AlertReceipt::Accepted { http_status: 204 }),
                cronharbor::now_us(),
            )
            .unwrap();
        assert_eq!(items(&store)[0].attempt_count, 13);
    }
    #[test]
    fn interrupted_manual_retry_fails_its_operation_and_never_rearms_automatic_delivery() {
        let (dir, mut store, _, at) = setup("OneShotJob");
        let launched = launch(&mut store, at);
        result(&mut store, &launched, EndState::Failed, at + 1);
        let job = store.claim_alert(at + 2).unwrap().unwrap();
        store
            .complete_alert(
                receipt(
                    job,
                    AlertReceipt::Failed {
                        code: AlertErrorCode::HttpRejected,
                        http_status: Some(400),
                    },
                ),
                at + 3,
            )
            .unwrap();
        let id = items(&store)[0].alert_id.clone();
        let operation = application::execute(
            &mut store,
            AppCommand::RetryAlert {
                id,
                idempotency_key: "manual-lost".into(),
            },
            None,
        )
        .unwrap();
        store
            .claim_alert(cronharbor::now_us() + 1)
            .unwrap()
            .unwrap();
        drop(store);
        let mut store =
            Store::open_with_alerts(&dir.path().join("state"), Some(&config())).unwrap();
        let operation = application::execute(
            &mut store,
            AppCommand::Operation {
                id: operation["id"].as_str().unwrap().into(),
            },
            None,
        )
        .unwrap();
        assert_eq!(operation["state"], "FAILED");
        assert_eq!(operation["error_code"], "MANUAL_RECEIPT_UNKNOWN");
        assert_eq!(
            items(&store)[0].last_error_code,
            Some(AlertErrorCode::ManualReceiptUnknown)
        );
        assert!(store.claim_alert(cronharbor::now_us()).unwrap().is_none());
    }
    #[test]
    fn tampered_persisted_envelope_is_blocked_before_network_handoff() {
        for (field, value) in [
            ("schema_version", json!(2)),
            ("alert_id", json!("another-alert")),
            ("namespace", json!("other")),
            ("summary", json!("untrusted raw internal error")),
            ("incident_version", json!(2)),
            ("severity", json!("info")),
            ("occurred_at_us", json!(9007199254740992_i64)),
            ("unexpected", json!("extra")),
        ] {
            let (_dir, mut store, _, at) = setup("OneShotJob");
            let launched = launch(&mut store, at);
            result(&mut store, &launched, EndState::Failed, at + 1);
            let payload: String = db(&store)
                .query_row("SELECT payload_json FROM alert_deliveries", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let mut payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
            payload[field] = value;
            db(&store)
                .execute(
                    "UPDATE alert_deliveries SET payload_json=?1",
                    [payload.to_string()],
                )
                .unwrap();
            assert!(
                store.claim_alert(at + 2).unwrap().is_none(),
                "{field} must never reach the worker"
            );
            let rows = items(&store);
            assert_eq!(rows[0].state, AlertDeliveryState::Blocked);
            assert_eq!(
                rows[0].last_error_code,
                Some(AlertErrorCode::InvalidPayload)
            );
            assert_eq!(rows[0].attempt_count, 0);
        }
    }
    #[test]
    fn destination_change_never_redirects_or_rearms_retired_deliveries() {
        let (dir, mut store, _, at) = setup("OneShotJob");
        let launched = launch(&mut store, at);
        result(&mut store, &launched, EndState::Failed, at + 1);
        drop(store);
        let mut changed = config();
        changed.endpoint = "https://other.example.com/alerts".into();
        let mut store = Store::open_with_alerts(&dir.path().join("state"), Some(&changed)).unwrap();
        assert_eq!(
            items(&store)[0].state,
            AlertDeliveryState::DestinationChanged
        );
        assert!(store.claim_alert(at + 2).unwrap().is_none());
        drop(store);
        let mut store =
            Store::open_with_alerts(&dir.path().join("state"), Some(&config())).unwrap();
        assert!(store.claim_alert(cronharbor::now_us()).unwrap().is_none());
        assert_eq!(
            items(&store)[0].state,
            AlertDeliveryState::DestinationChanged
        );
    }
    #[test]
    fn successful_new_delivery_clears_current_credential_error_while_old_blocked_row_remains() {
        let (_dir, mut store, _, at) = setup("OneShotJob");
        let first = launch(&mut store, at);
        result(&mut store, &first, EndState::Failed, at + 1);
        let job = store.claim_alert(at + 2).unwrap().unwrap();
        store
            .complete_alert(
                receipt(
                    job,
                    AlertReceipt::Blocked {
                        code: AlertErrorCode::CredentialUnavailable,
                    },
                ),
                at + 3,
            )
            .unwrap();
        assert_eq!(
            store.alert_status().unwrap().configuration_state,
            cronharbor::contracts::AlertConfigurationState::CredentialUnavailable
        );
        let second = launch(&mut store, at + 4);
        result(&mut store, &second, EndState::Failed, at + 5);
        let job = store.claim_alert(at + 6).unwrap().unwrap();
        store
            .complete_alert(
                receipt(job, AlertReceipt::Accepted { http_status: 204 }),
                at + 7,
            )
            .unwrap();
        let status = store.alert_status().unwrap();
        assert_eq!(
            status.configuration_state,
            cronharbor::contracts::AlertConfigurationState::Ready
        );
        assert_eq!(status.blocked, 1);
        assert_eq!(status.last_error_code, None);
    }
    #[test]
    fn disabling_preserves_eligible_pending_delivery_and_same_destination_reenable_resumes() {
        let (dir, mut store, _, at) = setup("OneShotJob");
        let launched = launch(&mut store, at);
        result(&mut store, &launched, EndState::Failed, at + 1);
        drop(store);
        let path = dir.path().join("state");
        let mut store = Store::open(&path).unwrap();
        assert!(store.claim_alert(cronharbor::now_us()).unwrap().is_none());
        assert_eq!(items(&store)[0].state, AlertDeliveryState::Pending);
        drop(store);
        let mut store = Store::open_with_alerts(&path, Some(&config())).unwrap();
        assert!(store.claim_alert(cronharbor::now_us()).unwrap().is_some());
    }
    #[test]
    fn pending_manual_operations_are_finished_before_restore_hold_or_destination_retirement() {
        for restore in [false, true] {
            let (dir, mut store, _, at) = setup("OneShotJob");
            let launched = launch(&mut store, at);
            result(&mut store, &launched, EndState::Failed, at + 1);
            let id = items(&store)[0].alert_id.clone();
            db(&store).execute("UPDATE alert_deliveries SET state='FAILED',finished_at_us=?1,next_attempt_at_us=NULL WHERE id=?2",params![at,id]).unwrap();
            let accepted = application::execute(
                &mut store,
                AppCommand::RetryAlert {
                    id,
                    idempotency_key: "retire-pending".into(),
                },
                None,
            )
            .unwrap();
            drop(store);
            let path = dir.path().join("state");
            let mut destination = config();
            if restore {
                use std::os::unix::fs::PermissionsExt;
                std::fs::write(
                    path.join("restore-required.json"),
                    json!({"backup_at_us":at,"cutoff_us":at+2,"evidence":"test"}).to_string(),
                )
                .unwrap();
                std::fs::set_permissions(
                    path.join("restore-required.json"),
                    std::fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            } else {
                destination.endpoint = "https://other.example.com/alerts".into();
            }
            let mut store = Store::open_with_alerts(&path, Some(&destination)).unwrap();
            let operation = application::execute(
                &mut store,
                AppCommand::Operation {
                    id: accepted["id"].as_str().unwrap().into(),
                },
                None,
            )
            .unwrap();
            assert_eq!(operation["state"], "FAILED");
            assert_eq!(
                operation["error_code"],
                if restore {
                    "RESTORE_REPLAY_SUPPRESSED"
                } else {
                    "DESTINATION_CHANGED"
                }
            );
            let (manual, link): (bool, Option<String>) = db(&store)
                .query_row(
                    "SELECT manual_once,manual_operation_id FROM alert_deliveries",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert!(!manual);
            assert!(link.is_none());
            assert!(store.claim_alert(cronharbor::now_us()).unwrap().is_none());
            if restore {
                store
                    .restore_resume(at + 2, "verified cutoff", "local-owner")
                    .unwrap();
                assert_eq!(items(&store)[0].state, AlertDeliveryState::Expired);
            } else {
                assert_eq!(
                    items(&store)[0].state,
                    AlertDeliveryState::DestinationChanged
                );
            }
        }
    }
    #[test]
    fn source_unknown_marker_survives_outbox_history_eviction() {
        let (_dir, mut store, _, at) = setup("OneShotJob");
        let launched = launch(&mut store, at);
        result(&mut store, &launched, EndState::Unknown, at + 1);
        let job = store.claim_alert(at + 2).unwrap().unwrap();
        store
            .complete_alert(
                receipt(job, AlertReceipt::Accepted { http_status: 204 }),
                at + 3,
            )
            .unwrap();
        db(&store)
            .execute("DELETE FROM alert_deliveries", [])
            .unwrap();
        store.scan_current_alerts(at + 4).unwrap();
        assert!(items(&store).is_empty());
        let marker: String = db(&store)
            .query_row(
                "SELECT alert_unknown_outcome FROM attempts WHERE id=?1",
                [launched.attempt_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(marker, "QUEUED");
    }
    #[test]
    fn current_service_scan_is_bounded_and_continues_with_stable_source_markers() {
        let (_dir, mut store, mut manifest, at) = setup("Service");
        for index in 1..=100 {
            manifest.metadata.name = format!("service-{index}");
            store.apply(&manifest, at).unwrap();
        }
        db(&store)
            .execute("UPDATE service_instances SET observed_state='FAILED'", [])
            .unwrap();
        store.scan_current_alerts(at + 1).unwrap();
        assert_eq!(store.alert_status().unwrap().outstanding, 100);
        store.scan_current_alerts(at + 2).unwrap();
        assert_eq!(store.alert_status().unwrap().outstanding, 101);
        store.scan_current_alerts(at + 3).unwrap();
        assert_eq!(store.alert_status().unwrap().outstanding, 101);
        let first = store.alert_deliveries(None, 37).unwrap();
        assert_eq!(first.items.len(), 37);
        let second = store.alert_deliveries(first.next_cursor, 37).unwrap();
        assert_eq!(second.items.len(), 37);
        assert!(
            !second
                .items
                .iter()
                .any(|row| first.items.iter().any(|old| old.alert_id == row.alert_id))
        );
        for cursor in ["bad", "1:bad", "-x:00000000-0000-0000-0000-000000000000"] {
            assert_eq!(
                store
                    .alert_deliveries(Some(cursor.into()), 100)
                    .unwrap_err()
                    .code,
                "INPUT_INVALID"
            );
        }
    }
    #[test]
    fn disabled_delivery_still_expires_outstanding_rows_and_age_cleanup_is_bounded() {
        let (dir, store, _, at) = setup("OneShotJob");
        seed(&store, 150, false, at);
        db(&store).execute("UPDATE alert_deliveries SET state=CASE CAST(substr(source_key,9) AS INTEGER)%3 WHEN 0 THEN 'PENDING' WHEN 1 THEN 'BLOCKED' ELSE 'DESTINATION_CHANGED' END", []).unwrap();
        assert_eq!(store.alert_status().unwrap().blocked, 50);
        drop(store);
        let mut store = Store::open(&dir.path().join("state")).unwrap();
        store.maintain_alerts(at + DELIVERY_LIFETIME_US).unwrap();
        assert_eq!(store.alert_status().unwrap().outstanding, 0);
        store
            .maintain_alerts(at + DELIVERY_LIFETIME_US + 604_800_000_001)
            .unwrap();
        let count: u32 = db(&store)
            .query_row("SELECT count(*) FROM alert_deliveries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 50);
        assert_eq!(store.alert_status().unwrap().history_eviction_count, 100);
    }
    #[test]
    fn aged_restored_outbox_remains_held_until_explicit_resume_records_suppression() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, mut store, _, at) = setup("OneShotJob");
        let old = at - 10 * DELIVERY_LIFETIME_US;
        let first = launch(&mut store, old);
        result(&mut store, &first, EndState::Failed, old + 1);
        let second = launch(&mut store, old + 2);
        drop(store);
        let path = dir.path().join("state");
        std::fs::write(
            path.join("restore-required.json"),
            json!({"backup_at_us":old,"cutoff_us":old+3,"evidence":"aged restore drill"})
                .to_string(),
        )
        .unwrap();
        std::fs::set_permissions(
            path.join("restore-required.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let mut store = Store::open_with_alerts(&path, Some(&config())).unwrap();
        assert_eq!(items(&store)[0].state, AlertDeliveryState::RestoreHeld);
        store
            .maintain_alerts(at + 8 * DELIVERY_LIFETIME_US)
            .unwrap();
        assert_eq!(items(&store).len(), 1);
        assert_eq!(items(&store)[0].state, AlertDeliveryState::RestoreHeld);
        assert_eq!(store.alert_status().unwrap().outstanding, 1);
        assert_eq!(store.alert_status().unwrap().restore_suppressed_count, 0);
        assert!(
            store
                .claim_alert(at + 8 * DELIVERY_LIFETIME_US)
                .unwrap()
                .is_none()
        );
        store.scan_current_alerts(at).unwrap();
        let marker: String = db(&store)
            .query_row(
                "SELECT alert_unknown_outcome FROM attempts WHERE id=?1",
                [&second.attempt_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker, "NONE");
        store
            .restore_resume(old + 3, "verified exclusive cutoff", "local-owner")
            .unwrap();
        let rows = items(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, AlertDeliveryState::Expired);
        assert_eq!(
            rows[0].last_error_code,
            Some(AlertErrorCode::RestoreReplaySuppressed)
        );
        assert_eq!(store.alert_status().unwrap().restore_suppressed_count, 1);
        assert_eq!(store.alert_status().unwrap().outstanding, 0);
        store.scan_current_alerts(at).unwrap();
        assert_eq!(items(&store).len(), 2);
        assert_eq!(store.alert_status().unwrap().outstanding, 1);
    }
    #[test]
    fn restore_holds_old_outbox_and_does_not_consume_new_current_markers_before_resume() {
        let (dir, mut store, _, at) = setup("OneShotJob");
        let first = launch(&mut store, at);
        result(&mut store, &first, EndState::Failed, at + 1);
        let second = launch(&mut store, at + 2);
        drop(store);
        let path = dir.path().join("state");
        let marker = json!({"backup_at_us":at,"cutoff_us":at+3,"evidence":"test"});
        std::fs::write(path.join("restore-required.json"), marker.to_string()).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path.join("restore-required.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let mut store = Store::open_with_alerts(&path, Some(&config())).unwrap();
        assert_eq!(items(&store)[0].state, AlertDeliveryState::RestoreHeld);
        assert!(store.claim_alert(at + 4).unwrap().is_none());
        store.scan_current_alerts(at + 4).unwrap();
        let source_marker: String = db(&store)
            .query_row(
                "SELECT alert_unknown_outcome FROM attempts WHERE id=?1",
                [second.attempt_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source_marker, "NONE");
        store
            .restore_resume(at + 3, "verified exclusive cutoff", "local-owner")
            .unwrap();
        store.scan_current_alerts(at + 5).unwrap();
        let rows = items(&store);
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .any(|r| r.last_error_code == Some(AlertErrorCode::RestoreReplaySuppressed))
        );
        assert_eq!(store.alert_status().unwrap().restore_suppressed_count, 1);
        assert_eq!(store.alert_status().unwrap().outstanding, 1);
    }
}

#[test]
fn retry_delay_is_deterministic_bounded_and_honors_only_a_capped_retry_after() {
    for attempt in 1..=12 {
        let delay = cronharbor::alerts::retry_delay_seconds("alert", attempt, None);
        let base = 30_u32
            .saturating_mul(1_u32 << attempt.saturating_sub(1))
            .min(3600);
        assert!((base..=base + base / 5).contains(&delay));
        assert_eq!(
            delay,
            cronharbor::alerts::retry_delay_seconds("alert", attempt, None)
        );
    }
    assert_eq!(
        cronharbor::alerts::retry_delay_seconds("alert", 1, Some(u32::MAX)),
        3600
    );
}

#[test]
fn alert_administration_requires_global_admin_and_unscoped_operations_authority() {
    use cronharbor::contracts::Role;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("state")).unwrap();
    for (role, namespaces, capabilities, allowed) in [
        (Role::Admin, vec!["*"], vec!["operations.manage"], true),
        (Role::Admin, vec!["local"], vec!["operations.manage"], false),
        (
            Role::Admin,
            vec!["*"],
            vec!["operations.manage:target"],
            false,
        ),
        (Role::Editor, vec!["*"], vec!["operations.manage"], false),
        (Role::Observe, vec!["*"], vec!["operations.manage"], false),
    ] {
        let token = application::execute(
            &mut store,
            AppCommand::CreateToken {
                role,
                namespaces: namespaces.into_iter().map(String::from).collect(),
                capabilities: capabilities.into_iter().map(String::from).collect(),
                ttl_seconds: 60,
            },
            None,
        )
        .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned();
        for command in [
            AppCommand::AlertStatus,
            AppCommand::AlertDeliveries {
                cursor: None,
                limit: 100,
            },
        ] {
            let result = application::execute(&mut store, command, Some(&token));
            if allowed {
                assert!(result.is_ok())
            } else {
                assert_eq!(result.unwrap_err().code, "NOT_AUTHORIZED");
            }
        }
        let retry = application::execute(
            &mut store,
            AppCommand::RetryAlert {
                id: "missing".into(),
                idempotency_key: "admin-scope".into(),
            },
            Some(&token),
        );
        assert_eq!(
            retry.unwrap_err().code,
            if allowed {
                "ALERTS_DISABLED"
            } else {
                "NOT_AUTHORIZED"
            }
        );
    }
}
#[test]
fn genuine_schema_four_upgrade_adds_empty_alert_state_without_rewriting_prior_migrations() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let db = rusqlite::Connection::open(path.join("state.db")).unwrap();
    for (number, sql) in [
        (1, include_str!("../migrations/0001_initial.sql")),
        (2, include_str!("../migrations/0002_runtime.sql")),
        (3, include_str!("../migrations/0003_auth_rejections.sql")),
        (4, include_str!("../migrations/0004_log_streams.sql")),
    ] {
        db.execute_batch(sql).unwrap();
        db.execute(
            "INSERT INTO schema_migrations(version,checksum,applied_at_us) VALUES(?1,?2,0)",
            rusqlite::params![number, format!("{:x}", Sha256::digest(sql))],
        )
        .unwrap();
    }
    db.execute_batch("PRAGMA user_version=4").unwrap();
    drop(db);
    std::fs::set_permissions(
        path.join("state.db"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let store = Store::open(&path).unwrap();
    assert_eq!(store.alert_status().unwrap().outstanding, 0);
    let db = rusqlite::Connection::open(path.join("state.db")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        5
    );
    assert!(std::fs::read_dir(path).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("pre-migration-5-")
    }));
}
