use cronharbor::{
    application,
    config::Manifest,
    contracts::AppCommand,
    external::{self, ExecutorPolicy, SystemdSpec},
    store::Store,
};
use serde_json::json;
fn launches(store: &mut Store) -> Vec<cronharbor::store::Launch> {
    let mut launches = Vec::new();
    for name in ["one", "two"] {
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.kind = "OneShotJob".into();
        manifest.metadata.name = name.into();
        manifest.spec.execution.adapter = "systemd".into();
        manifest.spec.execution.command.clear();
        manifest.spec.execution.args.clear();
        manifest.spec.execution.systemd = Some(SystemdSpec {
            unit: "worker.service".into(),
        });
        store.apply(&manifest, cronharbor::now_us()).unwrap();
        application::execute(
            store,
            AppCommand::Run {
                id: format!("local/{name}"),
                idempotency_key: name.into(),
            },
            None,
        )
        .unwrap();
        launches.push(store.claim_due(cronharbor::now_us()).unwrap().unwrap());
    }
    launches
}
#[test]
fn one_unit_cannot_be_reserved_by_two_different_workloads_even_when_the_first_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("state")).unwrap();
    let jobs = launches(&mut store);
    let policy: ExecutorPolicy = serde_json::from_value(
        json!({"systemdEnabled":true,"scopedUnits":[{"unit":"worker.service","scope":"user"}]}),
    )
    .unwrap();
    let handle = external::systemd_intent(
        &policy,
        &SystemdSpec {
            unit: "worker.service".into(),
        },
        false,
    )
    .unwrap();
    store
        .persist_external_intent(&jobs[0].attempt_id, &handle)
        .unwrap();
    store
        .start_requested(&jobs[0].attempt_id, cronharbor::now_us())
        .unwrap();
    store
        .finish(
            &jobs[0],
            &cronharbor::process::Outcome {
                state: cronharbor::process::EndState::Unknown,
                error_code: Some("EXECUTOR_UNAVAILABLE"),
                exit_code: None,
                log_bytes: 0,
                log_truncated: true,
            },
            cronharbor::now_us(),
        )
        .unwrap();
    let result = store.persist_external_intent(&jobs[1].attempt_id, &handle);
    assert!(
        result.is_err(),
        "a second workload must not coalesce onto an already-owned unit"
    );
    assert!(store.attempt_is_unknown(&jobs[0].attempt_id).unwrap());
}
