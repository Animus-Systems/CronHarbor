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
    let scoped = application::execute(
        &mut store,
        AppCommand::Executions {
            workload_id: Some("local/first".into()),
            cursor: None,
            limit: 10,
        },
        Some(credential),
    )
    .unwrap();
    assert_eq!(scoped["items"].as_array().unwrap().len(), 1);
    assert_eq!(scoped["items"][0]["name"], "first");
    assert!(
        application::execute(
            &mut store,
            AppCommand::Executions {
                workload_id: Some("unrelated/hidden".into()),
                cursor: None,
                limit: 10,
            },
            Some(credential),
        )
        .is_err()
    );
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

#[test]
fn scoped_history_paginates_ties_without_other_workloads_or_old_attempts() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let now = cronharbor::now_us();
    for (namespace, name) in [
        ("local", "target"),
        ("local", "other"),
        ("unrelated", "hidden"),
    ] {
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.kind = "OneShotJob".into();
        manifest.metadata.namespace = namespace.into();
        manifest.metadata.name = name.into();
        store.apply(&manifest, now).unwrap();
    }
    let database = rusqlite::Connection::open(directory.path().join("state.db")).unwrap();
    for (id, namespace, name) in [
        ("a-target", "local", "target"),
        ("b-other", "local", "other"),
        ("c-target", "local", "target"),
        ("d-hidden", "unrelated", "hidden"),
        ("e-target", "local", "target"),
        ("g-target", "local", "target"),
        ("h-target", "local", "target"),
    ] {
        let (workload, revision): (String, String) = database
            .query_row(
                "SELECT id,current_revision_id FROM workloads WHERE namespace=?1 AND name=?2",
                rusqlite::params![namespace, name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        database.execute(
            "INSERT INTO executions(id,workload_id,revision_id,trigger_kind,created_at_us,state) VALUES(?1,?2,?3,'INTERNAL',?4,'SUCCEEDED')",
            rusqlite::params![id,workload,revision,now],
        ).unwrap();
        database.execute(
            "INSERT INTO attempts(id,execution_id,attempt_index,launch_nonce,state,executor_kind,created_at_us) VALUES(?1,?2,1,?3,'SUCCEEDED','process',?4)",
            rusqlite::params![format!("attempt-{id}"),id,format!("nonce-{id}"),now],
        ).unwrap();
    }
    database.execute(
        "INSERT INTO attempts(id,execution_id,attempt_index,launch_nonce,state,executor_kind,created_at_us) VALUES('attempt-latest','c-target',2,'nonce-latest','FAILED','process',?1)",
        rusqlite::params![now],
    ).unwrap();
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
    let mut cursor = None;
    let mut observed = Vec::new();
    let mut pages = 0;
    loop {
        pages += 1;
        assert!(pages <= 5, "pagination did not terminate");
        let page = application::execute(
            &mut store,
            AppCommand::Executions {
                workload_id: Some("local/target".into()),
                cursor,
                limit: 2,
            },
            Some(credential),
        )
        .unwrap();
        for row in page["items"].as_array().unwrap() {
            observed.push((
                row["execution_id"].as_str().unwrap().to_owned(),
                row["attempt_id"].as_str().unwrap().to_owned(),
            ));
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
        assert!(observed.len() < 6, "pagination repeated rows");
    }
    assert_eq!(
        observed,
        vec![
            ("h-target".into(), "attempt-h-target".into()),
            ("g-target".into(), "attempt-g-target".into()),
            ("e-target".into(), "attempt-e-target".into()),
            ("c-target".into(), "attempt-latest".into()),
            ("a-target".into(), "attempt-a-target".into()),
        ]
    );
}
