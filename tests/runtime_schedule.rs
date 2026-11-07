use chrono::DateTime;
use cronharbor::{config::Manifest, store::Store};
fn at(value: &str) -> i64 {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .timestamp_micros()
}
fn job() -> Manifest {
    Manifest::parse(include_str!("../examples/hello.yaml")).unwrap()
}
#[test]
fn madrid_fold_selects_first_second_or_both() {
    let mut m = job();
    m.spec.schedule.expression = "30 2 * * *".into();
    m.spec.schedule.timezone = "Europe/Madrid".into();
    for (fold, expected) in [
        ("first", "2026-10-25T00:30:00Z"),
        ("second", "2026-10-25T01:30:00Z"),
        ("both", "2026-10-25T00:30:00Z"),
    ] {
        m.spec.schedule.fold = fold.into();
        assert!(m.validate().is_ok());
        assert_eq!(
            m.next_after(at("2026-10-24T23:00:00Z")).unwrap(),
            at(expected)
        );
    }
    m.spec.schedule.fold = "both".into();
    assert_eq!(
        m.next_after(at("2026-10-25T00:30:00Z")).unwrap(),
        at("2026-10-25T01:30:00Z")
    );
}
#[test]
fn madrid_gap_is_skipped_or_coalesced_to_first_valid_minute() {
    let mut m = job();
    m.spec.schedule.expression = "30 2 * * *".into();
    m.spec.schedule.timezone = "Europe/Madrid".into();
    assert_eq!(
        m.next_after(at("2026-03-28T23:00:00Z")).unwrap(),
        at("2026-03-30T00:30:00Z")
    );
    m.spec.schedule.gap = "next_valid".into();
    assert_eq!(
        m.next_after(at("2026-03-28T23:00:00Z")).unwrap(),
        at("2026-03-29T01:00:00Z")
    );
}
#[test]
fn additional_kinds_and_interval_schedule_are_validated() {
    let raw = include_str!("../examples/hello.yaml");
    assert!(Manifest::parse(&raw.replace("ScheduledJob", "Service")).is_ok());
    assert!(Manifest::parse(&raw.replace("ScheduledJob", "OneShotJob")).is_ok());
    let m = Manifest::parse(&raw.replace(
        "type: cron",
        "type: interval\n    intervalSeconds: 90\n    anchorAtUs: 10000000",
    ))
    .unwrap();
    assert_eq!(m.next_after(10000000).unwrap(), 100000000);
}
#[test]
fn latest_and_catchup_are_bounded() {
    use std::os::unix::fs::PermissionsExt;
    for policy in ["latest", "catch_up"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut s = Store::open(dir.path()).unwrap();
        let mut m = job();
        m.spec.schedule.misfire.policy = policy.into();
        m.spec.schedule.misfire.max_occurrences = 2;
        m.spec.schedule.misfire.max_age_seconds = 3600;
        s.apply(&m, 0).unwrap();
        let launch = s.claim_due(3_600_000_000).unwrap().unwrap();
        assert!(launch.due_at_us >= 3_480_000_000);
    }
}
#[test]
fn published_service_example_accepts_restart_and_shutdown_defaults() {
    let raw = "apiVersion: cronharbor.dev/v1\nkind: Service\nmetadata:\n  name: worker\n  namespace: local\nspec:\n  owner: file\n  enabled: true\n  execution:\n    adapter: process\n    command: /bin/sleep\n    args: ['30']\n    workingDirectory: /tmp\n    sandbox: default\n  restart:\n    policy: on-failure\n    maxAttempts: 8\n    windowSeconds: 300\n  shutdown:\n    graceSeconds: 30\n";
    let m = Manifest::parse(raw).unwrap();
    assert_eq!(m.spec.service.max_restarts, 8);
    assert_eq!(m.spec.service.stop_grace_seconds, 30);
    assert!(m.canonical().unwrap().0.contains("\"service\""));
}
#[test]
fn retries_require_explicit_safety_and_never_replay_unknown() {
    use cronharbor::process::{EndState, Outcome};
    use std::os::unix::fs::PermissionsExt;
    for (policy, error, state, expected) in [
        ("idempotent", Some("EXIT_NONZERO"), EndState::Failed, true),
        (
            "start_rejected",
            Some("EXIT_NONZERO"),
            EndState::Failed,
            false,
        ),
        (
            "start_rejected",
            Some("START_REJECTED"),
            EndState::Failed,
            true,
        ),
        (
            "idempotent",
            Some("RECONCILIATION_REQUIRED"),
            EndState::Unknown,
            false,
        ),
    ] {
        let d = tempfile::tempdir().unwrap();
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut s = Store::open(d.path()).unwrap();
        let mut m = job();
        m.spec.retry.max_attempts = 2;
        m.spec.retry.policy = policy.into();
        s.apply(&m, 0).unwrap();
        let l = s.claim_due(60_000_000).unwrap().unwrap();
        s.start_requested(&l.attempt_id, 60_000_000).unwrap();
        s.finish(
            &l,
            &Outcome {
                state,
                error_code: error,
                exit_code: None,
                log_bytes: 0,
                log_truncated: false,
            },
            61_000_000,
        )
        .unwrap();
        let retry = s.claim_due(62_000_000).unwrap();
        assert_eq!(retry.is_some(), expected, "{policy} {state:?}");
        if let Some(retry) = retry {
            assert_eq!(retry.execution_id, l.execution_id);
            assert_ne!(retry.attempt_id, l.attempt_id);
        }
    }
}
#[test]
fn systemd_rejects_native_settings_that_the_unit_bridge_cannot_apply() {
    let mut m = job();
    m.spec.execution.adapter = "systemd".into();
    m.spec.execution.systemd = Some(cronharbor::external::SystemdSpec {
        unit: "worker.service".into(),
    });
    m.spec.execution.command.clear();
    m.spec.execution.args.clear();
    assert!(m.validate().is_ok());
    m.spec.execution.env.insert("MODE".into(), "worker".into());
    assert!(m.validate().is_err());
    m.spec.execution.env.clear();
    m.spec.execution.limits.memory_bytes = Some(1048576);
    assert!(m.validate().is_err());
}
