use cronharbor::{
    application,
    config::Manifest,
    contracts::{AppCommand, Role},
    process::{EndState, Outcome},
    store::Store,
};
use std::os::unix::fs::PermissionsExt;

#[test]
fn recent_failures_use_completion_time_and_preserve_scoped_unresolved_unknowns() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let now = cronharbor::now_us();
    for (name, namespace, started_ago, ended_ago, state) in [
        ("first", "local", 7_200_000_000, 1_000_000, EndState::Failed),
        ("second", "local", 60_000_000, 2_000_000, EndState::TimedOut),
        (
            "old",
            "local",
            10_800_000_000,
            7_200_000_000,
            EndState::Failed,
        ),
        ("unknown", "local", 14_400_000_000, 0, EndState::Unknown),
        ("hidden", "unrelated", 100_000_000, 0, EndState::Unknown),
    ] {
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.kind = "OneShotJob".into();
        manifest.metadata.name = name.into();
        manifest.metadata.namespace = namespace.into();
        store.apply(&manifest, now - started_ago).unwrap();
        application::execute(
            &mut store,
            AppCommand::Run {
                id: format!("{namespace}/{name}"),
                idempotency_key: name.into(),
            },
            None,
        )
        .unwrap();
        let launch = store.claim_due(now).unwrap().unwrap();
        store
            .start_requested(&launch.attempt_id, now - started_ago)
            .unwrap();
        store
            .finish(
                &launch,
                &Outcome {
                    state,
                    error_code: Some("FIXTURE_OUTCOME"),
                    exit_code: Some(1),
                    log_bytes: 0,
                    log_truncated: false,
                },
                now - ended_ago,
            )
            .unwrap();
        rusqlite::Connection::open(directory.path().join("state.db"))
            .unwrap()
            .execute(
                "UPDATE executions SET created_at_us=?1 WHERE id=?2",
                rusqlite::params![now - started_ago, launch.execution_id],
            )
            .unwrap();
        rusqlite::Connection::open(directory.path().join("state.db"))
            .unwrap()
            .execute(
                "UPDATE attempts SET created_at_us=?1,started_at_us=?1 WHERE execution_id=?2",
                rusqlite::params![now - started_ago, launch.execution_id],
            )
            .unwrap();
    }
    let token = application::execute(
        &mut store,
        AppCommand::CreateToken {
            role: Role::Viewer,
            namespaces: vec!["local".into()],
            capabilities: vec![],
            ttl_seconds: 3600,
        },
        None,
    )
    .unwrap();
    let credential = token["token"].as_str().unwrap();
    let recent = application::execute(
        &mut store,
        AppCommand::RecentFailures {
            window_seconds: 3600,
            limit: 10,
        },
        Some(credential),
    )
    .unwrap();
    let names: Vec<_> = recent["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["first", "second", "unknown"]);
    let history = application::execute(
        &mut store,
        AppCommand::Executions {
            workload_id: None,
            cursor: None,
            limit: 1,
        },
        Some(credential),
    )
    .unwrap();
    assert_eq!(history["items"][0]["name"], "second");
    let next = application::execute(
        &mut store,
        AppCommand::Executions {
            workload_id: None,
            cursor: Some(history["next_cursor"].as_str().unwrap().into()),
            limit: 1,
        },
        Some(credential),
    )
    .unwrap();
    assert_eq!(next["items"][0]["name"], "first");
}
