use cronharbor::{config::Manifest, contracts::AppCommand, server, store::Store};
use serde_json::Value;
use std::{
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct CliFixture {
    daemon: Daemon,
    directory: tempfile::TempDir,
}
impl CliFixture {
    fn new() -> Self {
        Self::prepared(|_| {})
    }
    fn prepared(prepare: impl FnOnce(&mut Store)) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut store = Store::open(directory.path()).unwrap();
        prepare(&mut store);
        drop(store);
        let mut fixture = Self {
            daemon: Daemon(
                Command::new(env!("CARGO_BIN_EXE_cronharbor"))
                    .arg("--state-dir")
                    .arg(directory.path())
                    .args(["server", "--paused"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            ),
            directory,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.directory.path().join("control.sock").exists() {
            assert!(
                fixture.daemon.0.try_wait().unwrap().is_none(),
                "daemon exited"
            );
            assert!(Instant::now() < deadline, "daemon did not become ready");
            thread::sleep(Duration::from_millis(20));
        }
        fixture
    }
    fn output(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_cronharbor"))
            .arg("--state-dir")
            .arg(self.directory.path())
            .args(args)
            .output()
            .unwrap()
    }
    fn json(&self, args: &[&str]) -> Value {
        let output = self.output(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn fails(&self, args: &[&str], code: &str) {
        let output = self.output(args);
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn write(&self, name: &str, manifest: &Manifest) -> std::path::PathBuf {
        let path = self.directory.path().join(name);
        std::fs::write(&path, serde_json::to_string(manifest).unwrap()).unwrap();
        path
    }
    fn apply(&self, path: &Path, key: &str) -> Value {
        self.json(&[
            "config",
            "apply",
            "-f",
            path.to_str().unwrap(),
            "--idempotency-key",
            key,
        ])
    }
    fn command(&self, command: AppCommand) -> cronharbor::Result<Value> {
        server::client(
            self.directory.path(),
            server::Request::Execute {
                command: Box::new(command),
                token: None,
            },
        )
    }
}
fn manifest(name: &str) -> Manifest {
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.kind = "OneShotJob".into();
    manifest.metadata.name = name.into();
    manifest
}

#[test]
fn cli_file_apply_replays_create_update_and_original_intent_after_a_new_revision() {
    let fixture = CliFixture::new();
    let mut definition = manifest("replay");
    let path = fixture.write("single.json", &definition);
    let created = fixture.apply(&path, "create");
    assert_eq!(fixture.apply(&path, "create"), created);
    let first = fixture.json(&["jobs", "list"])["items"][0]["revision_id"]
        .as_str()
        .unwrap()
        .to_owned();
    definition.spec.timeout_seconds += 1;
    fixture.write("single.json", &definition);
    let updated = fixture.apply(&path, "update");
    assert_eq!(fixture.apply(&path, "update"), updated);
    let second = fixture.json(&["jobs", "list"])["items"][0]["revision_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(first, second);
    definition.spec.timeout_seconds += 1;
    fixture.write("single.json", &definition);
    fixture.apply(&path, "newer");
    let newest = fixture.json(&["jobs", "list"])["items"][0]["revision_id"].clone();
    definition.spec.timeout_seconds -= 1;
    fixture.write("single.json", &definition);
    assert_eq!(fixture.apply(&path, "update"), updated);
    assert_eq!(
        fixture.json(&["jobs", "list"])["items"][0]["revision_id"],
        newest
    );
    fixture.fails(
        &[
            "config",
            "apply",
            "-f",
            path.to_str().unwrap(),
            "--idempotency-key",
            "update",
            "--expected-revision",
            &first,
        ],
        "IDEMPOTENCY_CONFLICT",
    );
    fixture.fails(
        &[
            "config",
            "apply",
            "-f",
            path.to_str().unwrap(),
            "--idempotency-key",
            "stale-explicit",
            "--expected-revision",
            &first,
        ],
        "REVISION_CONFLICT",
    );
    definition.spec.timeout_seconds += 2;
    fixture.write("single.json", &definition);
    fixture.fails(
        &[
            "config",
            "apply",
            "-f",
            path.to_str().unwrap(),
            "--idempotency-key",
            "update",
        ],
        "IDEMPOTENCY_CONFLICT",
    );
    fixture.fails(
        &["jobs", "run", "local/replay", "--idempotency-key", "update"],
        "IDEMPOTENCY_CONFLICT",
    );
    let audit = fixture.json(&["audit"]);
    assert_eq!(
        audit["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["action"] == "config.apply")
            .count(),
        3
    );
}

#[test]
fn cli_explicit_file_revision_replays_the_original_command_after_later_updates() {
    let fixture = CliFixture::new();
    let mut definition = manifest("explicit");
    let path = fixture.write("explicit.json", &definition);
    fixture.apply(&path, "initial");
    let first = fixture.json(&["jobs", "list"])["items"][0]["revision_id"]
        .as_str()
        .unwrap()
        .to_owned();
    definition.spec.timeout_seconds += 1;
    fixture.write("explicit.json", &definition);
    let args = [
        "config",
        "apply",
        "-f",
        path.to_str().unwrap(),
        "--idempotency-key",
        "explicit-update",
        "--expected-revision",
        &first,
    ];
    let accepted = fixture.json(&args);
    definition.spec.timeout_seconds += 1;
    fixture.write("explicit.json", &definition);
    fixture.apply(&path, "later");
    let latest = fixture.json(&["jobs", "list"])["items"][0]["revision_id"].clone();
    definition.spec.timeout_seconds -= 1;
    fixture.write("explicit.json", &definition);
    assert_eq!(fixture.json(&args), accepted);
    assert_eq!(
        fixture.json(&["jobs", "list"])["items"][0]["revision_id"],
        latest
    );
}

#[test]
fn cli_mixed_file_batch_replays_once_and_rolls_back_on_source_conflict() {
    let fixture = CliFixture::new();
    let mut original = manifest("existing");
    let path = fixture.write("original.json", &original);
    fixture.apply(&path, "initial");
    original.spec.timeout_seconds += 1;
    let batch = fixture.directory.path().join("batch");
    std::fs::create_dir(&batch).unwrap();
    std::fs::write(
        batch.join("01-update.json"),
        serde_json::to_string(&original).unwrap(),
    )
    .unwrap();
    std::fs::write(
        batch.join("02-create.json"),
        serde_json::to_string(&manifest("created")).unwrap(),
    )
    .unwrap();
    let applied = fixture.apply(&batch, "mixed");
    assert_eq!(fixture.apply(&batch, "mixed"), applied);
    let inventory = fixture.json(&["jobs", "list"]);
    assert_eq!(inventory["items"].as_array().unwrap().len(), 2);
    let audit = fixture.json(&["audit"]);
    assert_eq!(
        audit["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["action"] == "config.apply")
            .count(),
        3
    );
    original.spec.timeout_seconds += 1;
    std::fs::write(
        batch.join("01-update.json"),
        serde_json::to_string(&original).unwrap(),
    )
    .unwrap();
    fixture.fails(
        &[
            "config",
            "apply",
            "-f",
            batch.to_str().unwrap(),
            "--idempotency-key",
            "mixed",
        ],
        "IDEMPOTENCY_CONFLICT",
    );
    fixture.write("original.json", &original);
    fixture.apply(&path, "newer-single");
    let newer = fixture.json(&["jobs", "list"]);
    original.spec.timeout_seconds -= 1;
    std::fs::write(
        batch.join("01-update.json"),
        serde_json::to_string(&original).unwrap(),
    )
    .unwrap();
    assert_eq!(fixture.apply(&batch, "mixed"), applied);
    assert_eq!(fixture.json(&["jobs", "list"]), newer);
    original.spec.timeout_seconds += 2;
    std::fs::write(
        batch.join("01-update.json"),
        serde_json::to_string(&original).unwrap(),
    )
    .unwrap();
    let mut api_owned = manifest("api-owned");
    api_owned.spec.owner = "api".into();
    fixture
        .command(AppCommand::Apply {
            definition: serde_json::to_string(&api_owned).unwrap(),
            expected_revision: None,
            source_kind: "API".into(),
            idempotency_key: "api-create".into(),
        })
        .unwrap();
    std::fs::write(
        batch.join("02-create.json"),
        serde_json::to_string(&manifest("must-rollback")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        batch.join("03-api-owned.json"),
        serde_json::to_string(&api_owned).unwrap(),
    )
    .unwrap();
    fixture.fails(
        &[
            "config",
            "apply",
            "-f",
            batch.to_str().unwrap(),
            "--idempotency-key",
            "atomic-failure",
        ],
        "SOURCE_CONFLICT",
    );
    let after = fixture.json(&["jobs", "list"]);
    assert_eq!(after["items"].as_array().unwrap().len(), 3);
    assert!(
        !after["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "must-rollback")
    );
    let before_row = newer["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "existing")
        .unwrap();
    let after_row = after["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "existing")
        .unwrap();
    assert_eq!(after_row["revision_id"], before_row["revision_id"]);
    let audit = fixture.json(&["audit"]);
    assert_eq!(
        audit["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["action"] == "config.apply")
            .count(),
        5
    );
}

#[test]
fn cli_inventory_lists_follow_cursors_and_reject_unbounded_queries() {
    let fixture = CliFixture::prepared(|store| {
        for name in ["first", "second", "third"] {
            store.apply(&manifest(name), 0).unwrap();
        }
    });
    for _ in 0..3 {
        fixture.json(&["tokens", "create", "--namespace", "local"]);
    }
    for prefix in [["jobs", "list"], ["tokens", "list"]] {
        let default = fixture.json(&prefix);
        assert_eq!(default["items"].as_array().unwrap().len(), 3);
        let mut ids = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut args = vec![prefix[0], prefix[1], "--limit", "1"];
            if let Some(cursor) = &cursor {
                args.extend(["--cursor", cursor]);
            }
            let page = fixture.json(&args);
            assert_eq!(page["items"].as_array().unwrap().len(), 1);
            ids.push(page["items"][0]["id"].clone());
            cursor = page["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
            assert!(ids.len() < 4, "cursor failed to advance");
        }
        assert_eq!(
            ids,
            default["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["id"].clone())
                .collect::<Vec<_>>()
        );
        for limit in ["0", "101"] {
            fixture.fails(&[prefix[0], prefix[1], "--limit", limit], "INPUT_INVALID");
        }
        fixture.fails(
            &[prefix[0], prefix[1], "--cursor", &"x".repeat(129)],
            "INPUT_INVALID",
        );
    }
}

#[test]
fn cli_service_pages_filter_kind_before_the_global_limit() {
    let fixture = CliFixture::prepared(|store| {
        for (id, name, service) in [
            ("a-job", "first", false),
            ("b-service", "second", true),
            ("c-service", "third", true),
        ] {
            let mut definition = if service {
                Manifest::parse(include_str!("../examples/service.yaml")).unwrap()
            } else {
                manifest(name)
            };
            definition.metadata.name = name.into();
            store.apply(&definition, 0).unwrap();
            let old = store.manifest_for(&format!("local/{name}")).unwrap().0;
            // Fixture IDs deliberately place the job before both services, independent of UUID order.
            let mut database =
                rusqlite::Connection::open(store.directory.join("state.db")).unwrap();
            database.pragma_update(None, "foreign_keys", true).unwrap();
            let tx = database.transaction().unwrap();
            tx.pragma_update(None, "defer_foreign_keys", true).unwrap();
            tx.execute(
                "UPDATE workloads SET id=?1 WHERE id=?2",
                rusqlite::params![id, old],
            )
            .unwrap();
            for table in ["workload_revisions", "service_instances", "events"] {
                tx.execute(
                    &format!("UPDATE {table} SET workload_id=?1 WHERE workload_id=?2"),
                    rusqlite::params![id, old],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
    });
    let all = fixture.json(&["jobs", "list"]);
    let rows = all["items"].as_array().unwrap();
    let first = fixture.json(&["services", "list", "--limit", "1"]);
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert_eq!(first["items"][0]["id"], rows[1]["id"]);
    assert_eq!(first["items"][0]["kind"], "managed_service");
    let second = fixture.json(&[
        "services",
        "list",
        "--limit",
        "1",
        "--cursor",
        first["next_cursor"].as_str().unwrap(),
    ]);
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_eq!(second["items"][0]["id"], rows[2]["id"]);
    assert!(second["next_cursor"].is_null());
    for limit in ["0", "101"] {
        fixture.fails(&["services", "list", "--limit", limit], "INPUT_INVALID");
    }
    fixture.fails(
        &["services", "list", "--cursor", &"x".repeat(129)],
        "INPUT_INVALID",
    );
}

#[test]
fn cli_execution_list_pages_filter_workload_and_preserve_the_bare_array() {
    let fixture = CliFixture::prepared(|store| {
        use cronharbor::{
            application,
            process::{EndState, Outcome},
        };
        for (index, name) in ["selected", "other", "selected"].into_iter().enumerate() {
            store.apply(&manifest(name), 0).unwrap();
            application::execute(
                store,
                AppCommand::Run {
                    id: format!("local/{name}"),
                    idempotency_key: format!("run-{index}"),
                },
                None,
            )
            .unwrap();
            let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
            store
                .start_requested(&launch.attempt_id, cronharbor::now_us())
                .unwrap();
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
        }
    });
    let bare = fixture.json(&["executions"]);
    assert_eq!(bare.as_array().unwrap().len(), 3);
    let all = fixture.json(&["executions", "list"]);
    assert_eq!(all["items"], bare);
    let first = fixture.json(&[
        "executions",
        "list",
        "--limit",
        "1",
        "--workload-id",
        "local/selected",
    ]);
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert_eq!(first["items"][0]["name"], "selected");
    let second = fixture.json(&[
        "executions",
        "list",
        "--limit",
        "1",
        "--workload-id",
        "local/selected",
        "--cursor",
        first["next_cursor"].as_str().unwrap(),
    ]);
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_eq!(second["items"][0]["name"], "selected");
    assert_ne!(
        first["items"][0]["execution_id"],
        second["items"][0]["execution_id"]
    );
    assert!(second["next_cursor"].is_null());
    for limit in ["0", "101"] {
        fixture.fails(&["executions", "list", "--limit", limit], "INPUT_INVALID");
    }
    fixture.fails(
        &["executions", "list", "--cursor", "invalid"],
        "INPUT_INVALID",
    );
}

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn local_cli_validates_applies_inspects_and_never_starts_a_second_owner() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let binary = env!("CARGO_BIN_EXE_cronharbor");
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/hello.yaml");
    assert!(
        Command::new(binary)
            .args(["config", "validate", "-f", fixture])
            .status()
            .unwrap()
            .success()
    );
    let mut daemon = Daemon(
        Command::new(binary)
            .arg("--state-dir")
            .arg(dir.path())
            .args(["server", "--paused"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dir.path().join("control.sock").exists() {
        assert!(daemon.0.try_wait().unwrap().is_none(), "daemon exited");
        assert!(Instant::now() < deadline, "daemon did not become ready");
        thread::sleep(Duration::from_millis(20));
    }
    let second = Command::new(binary)
        .arg("--state-dir")
        .arg(dir.path())
        .arg("server")
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("OWNER_UNAVAILABLE"));
    for args in [
        vec!["config", "apply", "-f", fixture],
        vec!["status"],
        vec!["executions"],
        vec!["log-storage", "status"],
        vec!["alerts", "status"],
        vec!["alerts", "list", "--limit", "10"],
    ] {
        let output = Command::new(binary)
            .arg("--state-dir")
            .arg(dir.path())
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    }
    let output = Command::new(binary)
        .arg("--state-dir")
        .arg(dir.path())
        .arg("status")
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["paused"], true);
    assert_eq!(status["workloads"], 1);

    let updated = Command::new(binary)
        .arg("--state-dir")
        .arg(dir.path())
        .args([
            "log-storage",
            "set",
            "--max-disk-bytes",
            "536870912",
            "--max-segments",
            "2048",
            "--retention-seconds",
            "1209600",
            "--expected-revision",
            "1",
            "--idempotency-key",
            "cli-log-policy-1",
        ])
        .output()
        .unwrap();
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    let output = Command::new(binary)
        .arg("--state-dir")
        .arg(dir.path())
        .args(["log-storage", "status"])
        .output()
        .unwrap();
    let policy: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(policy["policy"]["revision"], 2);
    assert_eq!(policy["policy"]["max_segments"], 2048);
    assert_eq!(policy["policy"]["retention_seconds"], 1209600);
}
