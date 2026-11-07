//! Explicitly enabled, allowlisted external lifecycle adapters.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DockerSpec {
    pub image: String,
    pub user: String,
    pub network: String,
    pub memory_bytes: u64,
    pub pids_limit: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SystemdSpec {
    pub unit: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutorPolicy {
    pub docker_enabled: bool,
    pub docker_binary: String,
    pub docker_host: String,
    pub images: Vec<String>,
    pub networks: Vec<String>,
    pub max_memory_bytes: u64,
    pub max_pids: u32,
    pub systemd_enabled: bool,
    pub systemctl_binary: String,
    pub journalctl_binary: String,
    pub systemd_user: bool,
    pub units: Vec<String>,
}
impl Default for ExecutorPolicy {
    fn default() -> Self {
        Self {
            docker_enabled: false,
            docker_binary: "/usr/bin/docker".into(),
            docker_host: "unix:///var/run/docker.sock".into(),
            images: Vec::new(),
            networks: vec!["none".into()],
            max_memory_bytes: 4 * 1024 * 1024 * 1024,
            max_pids: 256,
            systemd_enabled: false,
            systemctl_binary: "/usr/bin/systemctl".into(),
            journalctl_binary: "/usr/bin/journalctl".into(),
            systemd_user: false,
            units: Vec::new(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Identity {
    pub instance_id: String,
    pub workload_id: String,
    pub revision_id: String,
    pub execution_id: String,
    pub launch_nonce: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExternalHandle {
    Docker {
        id: String,
        labels: BTreeMap<String, String>,
    },
    Systemd {
        unit: String,
        #[serde(default)]
        since_us: Option<i64>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalState {
    Running,
    Exited,
    Absent,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalOutcome {
    Succeeded,
    Failed,
}
#[derive(Clone, Debug)]
pub struct Observation {
    pub state: ExternalState,
    pub exit_code: Option<i32>,
    pub outcome: Option<TerminalOutcome>,
    pub healthy: Option<bool>,
}

fn unavailable() -> Error {
    Error::new(
        "EXECUTOR_UNAVAILABLE",
        "External executor is unavailable or did not provide a bounded response",
    )
}
fn policy_error() -> Error {
    Error::new(
        "NOT_AUTHORIZED",
        "Executor policy does not permit this workload",
    )
}

pub fn load_policy(directory: &Path) -> Result<ExecutorPolicy> {
    let path = directory.join("executors.json");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ExecutorPolicy::default());
        }
        Err(_) => return Err(policy_error()),
        Ok(meta)
            if !meta.is_file()
                || meta.uid() != nix::unistd::geteuid().as_raw()
                || meta.mode() & 0o077 != 0
                || meta.nlink() != 1
                || meta.len() > 65_536 =>
        {
            return Err(policy_error());
        }
        Ok(_) => {}
    }
    let bytes = fs::read(path).map_err(|_| policy_error())?;
    let policy: ExecutorPolicy = serde_json::from_slice(&bytes)
        .map_err(|_| Error::new("CONFIG_INVALID", "Invalid executor policy"))?;
    if policy.images.len() > 256
        || policy.units.len() > 256
        || policy.networks.len() > 32
        || !policy.docker_host.starts_with("unix:///")
        || policy.docker_host.contains('\0')
    {
        return Err(policy_error());
    }
    for binary in [
        &policy.docker_binary,
        &policy.systemctl_binary,
        &policy.journalctl_binary,
    ] {
        if !Path::new(binary).is_absolute() || binary.contains('\0') {
            return Err(policy_error());
        }
    }
    Ok(policy)
}

pub fn validate_docker(policy: &ExecutorPolicy, spec: &DockerSpec) -> Result<()> {
    let digest = spec.image.rsplit_once("@sha256:").map(|(_, value)| value);
    let numeric_user = spec.user.split(':').all(|part| {
        !part.is_empty()
            && part.bytes().all(|c| c.is_ascii_digit())
            && part.parse::<u32>().is_ok_and(|id| id > 0)
    });
    if !policy.docker_enabled
        || !policy.images.contains(&spec.image)
        || !digest.is_some_and(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
        || !numeric_user
        || spec.user.split(':').count() > 2
        || !policy.networks.contains(&spec.network)
        || matches!(spec.network.as_str(), "host" | "container")
        || spec.network.starts_with("container:")
        || spec.memory_bytes < 6 * 1024 * 1024
        || spec.memory_bytes > policy.max_memory_bytes
        || spec.pids_limit == 0
        || spec.pids_limit > policy.max_pids
    {
        return Err(policy_error());
    }
    Ok(())
}

pub fn docker_handle(identity: &Identity) -> ExternalHandle {
    let labels = [
        ("instance", &identity.instance_id),
        ("workload", &identity.workload_id),
        ("revision", &identity.revision_id),
        ("execution", &identity.execution_id),
        ("launch", &identity.launch_nonce),
    ]
    .into_iter()
    .map(|(key, value)| (format!("dev.cronharbor.{key}"), value.clone()))
    .collect();
    ExternalHandle::Docker {
        id: format!("cronharbor-{}", identity.launch_nonce),
        labels,
    }
}

pub struct DockerContext<'a> {
    pub command: &'a str,
    pub args: &'a [String],
    pub env: &'a BTreeMap<String, String>,
    pub working_directory: &'a str,
}

pub fn start_docker(
    policy: &ExecutorPolicy,
    spec: &DockerSpec,
    identity: &Identity,
    context: DockerContext<'_>,
    state_dir: &Path,
) -> Result<ExternalHandle> {
    let DockerContext {
        command,
        args,
        env,
        working_directory,
    } = context;
    validate_docker(policy, spec)?;
    if !Path::new(working_directory).is_absolute()
        || working_directory.len() > 4096
        || working_directory.contains('\0')
    {
        return Err(Error::new(
            "CONFIG_INVALID",
            "Docker working directory must be an absolute container path",
        ));
    }
    let handle = docker_handle(identity);
    let ExternalHandle::Docker { id, labels } = &handle else {
        return Err(policy_error());
    };
    match inspect(policy, &handle)?.state {
        ExternalState::Running => return Ok(handle),
        ExternalState::Absent => {}
        _ => {
            return Err(Error::new(
                "RECONCILIATION_REQUIRED",
                "Existing external launch requires reconciliation",
            ));
        }
    }
    let mut options = vec![
        "create".to_owned(),
        "--name".into(),
        id.clone(),
        "--read-only".into(),
        "--workdir".into(),
        working_directory.to_owned(),
        "--network".into(),
        spec.network.clone(),
        "--user".into(),
        spec.user.clone(),
        "--cap-drop".into(),
        "ALL".into(),
        "--security-opt".into(),
        "no-new-privileges:true".into(),
        "--memory".into(),
        spec.memory_bytes.to_string(),
        "--memory-swap".into(),
        spec.memory_bytes.to_string(),
        "--pids-limit".into(),
        spec.pids_limit.to_string(),
        "--cpus".into(),
        "1".into(),
        "--log-driver".into(),
        "json-file".into(),
        "--log-opt".into(),
        "max-size=1m".into(),
        "--log-opt".into(),
        "max-file=2".into(),
    ];
    for (key, value) in labels {
        options.extend(["--label".into(), format!("{key}={value}")]);
    }
    let env_path = state_dir.join(format!("{}.docker-env", identity.launch_nonce));
    if !env.is_empty() {
        use std::io::Write;
        let mut file = crate::store::secure_file(&env_path)?;
        for (key, value) in env {
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                || key.as_bytes()[0].is_ascii_digit()
                || value.contains(['\n', '\r', '\0'])
            {
                let _ = fs::remove_file(&env_path);
                return Err(Error::new(
                    "CONFIG_INVALID",
                    "Docker environment must use single-line values and valid names",
                ));
            }
            writeln!(file, "{key}={value}").map_err(|_| unavailable())?;
        }
        file.sync_all().map_err(|_| unavailable())?;
        options.extend(["--env-file".into(), env_path.to_string_lossy().into_owned()]);
    }
    options.push(spec.image.clone());
    if !command.is_empty() {
        options.push(command.to_owned());
    }
    options.extend_from_slice(args);
    let created = docker(policy, &options, Duration::from_secs(15));
    let _ = fs::remove_file(&env_path);
    if !created?.success {
        return Err(unavailable());
    }
    if !docker(
        policy,
        &["start".into(), id.clone()],
        Duration::from_secs(10),
    )?
    .success
    {
        return Err(unavailable());
    }
    Ok(handle)
}

fn valid_unit(policy: &ExecutorPolicy, unit: &str) -> bool {
    policy.systemd_enabled
        && policy.units.iter().any(|value| value == unit)
        && unit.len() <= 255
        && unit.ends_with(".service")
        && unit
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && unit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.@:-".contains(&b))
}

pub fn start_systemd(policy: &ExecutorPolicy, spec: &SystemdSpec) -> Result<ExternalHandle> {
    if !valid_unit(policy, &spec.unit) {
        return Err(policy_error());
    }
    let since_us = crate::now_us();
    if !systemctl(
        policy,
        &["start".into(), spec.unit.clone()],
        Duration::from_secs(15),
    )?
    .success
    {
        return Err(unavailable());
    }
    Ok(ExternalHandle::Systemd {
        unit: spec.unit.clone(),
        since_us: Some(since_us),
    })
}

pub fn inspect(policy: &ExecutorPolicy, handle: &ExternalHandle) -> Result<Observation> {
    match handle {
        ExternalHandle::Docker { id, labels } => {
            if !policy.docker_enabled {
                return Err(policy_error());
            }
            validate_handle(id, labels)?;
            let output = docker(
                policy,
                &[
                    "inspect".into(),
                    "--type".into(),
                    "container".into(),
                    id.clone(),
                ],
                Duration::from_secs(5),
            )?;
            if output.truncated {
                return Err(unavailable());
            }
            if !output.success {
                let absent = match docker(
                    policy,
                    &[
                        "container".into(),
                        "ls".into(),
                        "--all".into(),
                        "--filter".into(),
                        format!("name={id}"),
                        "--format".into(),
                        "{{.Names}}".into(),
                    ],
                    Duration::from_secs(5),
                ) {
                    Ok(listing) if listing.success && !listing.truncated => {
                        String::from_utf8(listing.stdout).is_ok_and(|names| {
                            !names
                                .lines()
                                .flat_map(|line| line.split(','))
                                .any(|name| name == id)
                        })
                    }
                    _ => false,
                };
                return Ok(Observation {
                    state: if absent {
                        ExternalState::Absent
                    } else {
                        ExternalState::Unknown
                    },
                    exit_code: None,
                    outcome: None,
                    healthy: None,
                });
            }
            let values: Vec<serde_json::Value> =
                serde_json::from_slice(&output.stdout).map_err(|_| unavailable())?;
            let object = values
                .first()
                .filter(|_| values.len() == 1)
                .ok_or_else(unavailable)?;
            if labels.iter().any(|(key, value)| {
                object["Config"]["Labels"][key].as_str() != Some(value.as_str())
            }) {
                return Err(Error::new(
                    "OWNERSHIP_MISMATCH",
                    "Refusing to adopt an external object owned by another launch",
                ));
            }
            let state = &object["State"];
            let status = state["Status"].as_str().unwrap_or_default();
            let exit_code = state["ExitCode"]
                .as_i64()
                .and_then(|code| i32::try_from(code).ok());
            let healthy = match state["Health"]["Status"].as_str() {
                Some("healthy") => Some(true),
                Some("unhealthy" | "starting") => Some(false),
                _ => None,
            };
            Ok(Observation {
                state: if state["Running"].as_bool() == Some(true) {
                    ExternalState::Running
                } else if matches!(status, "exited" | "dead") {
                    ExternalState::Exited
                } else {
                    ExternalState::Unknown
                },
                exit_code,
                outcome: exit_code.map(|code| {
                    if code == 0 {
                        TerminalOutcome::Succeeded
                    } else {
                        TerminalOutcome::Failed
                    }
                }),
                healthy,
            })
        }
        ExternalHandle::Systemd { unit, .. } => {
            if !valid_unit(policy, unit) {
                return Err(policy_error());
            }
            let output = systemctl(
                policy,
                &[
                    "show".into(),
                    "--no-pager".into(),
                    "--property=LoadState,ActiveState,SubState,Result,ExecMainCode,ExecMainStatus"
                        .into(),
                    unit.clone(),
                ],
                Duration::from_secs(5),
            )?;
            if output.truncated {
                return Err(unavailable());
            }
            let text = String::from_utf8(output.stdout).map_err(|_| unavailable())?;
            let mut properties = BTreeMap::new();
            for line in text.lines() {
                let (key, value) = line.split_once('=').ok_or_else(unavailable)?;
                if properties.insert(key, value).is_some() {
                    return Err(unavailable());
                }
            }
            if properties.get("LoadState") == Some(&"not-found") {
                return Ok(Observation {
                    state: ExternalState::Absent,
                    exit_code: None,
                    outcome: None,
                    healthy: None,
                });
            }
            if !output.success || properties.get("LoadState") != Some(&"loaded") {
                return Ok(Observation {
                    state: ExternalState::Unknown,
                    exit_code: None,
                    outcome: None,
                    healthy: None,
                });
            }
            let active = properties.get("ActiveState").copied().unwrap_or_default();
            Ok(Observation {
                state: if matches!(active, "active" | "activating" | "reloading") {
                    ExternalState::Running
                } else if matches!(active, "inactive" | "failed") {
                    ExternalState::Exited
                } else {
                    ExternalState::Unknown
                },
                exit_code: systemd_exit(&properties)
                    .and_then(|(code, status)| (code == 1).then_some(status)),
                outcome: systemd_outcome(&properties, active),
                healthy: Some(active == "active"),
            })
        }
    }
}

fn systemd_exit(properties: &BTreeMap<&str, &str>) -> Option<(u8, i32)> {
    let code = properties.get("ExecMainCode")?.parse::<u8>().ok()?;
    let status = properties.get("ExecMainStatus")?.parse::<u8>().ok()?;
    match (code, status) {
        (0, 0) | (1, _) | (2 | 3, 1..=64) => Some((code, i32::from(status))),
        _ => None,
    }
}

fn systemd_outcome(properties: &BTreeMap<&str, &str>, active: &str) -> Option<TerminalOutcome> {
    let (code, _) = systemd_exit(properties)?;
    // Result includes unit-operation failures and configured clean exit statuses/signals.
    match (active, *properties.get("Result")?) {
        ("inactive", "success") if matches!(code, 1 | 2) => Some(TerminalOutcome::Succeeded),
        (
            "inactive" | "failed",
            "resources" | "protocol" | "timeout" | "exit-code" | "signal" | "core-dump"
            | "watchdog" | "start-limit-hit" | "oom-kill",
        ) => Some(TerminalOutcome::Failed),
        _ => None,
    }
}

pub fn stop(policy: &ExecutorPolicy, handle: &ExternalHandle, grace_seconds: u64) -> Result<bool> {
    stop_if_current(policy, handle, grace_seconds, || true)
}
pub(crate) fn stop_if_current(
    policy: &ExecutorPolicy,
    handle: &ExternalHandle,
    grace_seconds: u64,
    current: impl Fn() -> bool,
) -> Result<bool> {
    if grace_seconds > 300 {
        return Err(Error::new(
            "CONFIG_INVALID",
            "Stop grace exceeds 300 seconds",
        ));
    }
    let observation = inspect(policy, handle)?;
    if !current() {
        return Ok(false);
    }
    if matches!(
        observation.state,
        ExternalState::Absent | ExternalState::Exited
    ) {
        return Ok(true);
    }
    if observation.state == ExternalState::Unknown {
        return Ok(false);
    }
    match handle {
        ExternalHandle::Docker { id, .. } => {
            let _ = docker(
                policy,
                &[
                    "stop".into(),
                    "--time".into(),
                    grace_seconds.to_string(),
                    id.clone(),
                ],
                Duration::from_secs(grace_seconds + 5),
            )?;
        }
        ExternalHandle::Systemd { unit, .. } => {
            let _ = systemctl(
                policy,
                &["stop".into(), unit.clone()],
                Duration::from_secs(grace_seconds + 5),
            )?;
        }
    }
    Ok(matches!(
        inspect(policy, handle)?.state,
        ExternalState::Exited | ExternalState::Absent
    ))
}

pub fn logs(
    policy: &ExecutorPolicy,
    handle: &ExternalHandle,
    max_bytes: usize,
    secrets: &[Vec<u8>],
) -> Result<(Vec<u8>, bool)> {
    logs_if_current(policy, handle, max_bytes, secrets, || true)
}
pub(crate) fn logs_if_current(
    policy: &ExecutorPolicy,
    handle: &ExternalHandle,
    max_bytes: usize,
    secrets: &[Vec<u8>],
    current: impl Fn() -> bool,
) -> Result<(Vec<u8>, bool)> {
    if !matches!(
        inspect(policy, handle)?.state,
        ExternalState::Running | ExternalState::Exited
    ) {
        return Err(unavailable());
    }
    if !current() {
        return Err(unavailable());
    }
    let output = match handle {
        ExternalHandle::Docker { id, .. } => {
            docker(policy, &["logs".into(), id.clone()], Duration::from_secs(5))?
        }
        ExternalHandle::Systemd { unit, since_us } => {
            if !valid_unit(policy, unit) {
                return Err(policy_error());
            }
            let boundary = since_us
                .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_micros)
                .filter(|_| since_us.is_some_and(|value| value >= 0))
                .ok_or_else(|| {
                    Error::new("LOG_UNAVAILABLE", "Unit log start boundary is unavailable")
                })?;
            let mut args = vec![
                "--no-pager".into(),
                "--output=cat".into(),
                format!("--since={}", boundary.format("%Y-%m-%d %H:%M:%S%.6f UTC")),
                "--unit".into(),
                unit.clone(),
            ];
            if policy.systemd_user {
                args.insert(0, "--user".into());
            }
            run(&policy.journalctl_binary, &args, Duration::from_secs(5))?
        }
    };
    if !output.success {
        return Err(unavailable());
    }
    if output.truncated && !secrets.is_empty() {
        return Err(Error::new(
            "LOG_UNAVAILABLE",
            "Incomplete external output cannot be safely redacted",
        ));
    }
    let mut bytes = output.stdout;
    bytes.extend(output.stderr);
    let (bytes, truncated) =
        crate::process::redact_bounded(&bytes, secrets, max_bytes.min(1_048_576));
    Ok((bytes, output.truncated || truncated))
}

pub fn remove_finished(policy: &ExecutorPolicy, handle: &ExternalHandle) -> Result<()> {
    match handle {
        ExternalHandle::Docker { id, .. } => match inspect(policy, handle)?.state {
            ExternalState::Absent => Ok(()),
            ExternalState::Exited => {
                if docker(
                    policy,
                    &["rm".into(), "--volumes".into(), id.clone()],
                    Duration::from_secs(5),
                )?
                .success
                {
                    Ok(())
                } else {
                    Err(unavailable())
                }
            }
            _ => Err(Error::new(
                "RECONCILIATION_REQUIRED",
                "Only a verified terminal owned container may be removed",
            )),
        },
        ExternalHandle::Systemd { .. } => Ok(()),
    }
}

pub fn original_secret_values(
    policy: &ExecutorPolicy,
    handle: &ExternalHandle,
    names: &[String],
) -> Result<Vec<String>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    if names.len() > 64 || names.iter().any(|name| !crate::config::env_name(name)) {
        return Err(policy_error());
    }
    let ExternalHandle::Docker { id, labels } = handle else {
        return Err(Error::new(
            "LOG_UNAVAILABLE",
            "Original launch secret context is unavailable",
        ));
    };
    validate_handle(id, labels)?;
    let output = docker(
        policy,
        &[
            "inspect".into(),
            "--type".into(),
            "container".into(),
            id.clone(),
        ],
        Duration::from_secs(5),
    )?;
    if !output.success || output.truncated {
        return Err(unavailable());
    }
    let objects: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).map_err(|_| unavailable())?;
    let object = objects
        .first()
        .filter(|_| objects.len() == 1)
        .ok_or_else(unavailable)?;
    if labels
        .iter()
        .any(|(key, value)| object["Config"]["Labels"][key].as_str() != Some(value))
    {
        return Err(Error::new(
            "OWNERSHIP_MISMATCH",
            "Original launch secret context belongs to another object",
        ));
    }
    let environment = object["Config"]["Env"].as_array().ok_or_else(unavailable)?;
    let mut values = Vec::new();
    for name in names {
        let matches = environment
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter_map(|entry| entry.split_once('='))
            .filter(|(key, _)| *key == name)
            .map(|(_, value)| value.to_owned())
            .collect::<Vec<_>>();
        if matches.is_empty()
            || matches
                .iter()
                .any(|value| value.is_empty() || value.len() > 65_536)
        {
            return Err(Error::new(
                "LOG_UNAVAILABLE",
                "Original launch secret context could not be verified",
            ));
        }
        values.extend(matches);
    }
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    Ok(values)
}

fn validate_handle(id: &str, labels: &BTreeMap<String, String>) -> Result<()> {
    if !id.starts_with("cronharbor-")
        || id.len() > 128
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || labels.len() != 5
        || ["instance", "workload", "revision", "execution", "launch"]
            .iter()
            .any(|key| !labels.contains_key(&format!("dev.cronharbor.{key}")))
    {
        return Err(policy_error());
    }
    Ok(())
}
fn docker(policy: &ExecutorPolicy, args: &[String], timeout: Duration) -> Result<CommandOutput> {
    if !policy.docker_enabled || !policy.docker_host.starts_with("unix:///") {
        return Err(policy_error());
    }
    let mut full = vec!["--host".into(), policy.docker_host.clone()];
    full.extend_from_slice(args);
    run(&policy.docker_binary, &full, timeout)
}
fn systemctl(policy: &ExecutorPolicy, args: &[String], timeout: Duration) -> Result<CommandOutput> {
    let mut full = Vec::new();
    if policy.systemd_user {
        full.push("--user".into());
    }
    full.extend_from_slice(args);
    run(&policy.systemctl_binary, &full, timeout)
}
struct CommandOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    success: bool,
    truncated: bool,
}
fn collect(mut reader: impl Read) -> (Vec<u8>, bool) {
    let mut result = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let keep = count.min(65_536usize.saturating_sub(result.len()));
                result.extend_from_slice(&buffer[..keep]);
                truncated |= keep < count;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }
    (result, truncated)
}
fn run(binary: &str, args: &[String], timeout: Duration) -> Result<CommandOutput> {
    use nix::{
        sys::signal::{Signal, killpg},
        unistd::Pid,
    };
    use std::os::unix::process::CommandExt;
    if !Path::new(binary).is_absolute()
        || args.len() > 1024
        || args.iter().any(|arg| arg.contains('\0'))
    {
        return Err(policy_error());
    }
    let mut command = Command::new(binary);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(home) = std::env::var_os("HOME") {
        command.env("HOME", home);
    }
    let mut child = command.spawn().map_err(|_| unavailable())?;
    let group = match i32::try_from(child.id()) {
        Ok(pid) => Pid::from_raw(pid),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unavailable());
        }
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(unavailable());
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(unavailable());
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    let out_sender = sender.clone();
    thread::spawn(move || {
        let _ = out_sender.send((true, collect(stdout)));
    });
    thread::spawn(move || {
        let _ = sender.send((false, collect(stderr)));
    });
    let deadline = Instant::now() + timeout;
    let mut output = None;
    let mut errors = None;
    loop {
        while let Ok((stdout, result)) = receiver.try_recv() {
            if stdout {
                output = Some(result);
            } else {
                errors = Some(result);
            }
        }
        // Keep the helper unreaped until EOF so its PID/group cannot be reused during cleanup.
        if output.is_some()
            && errors.is_some()
            && let Some(status) = child.try_wait().map_err(|_| unavailable())?
        {
            let (stdout, a) = output.take().ok_or_else(unavailable)?;
            let (stderr, b) = errors.take().ok_or_else(unavailable)?;
            return Ok(CommandOutput {
                stdout,
                stderr,
                success: status.success(),
                truncated: a || b,
            });
        }
        if Instant::now() >= deadline {
            let _ = killpg(group, Signal::SIGKILL);
            let _ = child.kill();
            let _ = child.wait();
            return Err(unavailable());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helper_deadline_includes_descendants_holding_output_pipes() {
        let start = Instant::now();
        let result = run(
            "/bin/sh",
            &["-c".into(), "/bin/sleep 2 & exit 0".into()],
            Duration::from_millis(100),
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_millis(750));
    }
}
