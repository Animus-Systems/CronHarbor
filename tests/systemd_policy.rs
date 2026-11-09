use cronharbor::{
    config::Manifest,
    external::{self, ExecutorPolicy, ExternalHandle, SystemdSpec},
};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn helper(root: &Path) -> String {
    let path = root.join("systemctl");
    fs::write(&path,r#"#!/bin/sh
printf '%s\n' "$@" >> "$0.calls"
case "$1" in --user|--system) shift;; esac
case "$1" in --no-block) shift;; esac
case "$1" in
start) : > "$0.active"; : > "$0.started";;
stop) rm -f "$0.active";;
show)
 active=inactive; id=
 if [ -f "$0.active" ]; then active=active; fi
 if [ -f "$0.started" ]; then id=11111111111111111111111111111111; fi
 printf 'LoadState=loaded\nActiveState=%s\nSubState=running\nResult=success\nExecMainCode=1\nExecMainStatus=0\nInvocationID=%s\nNRestarts=0\nJob=\n' "$active" "$id";;
*) exit 1;;
esac
"#).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path.to_string_lossy().into_owned()
}
fn scoped(binary: &str, scope: &str) -> ExecutorPolicy {
    serde_json::from_value(json!({"systemdEnabled":true,"systemctlBinary":binary,
      "scopedUnits":[{"unit":"worker.service","scope":scope}]}))
    .unwrap()
}
#[test]
fn exact_scoped_user_unit_records_manager_and_cannot_be_redirected_by_policy_rotation() {
    let root = tempfile::tempdir().unwrap();
    let binary = helper(root.path());
    let policy = scoped(&binary, "user");
    let handle = external::start_systemd(
        &policy,
        &SystemdSpec {
            unit: "worker.service".into(),
        },
    )
    .unwrap();
    assert_eq!(serde_json::to_value(&handle).unwrap()["scope"], "user");
    let before = fs::read_to_string(format!("{binary}.calls")).unwrap();
    assert!(before.contains("--user\n"));
    assert!(external::stop(&scoped(&binary, "system"), &handle, 1).is_err());
    assert_eq!(
        before,
        fs::read_to_string(format!("{binary}.calls")).unwrap()
    );
}
#[test]
fn exact_scoped_system_unit_uses_system_manager() {
    let root = tempfile::tempdir().unwrap();
    let binary = helper(root.path());
    let handle = external::start_systemd(
        &scoped(&binary, "system"),
        &SystemdSpec {
            unit: "worker.service".into(),
        },
    )
    .unwrap();
    assert_eq!(serde_json::to_value(handle).unwrap()["scope"], "system");
    let calls = fs::read_to_string(format!("{binary}.calls")).unwrap();
    assert!(calls.contains("--system\n"));
    assert!(!calls.contains("--user\n"));
}
#[test]
fn ambiguous_scope_or_legacy_collision_is_refused_before_any_effect() {
    let root = tempfile::tempdir().unwrap();
    let binary = helper(root.path());
    for entries in [
        json!([{"unit":"worker.service","scope":"user"},{"unit":"worker.service","scope":"system"}]),
        json!([{"unit":"worker.service","scope":"user"},{"unit":"worker.service","scope":"user"}]),
    ] {
        let policy: ExecutorPolicy = serde_json::from_value(
            json!({"systemdEnabled":true,"systemctlBinary":binary,"scopedUnits":entries}),
        )
        .unwrap();
        assert!(
            external::start_systemd(
                &policy,
                &SystemdSpec {
                    unit: "worker.service".into()
                }
            )
            .is_err()
        );
    }
    let policy:ExecutorPolicy=serde_json::from_value(json!({"systemdEnabled":true,"systemctlBinary":binary,"units":["worker.service"],"scopedUnits":[{"unit":"worker.service","scope":"system"}]})).unwrap();
    assert!(
        external::start_systemd(
            &policy,
            &SystemdSpec {
                unit: "worker.service".into()
            }
        )
        .is_err()
    );
    assert!(!root.path().join("systemctl.calls").exists());
}
#[test]
fn legacy_handle_cannot_silently_adopt_a_new_scoped_mapping() {
    let root = tempfile::tempdir().unwrap();
    let binary = helper(root.path());
    let legacy = ExternalHandle::Systemd {
        unit: "worker.service".into(),
        since_us: Some(cronharbor::now_us()),
    };
    assert!(external::inspect(&scoped(&binary, "system"), &legacy).is_err());
    assert!(!root.path().join("systemctl.calls").exists());
}
#[test]
fn evaluation_shutdown_grace_accepts_nine_hundred_and_rejects_nine_hundred_one() {
    let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
    manifest.kind = "Service".into();
    manifest.spec.service.stop_grace_seconds = 900;
    manifest.validate().unwrap();
    manifest.spec.service.stop_grace_seconds = 901;
    assert!(manifest.validate().is_err());
}

#[test]
fn legacy_handle_requires_an_explicit_manager_baseline_before_observation() {
    let root = tempfile::tempdir().unwrap();
    let binary = helper(root.path());
    let policy:ExecutorPolicy=serde_json::from_value(json!({"systemdEnabled":true,"systemctlBinary":binary,"systemdUser":true,"units":["worker.service"]})).unwrap();
    let handle = ExternalHandle::Systemd {
        unit: "worker.service".into(),
        since_us: Some(cronharbor::now_us()),
    };
    assert!(external::inspect(&policy, &handle).is_err());
    assert!(!root.path().join("systemctl.calls").exists());
}
#[test]
fn legacy_manager_baseline_cannot_follow_a_changed_global_manager() {
    let root = tempfile::tempdir().unwrap();
    let binary = helper(root.path());
    let mut policy:ExecutorPolicy=serde_json::from_value(json!({"systemdEnabled":true,"systemctlBinary":binary,"systemdUser":true,"legacySystemdScope":"user","units":["worker.service"]})).unwrap();
    let handle = ExternalHandle::Systemd {
        unit: "worker.service".into(),
        since_us: Some(cronharbor::now_us()),
    };
    assert!(external::inspect(&policy, &handle).is_ok());
    let before = fs::read_to_string(format!("{binary}.calls")).unwrap();
    policy.systemd_user = false;
    assert!(external::inspect(&policy, &handle).is_err());
    assert_eq!(
        before,
        fs::read_to_string(format!("{binary}.calls")).unwrap()
    );
}

#[test]
fn user_manager_commands_receive_only_the_canonical_uid_bus_context() {
    let root = tempfile::tempdir().unwrap();
    let binary = helper(root.path());
    let before = fs::read_to_string(&binary).unwrap();
    let guarded=before.replacen("#!/bin/sh\n", "#!/bin/sh\nif [ \"$1\" = --user ]; then\n test \"$XDG_RUNTIME_DIR\" = \"/run/user/$(id -u)\" || exit 92\n test \"$DBUS_SESSION_BUS_ADDRESS\" = \"unix:path=/run/user/$(id -u)/bus\" || exit 93\nfi\n",1);
    fs::write(&binary, guarded).unwrap();
    external::start_systemd(
        &scoped(&binary, "user"),
        &SystemdSpec {
            unit: "worker.service".into(),
        },
    )
    .unwrap();
}
