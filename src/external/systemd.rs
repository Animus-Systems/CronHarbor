use super::{ExecutorPolicy, ExternalHandle, policy_error};
use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SystemdScope {
    User,
    System,
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScopedSystemdUnit {
    pub unit: String,
    pub scope: SystemdScope,
}
fn unit_name(unit: &str) -> bool {
    unit.len() <= 255
        && unit.ends_with(".service")
        && unit
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && unit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.@:-".contains(&b))
}
pub(super) fn validate(policy: &ExecutorPolicy) -> Result<()> {
    if policy.units.len() + policy.scoped_units.len() > 256 {
        return Err(policy_error());
    }
    let mut names = BTreeSet::new();
    for unit in policy
        .units
        .iter()
        .map(String::as_str)
        .chain(policy.scoped_units.iter().map(|entry| entry.unit.as_str()))
    {
        if !unit_name(unit) || !names.insert(unit) {
            return Err(policy_error());
        }
    }
    Ok(())
}
pub(super) fn resolve(policy: &ExecutorPolicy, unit: &str) -> Result<SystemdScope> {
    validate(policy)?;
    if !policy.systemd_enabled || !unit_name(unit) {
        return Err(policy_error());
    }
    if let Some(entry) = policy.scoped_units.iter().find(|entry| entry.unit == unit) {
        return Ok(entry.scope);
    }
    if policy.units.iter().any(|entry| entry == unit) {
        return Ok(if policy.systemd_user {
            SystemdScope::User
        } else {
            SystemdScope::System
        });
    }
    Err(policy_error())
}
pub(super) fn authorize(policy: &ExecutorPolicy, handle: &ExternalHandle) -> Result<SystemdScope> {
    match handle {
        ExternalHandle::Systemd { unit, .. } => {
            if !policy.units.contains(unit) {
                return Err(policy_error());
            }
            let scope = resolve(policy, unit)?;
            if policy.legacy_systemd_scope != Some(scope) {
                return Err(policy_error());
            }
            Ok(scope)
        }
        ExternalHandle::SystemdV2 { unit, scope, .. } => {
            if resolve(policy, unit)? != *scope {
                return Err(policy_error());
            }
            Ok(*scope)
        }
        ExternalHandle::Docker { .. } => Err(policy_error()),
    }
}

pub(super) fn intent(
    policy: &ExecutorPolicy,
    spec: &super::SystemdSpec,
    service: bool,
) -> Result<ExternalHandle> {
    Ok(ExternalHandle::SystemdV2 {
        unit: spec.unit.clone(),
        scope: resolve(policy, &spec.unit)?,
        since_us: Some(crate::now_us()),
        service,
        invocation_id: None,
    })
}
pub(super) fn properties(
    policy: &ExecutorPolicy,
    handle: &ExternalHandle,
) -> Result<std::collections::BTreeMap<String, String>> {
    let ExternalHandle::SystemdV2 { unit, .. } = handle else {
        return Err(policy_error());
    };
    let result=super::systemctl_handle(policy,handle,&["show".into(),"--no-pager".into(),"--property=LoadState,ActiveState,SubState,Result,ExecMainCode,ExecMainStatus,InvocationID,Job".into(),unit.clone()],std::time::Duration::from_secs(2))?;
    if !result.success || result.truncated {
        return Err(super::unavailable());
    }
    let mut values = std::collections::BTreeMap::new();
    for line in std::str::from_utf8(&result.stdout)
        .map_err(|_| super::unavailable())?
        .lines()
    {
        let (key, value) = line.split_once('=').ok_or_else(super::unavailable)?;
        if values.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(super::unavailable());
        }
    }
    if values.get("LoadState").map(String::as_str) != Some("loaded") {
        return Err(super::unavailable());
    }
    Ok(values)
}
fn valid_invocation(value: &str) -> bool {
    value.len() == 32
        && value.bytes().all(|b| b.is_ascii_hexdigit())
        && value.bytes().any(|b| b != b'0')
}
fn invocation(value: Option<&String>) -> Result<Option<String>> {
    match value {
        Some(value) if value.is_empty() => Ok(None),
        Some(value) if valid_invocation(value) => Ok(Some(value.clone())),
        _ => Err(super::unavailable()),
    }
}
pub(super) fn start(policy: &ExecutorPolicy, handle: &ExternalHandle) -> Result<ExternalHandle> {
    let ExternalHandle::SystemdV2 {
        unit,
        scope,
        since_us,
        service,
        invocation_id,
    } = handle
    else {
        return Err(policy_error());
    };
    authorize(policy, handle)?;
    if invocation_id.is_some() {
        return Err(crate::Error::new(
            "START_REJECTED",
            "A launch receipt cannot request another start",
        ));
    }
    let before = properties(policy, handle).map_err(|_| {
        crate::Error::new(
            "START_REJECTED",
            "Unit start prerequisites could not be verified",
        )
    })?;
    let active = before
        .get("ActiveState")
        .map(String::as_str)
        .ok_or_else(|| {
            crate::Error::new(
                "START_REJECTED",
                "Unit state could not be verified before start",
            )
        })?;
    let previous = invocation(before.get("InvocationID")).map_err(|_| {
        crate::Error::new(
            "START_REJECTED",
            "Unit invocation could not be verified before start",
        )
    })?;
    if matches!(active, "active" | "activating" | "reloading") {
        if !service || active != "active" {
            return Err(crate::Error::new(
                "START_REJECTED",
                "The unit already has running work",
            ));
        }
        let id = previous.ok_or_else(|| {
            crate::Error::new(
                "START_REJECTED",
                "Cannot adopt an unverified service invocation",
            )
        })?;
        return Ok(ExternalHandle::SystemdV2 {
            unit: unit.clone(),
            scope: *scope,
            since_us: *since_us,
            service: true,
            invocation_id: Some(id),
        });
    }
    if !matches!(active, "inactive" | "failed")
        || before.get("Job").is_none_or(|value| !value.is_empty())
    {
        return Err(crate::Error::new(
            "START_REJECTED",
            "The unit has an unresolved transition",
        ));
    }
    let result = super::systemctl_handle(
        policy,
        handle,
        &[
            "--no-block".into(),
            "start".into(),
            "--job-mode=fail".into(),
            unit.clone(),
        ],
        std::time::Duration::from_secs(5),
    )?;
    if !result.success || result.truncated {
        return Err(super::unavailable());
    }
    let began = std::time::Instant::now();
    loop {
        let observed = properties(policy, handle)?;
        if let Some(id) = invocation(observed.get("InvocationID"))?
            && Some(&id) != previous.as_ref()
        {
            return Ok(ExternalHandle::SystemdV2 {
                unit: unit.clone(),
                scope: *scope,
                since_us: *since_us,
                service: *service,
                invocation_id: Some(id),
            });
        }
        if began.elapsed() >= std::time::Duration::from_secs(3) {
            return Err(super::unavailable());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
pub(super) fn verify_invocation(
    handle: &ExternalHandle,
    properties: &std::collections::BTreeMap<&str, &str>,
) -> Result<()> {
    if let ExternalHandle::SystemdV2 {
        service,
        invocation_id,
        ..
    } = handle
    {
        let expected = invocation_id
            .as_ref()
            .filter(|value| valid_invocation(value))
            .ok_or_else(super::unavailable)?;
        let actual = properties
            .get("InvocationID")
            .filter(|value| valid_invocation(value))
            .ok_or_else(super::unavailable)?;
        // A managed service owns the unit across its existing OS restart policy.
        if !service && expected != *actual {
            return Err(super::unavailable());
        }
    }
    Ok(())
}

// Keep completed oneshot metadata loaded until its durable outcome is settled.
const REFERENCE_SCRIPT: &str = r#"
import dbus, os, sys
try:
    address = "unix:path=/run/user/%d/bus" % os.geteuid() if sys.argv[1] == "user" else dbus.bus.BUS_SYSTEM
    bus = dbus.bus.BusConnection(address)
    manager = dbus.Interface(bus.get_object("org.freedesktop.systemd1", "/org/freedesktop/systemd1"), "org.freedesktop.systemd1.Manager")
    manager.LoadUnit(sys.argv[2], timeout=2)
    manager.RefUnit(sys.argv[2], timeout=2)
    print("READY", flush=True)
    sys.stdin.buffer.read(1)
    bus.close()
except Exception:
    sys.exit(1)
"#;

pub(crate) struct SystemdReference(std::process::Child);
impl Drop for SystemdReference {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        loop {
            match self.0.try_wait() {
                Ok(Some(_)) => return,
                _ if std::time::Instant::now() >= deadline => {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                    return;
                }
                _ => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
    }
}
pub(crate) fn reference(
    policy: &ExecutorPolicy,
    handle: &ExternalHandle,
) -> Result<Option<SystemdReference>> {
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::Duration,
    };
    let ExternalHandle::SystemdV2 {
        unit,
        scope,
        service: false,
        ..
    } = handle
    else {
        return Ok(None);
    };
    authorize(policy, handle)?;
    let rejected = || {
        crate::Error::new(
            "START_REJECTED",
            "Scheduled systemd metadata reference is unavailable",
        )
    };
    let child = Command::new(&policy.systemd_reference_python)
        .args([
            "-I",
            "-u",
            "-c",
            REFERENCE_SCRIPT,
            if *scope == SystemdScope::User {
                "user"
            } else {
                "system"
            },
            unit,
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| rejected())?;
    let mut reference = SystemdReference(child);
    let mut output = reference.0.stdout.take().ok_or_else(rejected)?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut ready = [0; 6];
        let result = output.read_exact(&mut ready).map(|()| ready);
        let _ = tx.send(result);
    });
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(ready))
            if &ready == b"READY\n"
                && reference.0.try_wait().is_ok_and(|value| value.is_none()) =>
        {
            Ok(Some(reference))
        }
        _ => Err(rejected()),
    }
}

#[cfg(test)]
mod reference_tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};
    #[test]
    fn scheduled_metadata_reference_lives_until_its_owner_drops_it() {
        let root = tempfile::tempdir().unwrap();
        let python = root.path().join("python");
        fs::write(
            &python,
            "#!/bin/sh\nprintf 'READY\\n'\ncat >/dev/null\nprintf closed > \"$0.closed\"\n",
        )
        .unwrap();
        fs::set_permissions(&python, fs::Permissions::from_mode(0o700)).unwrap();
        let policy: ExecutorPolicy = serde_json::from_value(serde_json::json!({
            "systemdEnabled": true, "systemdReferencePython": python,
            "scopedUnits": [{"unit":"proof.service","scope":"user"}]
        }))
        .unwrap();
        let handle = intent(
            &policy,
            &super::super::SystemdSpec {
                unit: "proof.service".into(),
            },
            false,
        )
        .unwrap();
        let reference = reference(&policy, &handle).unwrap().unwrap();
        assert!(!python.with_extension("closed").exists());
        drop(reference);
        assert_eq!(
            fs::read_to_string(python.with_extension("closed")).unwrap(),
            "closed"
        );
    }
    #[test]
    fn stuck_reference_cleanup_leaves_room_for_a_900_second_service_drain() {
        let root = tempfile::tempdir().unwrap();
        let python = root.path().join("python");
        fs::write(
            &python,
            "#!/bin/sh\nprintf 'READY\\n'\nexec /bin/sleep 10\n",
        )
        .unwrap();
        fs::set_permissions(&python, fs::Permissions::from_mode(0o700)).unwrap();
        let policy:ExecutorPolicy=serde_json::from_value(serde_json::json!({"systemdEnabled":true,"systemdReferencePython":python,"scopedUnits":[{"unit":"proof.service","scope":"user"}]})).unwrap();
        let handle = intent(
            &policy,
            &super::super::SystemdSpec {
                unit: "proof.service".into(),
            },
            false,
        )
        .unwrap();
        let reference = reference(&policy, &handle).unwrap();
        let began = std::time::Instant::now();
        drop(reference);
        assert!(began.elapsed() < std::time::Duration::from_millis(500));
    }
    #[test]
    fn persistent_services_need_no_metadata_reference_or_extra_system_authority() {
        let policy: ExecutorPolicy=serde_json::from_value(serde_json::json!({"systemdEnabled":true,"systemdReferencePython":"/missing/python","scopedUnits":[{"unit":"proof.service","scope":"system"}]})).unwrap();
        let handle = intent(
            &policy,
            &super::super::SystemdSpec {
                unit: "proof.service".into(),
            },
            true,
        )
        .unwrap();
        assert!(reference(&policy, &handle).unwrap().is_none());
    }
    #[test]
    fn missing_reference_prerequisite_rejects_before_requesting_start() {
        let policy: ExecutorPolicy=serde_json::from_value(serde_json::json!({"systemdEnabled":true,"systemdReferencePython":"/missing/python","scopedUnits":[{"unit":"proof.service","scope":"user"}]})).unwrap();
        let handle = intent(
            &policy,
            &super::super::SystemdSpec {
                unit: "proof.service".into(),
            },
            false,
        )
        .unwrap();
        assert_eq!(
            reference(&policy, &handle).err().unwrap().code,
            "START_REJECTED"
        );
    }
}
