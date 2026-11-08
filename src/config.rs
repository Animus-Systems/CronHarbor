use crate::{Error, Result};
use chrono_tz::Tz;
use croner::Cron;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::File, io::Read, path::Path, str::FromStr};
pub const MAX_CONFIG_BYTES: usize = 65_536;
pub const GRACE_US: i64 = 60_000_000;
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Manifest {
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    pub spec: Spec,
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub name: String,
    pub namespace: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Spec {
    pub owner: String,
    pub enabled: bool,
    #[serde(default)]
    pub schedule: Schedule,
    pub execution: Execution,
    #[serde(default)]
    pub concurrency: Concurrency,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub retry: Retry,
    #[serde(default)]
    pub logs: Logs,
    #[serde(default)]
    pub service: Service,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<LegacyRestart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown: Option<LegacyShutdown>,
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Schedule {
    pub r#type: String,
    #[serde(default)]
    pub expression: String,
    #[serde(default = "utc")]
    pub timezone: String,
    #[serde(default = "first")]
    pub fold: String,
    #[serde(default = "skip")]
    pub gap: String,
    #[serde(default)]
    pub misfire: Misfire,
    #[serde(default)]
    pub interval_seconds: u64,
    #[serde(default)]
    pub anchor_at_us: i64,
}
fn timeout() -> u64 {
    86400
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LegacyRestart {
    pub policy: String,
    pub max_attempts: u32,
    pub window_seconds: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LegacyShutdown {
    pub grace_seconds: u64,
}
fn utc() -> String {
    "UTC".into()
}
fn first() -> String {
    "first".into()
}
fn skip() -> String {
    "skip".into()
}
impl Default for Schedule {
    fn default() -> Self {
        Self {
            r#type: "cron".into(),
            expression: "* * * * *".into(),
            timezone: utc(),
            fold: first(),
            gap: skip(),
            misfire: Misfire::default(),
            interval_seconds: 0,
            anchor_at_us: 0,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Misfire {
    pub policy: String,
    pub lateness_grace_seconds: u64,
    pub max_age_seconds: u64,
    pub max_occurrences: u32,
}
impl Default for Misfire {
    fn default() -> Self {
        Self {
            policy: skip(),
            lateness_grace_seconds: 60,
            max_age_seconds: 3600,
            max_occurrences: 1,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Execution {
    pub adapter: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "cwd")]
    pub working_directory: String,
    #[serde(default = "sandbox")]
    pub sandbox: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub secret_refs: BTreeMap<String, SecretRef>,
    #[serde(default)]
    pub docker: Option<crate::external::DockerSpec>,
    #[serde(default)]
    pub systemd: Option<crate::external::SystemdSpec>,
    #[serde(default)]
    pub limits: ResourceLimits,
}
fn cwd() -> String {
    "/tmp".into()
}
fn sandbox() -> String {
    "default".into()
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, tag = "provider", rename_all = "snake_case")]
pub enum SecretRef {
    Env { name: String },
    File { path: String },
}
/// Installation outbound alert destination. This is restart-only configuration, never persisted.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AlertConfig {
    pub endpoint: String,
    pub token_ref: SecretRef,
    #[serde(default)]
    pub allowed_private_addresses: Vec<std::net::IpAddr>,
    #[serde(default)]
    pub ca_file: Option<std::path::PathBuf>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResourceLimits {
    pub memory_bytes: Option<u64>,
    pub cpu_seconds: Option<u64>,
    pub processes: Option<u32>,
    pub open_files: Option<u64>,
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Concurrency {
    pub mode: String,
    pub max_active: u32,
}
impl Default for Concurrency {
    fn default() -> Self {
        Self {
            mode: "forbid".into(),
            max_active: 1,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Retry {
    pub max_attempts: u32,
    #[serde(default = "retry_policy")]
    pub policy: String,
    #[serde(default = "retry_delay")]
    pub backoff_seconds: u64,
}
fn retry_policy() -> String {
    "never".into()
}
fn retry_delay() -> u64 {
    1
}
impl Default for Retry {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            policy: retry_policy(),
            backoff_seconds: 1,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Logs {
    pub max_run_bytes: u64,
    /// Legacy installation control. Existing values may be retained or removed, but not changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_disk_bytes: Option<u64>,
    /// Legacy installation control; use the installation log storage policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_segments: Option<u32>,
    /// Legacy installation control; use the installation log storage policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<LogRotation>,
}
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LogRotation {
    pub segment_bytes: u64,
    pub max_segments: u32,
}
impl Default for Logs {
    fn default() -> Self {
        Self {
            max_run_bytes: 10485760,
            max_disk_bytes: None,
            max_segments: None,
            retention_seconds: None,
            rotation: None,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase", default)]
pub struct Service {
    pub desired_state: String,
    pub restart: String,
    pub max_restarts: u32,
    pub window_seconds: u64,
    pub backoff_seconds: u64,
    pub max_backoff_seconds: u64,
    pub reset_after_seconds: u64,
    pub stop_grace_seconds: u64,
    pub startup_grace_seconds: u64,
    pub readiness: Option<Probe>,
    pub liveness: Option<Probe>,
    pub restart_on_unhealthy: bool,
}
impl Default for Service {
    fn default() -> Self {
        Self {
            desired_state: "RUNNING".into(),
            restart: "on-failure".into(),
            max_restarts: 5,
            window_seconds: 300,
            backoff_seconds: 1,
            max_backoff_seconds: 60,
            reset_after_seconds: 300,
            stop_grace_seconds: 1,
            startup_grace_seconds: 10,
            readiness: None,
            liveness: None,
            restart_on_unhealthy: false,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Probe {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub interval_seconds: u64,
    pub timeout_seconds: u64,
    #[serde(default = "threshold")]
    pub failure_threshold: u32,
    #[serde(default = "threshold")]
    pub success_threshold: u32,
}
fn threshold() -> u32 {
    1
}
fn invalid(message: &'static str) -> Error {
    Error::new("CONFIG_INVALID", message)
}
pub fn env_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit()))
}
fn argv(command: &str, args: &[String]) -> bool {
    Path::new(command).is_absolute()
        && command.len() <= 4096
        && !command.contains('\0')
        && args.len() <= 256
        && args.iter().all(|s| s.len() <= 8192 && !s.contains('\0'))
}
impl Manifest {
    pub fn read(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|_| invalid("Cannot read manifest"))?;
        let mut text = String::new();
        file.take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_string(&mut text)
            .map_err(|_| invalid("Manifest must be readable UTF-8"))?;
        Self::parse(&text)
    }
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > MAX_CONFIG_BYTES {
            return Err(invalid("Manifest exceeds 64 KiB"));
        }
        let mut value: Self = serde_yaml_ng::from_str(text)
            .map_err(|_| invalid("Manifest does not match the supported schema"))?;
        if let Some(restart) = value.spec.restart.take() {
            value.spec.service.restart = restart.policy;
            value.spec.service.max_restarts = restart.max_attempts;
            value.spec.service.window_seconds = restart.window_seconds;
        }
        if let Some(shutdown) = value.spec.shutdown.take() {
            value.spec.service.stop_grace_seconds = shutdown.grace_seconds;
        }
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<()> {
        if self.api_version != "cronharbor.dev/v1"
            || !matches!(
                self.kind.as_str(),
                "ScheduledJob" | "OneShotJob" | "Service"
            )
        {
            return Err(invalid("Unsupported API version or workload kind"));
        }
        for name in [&self.metadata.name, &self.metadata.namespace] {
            let boundary = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
            if name.is_empty()
                || name.len() > 128
                || !name.bytes().all(|c| boundary(c) || c == b'.' || c == b'-')
                || !name.as_bytes().first().is_some_and(|c| boundary(*c))
                || !name.as_bytes().last().is_some_and(|c| boundary(*c))
            {
                return Err(invalid("Invalid workload name or namespace"));
            }
        }
        if self.metadata.labels.len() > 64
            || self
                .metadata
                .labels
                .iter()
                .any(|(k, v)| k.len() > 128 || v.len() > 256)
        {
            return Err(invalid("Labels exceed schema limits"));
        }
        let s = &self.spec;
        let schedule = &s.schedule;
        if !matches!(s.owner.as_str(), "file" | "api")
            || !matches!(schedule.r#type.as_str(), "cron" | "interval")
            || Tz::from_str(&schedule.timezone).is_err()
            || !matches!(schedule.fold.as_str(), "first" | "second" | "both")
            || !matches!(schedule.gap.as_str(), "skip" | "next_valid")
            || !matches!(
                schedule.misfire.policy.as_str(),
                "skip" | "latest" | "catch_up"
            )
            || schedule.misfire.lateness_grace_seconds != 60
            || !(1..=1000).contains(&schedule.misfire.max_occurrences)
            || !(1..=31_536_000).contains(&schedule.misfire.max_age_seconds)
            || !matches!(s.concurrency.mode.as_str(), "forbid" | "allow")
            || !(1..=256).contains(&s.concurrency.max_active)
            || (s.concurrency.mode == "forbid" && s.concurrency.max_active != 1)
            || !(1..=20).contains(&s.retry.max_attempts)
            || !matches!(
                s.retry.policy.as_str(),
                "never" | "start_rejected" | "idempotent"
            )
            || (s.retry.max_attempts > 1 && s.retry.policy == "never")
            || !(1..=3600).contains(&s.retry.backoff_seconds)
        {
            return Err(invalid(
                "Invalid ownership, schedule, concurrency or explicit retry policy",
            ));
        }
        if schedule.r#type == "cron" {
            if schedule.expression.len() > 128
                || schedule.expression.split_whitespace().count() != 5
                || !schedule
                    .expression
                    .bytes()
                    .all(|c| c.is_ascii_digit() || b"*,-/ \t".contains(&c))
                || Cron::from_str(&schedule.expression).is_err()
            {
                return Err(invalid("Use exactly five valid numeric cron fields"));
            }
        } else if !(1..=31_536_000).contains(&schedule.interval_seconds) {
            return Err(invalid("Interval seconds are outside limits"));
        }
        if self.kind == "ScheduledJob" {
            self.next_after(0)?;
        }
        let e = &s.execution;
        if !matches!(e.adapter.as_str(), "process" | "docker" | "systemd")
            || !matches!(e.sandbox.as_str(), "default" | "trusted")
            || (!matches!(e.adapter.as_str(), "systemd") && !argv(&e.command, &e.args))
            || !Path::new(&e.working_directory).is_absolute()
            || e.working_directory.len() > 4096
            || e.working_directory.contains('\0')
            || (e.adapter == "docker" && e.docker.is_none())
            || (e.adapter == "systemd" && e.systemd.is_none())
            || (e.adapter != "docker" && e.docker.is_some())
            || (e.adapter != "systemd" && e.systemd.is_some())
        {
            return Err(invalid(
                "Use an explicit supported executor, executable and bounded argv/cwd",
            ));
        }
        let limits = [
            e.limits.memory_bytes,
            e.limits.cpu_seconds,
            e.limits.processes.map(u64::from),
            e.limits.open_files,
        ];
        if limits.contains(&Some(0))
            || (e.adapter != "process" && limits.iter().any(Option::is_some))
        {
            return Err(invalid(
                "Native resource limits require the process executor; Docker limits belong in docker and systemd limits remain in the existing unit",
            ));
        }
        if e.adapter == "systemd"
            && (!e.command.is_empty()
                || !e.args.is_empty()
                || e.working_directory != "/tmp"
                || !e.env.is_empty()
                || !e.secret_refs.is_empty())
        {
            return Err(invalid(
                "Systemd command, environment, working directory and credentials remain owned by the existing allowlisted unit",
            ));
        }
        if e.env.len() + e.secret_refs.len() > 64
            || e.env.iter().any(|(k, v)| {
                !env_name(k) || v.len() > 8192 || v.contains('\0') || sensitive_env(k)
            })
            || e.secret_refs
                .keys()
                .any(|k| !env_name(k) || e.env.contains_key(k) || k.starts_with("CRONHARBOR_"))
        {
            return Err(invalid(
                "Invalid environment or sensitive plaintext environment value; use secretRefs",
            ));
        }
        for value in e.secret_refs.values() {
            match value {
                SecretRef::Env { name } if env_name(name) => {}
                SecretRef::File { path }
                    if Path::new(path).is_absolute()
                        && path.len() <= 4096
                        && !path.contains('\0') => {}
                _ => return Err(invalid("Invalid secret reference")),
            }
        }
        if !(1..=31_536_000).contains(&s.timeout_seconds)
            || s.logs.max_run_bytes > 1_073_741_824
            || s.logs
                .max_disk_bytes
                .is_some_and(|v| v == 0 || v > 10_737_418_240)
            || s.logs.max_segments.is_some_and(|v| v == 0 || v > 100000)
            || s.logs
                .retention_seconds
                .is_some_and(|v| v == 0 || v > 9_223_372_036_854)
        {
            return Err(invalid("Timeout or log retention is outside limits"));
        }
        if let Some(rotation) = &s.logs.rotation
            && (e.adapter != "process"
                || rotation.segment_bytes < 65_536
                || !(2..=64).contains(&rotation.max_segments)
                || rotation
                    .segment_bytes
                    .checked_mul(2)
                    .is_none_or(|minimum| s.logs.max_run_bytes < minimum))
        {
            return Err(invalid(
                "Native log rotation requires segments of at least 64 KiB, 2 to 64 segments, and room for two segments",
            ));
        }
        let svc = &s.service;
        if !matches!(svc.desired_state.as_str(), "RUNNING" | "STOPPED")
            || !matches!(svc.restart.as_str(), "never" | "on-failure" | "always")
            || svc.max_restarts > 1000
            || svc.window_seconds == 0
            || svc.backoff_seconds == 0
            || svc.max_backoff_seconds < svc.backoff_seconds
            || svc.max_backoff_seconds > 86400
            || svc.stop_grace_seconds > 300
            || svc.startup_grace_seconds > 3600
        {
            return Err(invalid("Invalid service supervision policy"));
        }
        for p in [&svc.readiness, &svc.liveness].into_iter().flatten() {
            if !argv(&p.command, &p.args)
                || p.interval_seconds == 0
                || p.interval_seconds > 86400
                || p.timeout_seconds == 0
                || p.timeout_seconds > 60
                || p.failure_threshold == 0
                || p.success_threshold == 0
                || e.adapter != "process"
            {
                return Err(invalid(
                    "Exec probes require a native process and bounded timing/argv",
                ));
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| invalid("Cannot normalize manifest"))?
            .len()
            > MAX_CONFIG_BYTES
        {
            return Err(invalid("Normalized manifest exceeds 64 KiB"));
        }
        Ok(())
    }
    pub fn next_after(&self, timestamp_us: i64) -> Result<i64> {
        crate::scheduler::next_after(&self.spec.schedule, timestamp_us)
    }
    pub fn canonical(&self) -> Result<(String, String)> {
        self.validate()?;
        let json = serde_json::to_string(self).map_err(|_| invalid("Cannot normalize manifest"))?;
        let hash = format!("{:x}", Sha256::digest(json.as_bytes()));
        Ok((json, hash))
    }
    pub fn storage_kind(&self) -> &'static str {
        match self.kind.as_str() {
            "Service" => "managed_service",
            "OneShotJob" => "one_shot_job",
            _ => "scheduled_job",
        }
    }
}
fn sensitive_env(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.starts_with("CRONHARBOR_")
        || ["TOKEN", "SECRET", "PASSWORD", "PRIVATE_KEY", "API_KEY"]
            .iter()
            .any(|word| n.contains(word))
}
