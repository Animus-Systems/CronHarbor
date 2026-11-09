use cronharbor::external::{self, ExecutorPolicy, ExternalHandle, ExternalState, SystemdSpec};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    time::{Duration, Instant},
};
fn fixture(root: &Path) -> (ExecutorPolicy, String) {
    let binary = root.join("systemctl");
    fs::write(&binary,r#"#!/bin/sh
printf '%s\n' "$@" >> "$0.calls"
case "$1" in --user|--system) shift;; esac
nonblock=false
if [ "$1" = --no-block ]; then nonblock=true; shift; fi
case "$1" in
start)
 if [ "$nonblock" = false ]; then sleep 1; fi
 printf '11111111111111111111111111111111' > "$0.id"
 printf 'activating' > "$0.state"
 if [ -f "$0.lost" ]; then exit 1; fi;;
show)
 if [ -f "$0.missing" ]; then printf 'LoadState=not-found\n'; exit 0; fi
 state=inactive; id=
 if [ -f "$0.id" ]; then id=$(cat "$0.id"); fi
 if [ -f "$0.state" ]; then state=$(cat "$0.state"); fi
 printf 'LoadState=loaded\nActiveState=%s\nSubState=start\nResult=success\nExecMainCode=1\nExecMainStatus=0\nInvocationID=%s\nNRestarts=0\nJob=\n' "$state" "$id";;
stop) printf inactive > "$0.state";;
*) exit 1;;
esac
"#).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let name = binary.to_string_lossy().into_owned();
    let policy=serde_json::from_value(json!({"systemdEnabled":true,"systemctlBinary":name,"scopedUnits":[{"unit":"worker.service","scope":"user"}]})).unwrap();
    (policy, name)
}
#[test]
fn oneshot_start_returns_before_completion_and_pins_the_actual_invocation() {
    let dir = tempfile::tempdir().unwrap();
    let (policy, _) = fixture(dir.path());
    let began = Instant::now();
    let handle = external::start_systemd(
        &policy,
        &SystemdSpec {
            unit: "worker.service".into(),
        },
    )
    .unwrap();
    assert!(
        began.elapsed() < Duration::from_millis(700),
        "start must not wait for oneshot completion"
    );
    assert_eq!(
        serde_json::to_value(&handle).unwrap()["invocation_id"],
        "11111111111111111111111111111111"
    );
    assert_eq!(
        external::inspect(&policy, &handle).unwrap().state,
        ExternalState::Running
    );
}
#[test]
fn scheduled_start_does_not_coalesce_with_an_already_running_unit() {
    let dir = tempfile::tempdir().unwrap();
    let (policy, binary) = fixture(dir.path());
    fs::write(format!("{binary}.state"), "active").unwrap();
    assert!(
        external::start_systemd(
            &policy,
            &SystemdSpec {
                unit: "worker.service".into()
            }
        )
        .is_err()
    );
    assert!(
        !fs::read_to_string(format!("{binary}.calls"))
            .unwrap()
            .lines()
            .any(|line| line == "start")
    );
}
#[test]
fn a_prior_success_or_foreign_invocation_cannot_finish_or_stop_this_run() {
    let dir = tempfile::tempdir().unwrap();
    let (policy, binary) = fixture(dir.path());
    let handle = external::start_systemd(
        &policy,
        &SystemdSpec {
            unit: "worker.service".into(),
        },
    )
    .unwrap();
    fs::write(format!("{binary}.state"), "inactive").unwrap();
    fs::write(format!("{binary}.id"), "22222222222222222222222222222222").unwrap();
    assert!(external::inspect(&policy, &handle).is_err());
    assert!(external::stop(&policy, &handle, 1).is_err());
    assert!(
        !fs::read_to_string(format!("{binary}.calls"))
            .unwrap()
            .lines()
            .any(|line| line == "stop")
    );
}
#[test]
fn an_interrupted_intent_with_no_invocation_receipt_is_not_reattached_to_a_later_unit() {
    let dir = tempfile::tempdir().unwrap();
    let (policy, binary) = fixture(dir.path());
    let handle:ExternalHandle=serde_json::from_value(json!({"kind":"systemd_v2","unit":"worker.service","scope":"user","since_us":1,"service":false,"invocation_id":null})).unwrap();
    fs::write(format!("{binary}.state"), "inactive").unwrap();
    fs::write(format!("{binary}.id"), "22222222222222222222222222222222").unwrap();
    assert!(external::inspect(&policy, &handle).is_err());
}
#[test]
fn healthy_service_adoption_keeps_the_invocation_without_issuing_start() {
    let dir = tempfile::tempdir().unwrap();
    let (policy, binary) = fixture(dir.path());
    fs::write(format!("{binary}.state"), "active").unwrap();
    fs::write(format!("{binary}.id"), "33333333333333333333333333333333").unwrap();
    let intent = external::systemd_intent(
        &policy,
        &SystemdSpec {
            unit: "worker.service".into(),
        },
        true,
    )
    .unwrap();
    let receipt = external::start_systemd_intent(&policy, &intent).unwrap();
    assert_eq!(
        serde_json::to_value(&receipt).unwrap()["invocation_id"],
        "33333333333333333333333333333333"
    );
    assert!(
        !fs::read_to_string(format!("{binary}.calls"))
            .unwrap()
            .lines()
            .any(|line| line == "start")
    );
}
#[test]
fn active_service_with_a_nil_invocation_cannot_be_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let (policy, binary) = fixture(dir.path());
    fs::write(format!("{binary}.state"), "active").unwrap();
    fs::write(format!("{binary}.id"), "00000000000000000000000000000000").unwrap();
    let intent = external::systemd_intent(
        &policy,
        &SystemdSpec {
            unit: "worker.service".into(),
        },
        true,
    )
    .unwrap();
    assert!(external::start_systemd_intent(&policy, &intent).is_err());
    fs::write(format!("{binary}.id"), "").unwrap();
    assert_eq!(
        external::start_systemd_intent(&policy, &intent)
            .unwrap_err()
            .code,
        "START_REJECTED"
    );
}

#[test]
fn missing_unit_is_rejected_before_start_while_a_lost_enqueue_ack_stays_uncertain() {
    let dir = tempfile::tempdir().unwrap();
    let (policy, binary) = fixture(dir.path());
    fs::write(format!("{binary}.missing"), "").unwrap();
    let error = external::start_systemd(
        &policy,
        &SystemdSpec {
            unit: "worker.service".into(),
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "START_REJECTED");
    assert!(
        !fs::read_to_string(format!("{binary}.calls"))
            .unwrap()
            .lines()
            .any(|line| line == "start")
    );
    fs::remove_file(format!("{binary}.missing")).unwrap();
    fs::write(format!("{binary}.lost"), "").unwrap();
    let error = external::start_systemd(
        &policy,
        &SystemdSpec {
            unit: "worker.service".into(),
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "EXECUTOR_UNAVAILABLE");
    assert!(
        fs::read_to_string(format!("{binary}.calls"))
            .unwrap()
            .lines()
            .any(|line| line == "start")
    );
}
