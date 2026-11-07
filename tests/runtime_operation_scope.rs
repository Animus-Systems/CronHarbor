use cronharbor::{
    application,
    contracts::{AppCommand, Role},
    store::Store,
};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;

fn call(store: &mut Store, command: AppCommand, token: Option<&str>) -> Value {
    application::execute(store, command, token).unwrap()
}

fn token(store: &mut Store, namespace: &str) -> String {
    call(
        store,
        AppCommand::CreateToken {
            role: Role::Admin,
            namespaces: vec![namespace.into()],
            capabilities: vec![
                "operations.manage".into(),
                "workload.run_preapproved".into(),
            ],
            ttl_seconds: 3600,
        },
        None,
    )["token"]
        .as_str()
        .unwrap()
        .into()
}

fn inspect(store: &mut Store, operation: &Value, token: &str, allowed: bool) {
    let result = application::execute(
        store,
        AppCommand::Operation {
            id: operation["id"].as_str().unwrap().into(),
        },
        Some(token),
    );
    if allowed {
        assert_eq!(result.unwrap()["id"], operation["id"]);
    } else {
        assert_eq!(result.unwrap_err().code, "NOT_AUTHORIZED");
    }
}

#[test]
fn operation_lookup_enforces_target_namespace_for_other_actors() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let local = token(&mut store, "local");
    let unrelated = token(&mut store, "unrelated");
    let wildcard = token(&mut store, "*");
    let definition = include_str!("../examples/hello.yaml")
        .replace("owner: file", "owner: api")
        .replace("ScheduledJob", "OneShotJob");
    let applied = call(
        &mut store,
        AppCommand::Apply {
            definition,
            expected_revision: None,
            source_kind: "API".into(),
            idempotency_key: "apply".into(),
        },
        None,
    );
    let run = call(
        &mut store,
        AppCommand::Run {
            id: "local/hello".into(),
            idempotency_key: "run".into(),
        },
        None,
    );
    let launch = store.claim_due(cronharbor::now_us()).unwrap().unwrap();
    let cancelled = call(
        &mut store,
        AppCommand::Cancel {
            execution_id: launch.execution_id,
            idempotency_key: "cancel".into(),
        },
        None,
    );
    let paused = call(
        &mut store,
        AppCommand::Pause {
            id: "local/hello".into(),
            paused: true,
            idempotency_key: "pause".into(),
        },
        None,
    );
    for operation in [&applied, &run, &cancelled, &paused] {
        inspect(&mut store, operation, &local, true);
        inspect(&mut store, operation, &wildcard, true);
        inspect(&mut store, operation, &unrelated, false);
    }
    let own = call(
        &mut store,
        AppCommand::Pause {
            id: "local/hello".into(),
            paused: false,
            idempotency_key: "own-pause".into(),
        },
        Some(&local),
    );
    inspect(&mut store, &own, &local, true);
}

#[test]
fn batch_operations_require_instance_scope_except_for_their_actor() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let local = token(&mut store, "local");
    let wildcard = token(&mut store, "*");
    let batch = call(
        &mut store,
        AppCommand::ApplyBatch {
            definitions: vec![
                include_str!("../examples/hello.yaml").replace("owner: file", "owner: api"),
            ],
            expected_revisions: vec![None],
            source_kind: "API".into(),
            idempotency_key: "batch".into(),
        },
        None,
    );
    inspect(&mut store, &batch, &wildcard, true);
    inspect(&mut store, &batch, &local, false);
    assert!(
        application::execute(
            &mut store,
            AppCommand::Operation {
                id: batch["id"].as_str().unwrap().into()
            },
            None,
        )
        .is_ok()
    );
}
