use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};

use crate::{
    Error, Result,
    store::{Launch, private_directory, secure_file},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndState {
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
    Unknown,
}

impl EndState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
            Self::TimedOut => "TIMED_OUT",
            Self::Cancelled => "CANCELLED",
            Self::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub state: EndState,
    pub error_code: Option<&'static str>,
    pub exit_code: Option<i32>,
    pub log_bytes: u64,
    pub log_truncated: bool,
}

impl Outcome {
    pub fn rejected() -> Self {
        Self {
            state: EndState::Failed,
            error_code: Some("START_REJECTED"),
            exit_code: None,
            log_bytes: 0,
            log_truncated: false,
        }
    }
}

struct Capture {
    file: File,
    limit: u64,
    bytes: u64,
    truncated: bool,
    redactions: Vec<Vec<u8>>,
    directory: PathBuf,
    disk_limit: u64,
}

pub struct RunningProcess {
    child: Child,
    group: Pid,
    deadline: Instant,
    stopped_at: Option<Instant>,
    stop_state: Option<EndState>,
    exit: Option<ExitStatus>,
    exit_observed_at: Option<Instant>,
    capture: Arc<Mutex<Capture>>,
    readers: Vec<JoinHandle<()>>,
    finished: bool,
    stop_grace: Duration,
    service: Option<crate::config::Service>,
    started: Instant,
    readiness: Option<RunningProbe>,
    liveness: Option<RunningProbe>,
    health_error: bool,
}

impl RunningProcess {
    pub fn spawn(launch: &Launch, state_directory: &Path) -> Result<Self> {
        Self::spawn_with_log_limit(
            launch,
            state_directory,
            launch.manifest.spec.logs.max_disk_bytes,
        )
    }
    pub fn spawn_with_log_limit(
        launch: &Launch,
        state_directory: &Path,
        disk_limit: u64,
    ) -> Result<Self> {
        launch.manifest.validate()?;
        let execution = &launch.manifest.spec.execution;
        if execution.adapter != "process" {
            return Err(Error::new(
                "CONFIG_INVALID",
                "Native executor requires a process workload",
            ));
        }
        let env = resolve_environment(execution)?;
        let redactions = secret_values(execution, &env);
        let executable = std::fs::canonicalize(&execution.command)
            .map_err(|_| Error::new("START_REJECTED", "Executable is unavailable"))?;
        let metadata = std::fs::metadata(&executable)
            .map_err(|_| Error::new("START_REJECTED", "Executable is unavailable"))?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(Error::new(
                "START_REJECTED",
                "Command must be an executable regular file",
            ));
        }
        let cwd = std::fs::canonicalize(&execution.working_directory)
            .map_err(|_| Error::new("START_REJECTED", "Working directory is unavailable"))?;
        if !cwd.is_dir() {
            return Err(Error::new(
                "START_REJECTED",
                "Working directory is unavailable",
            ));
        }
        let logs = private_directory(&state_directory.join("logs"))?;
        let file = secure_file(&logs.join(format!("{}.log", launch.attempt_id)))?;
        let capture = Arc::new(Mutex::new(Capture {
            file,
            limit: launch.manifest.spec.logs.max_run_bytes,
            bytes: 0,
            truncated: false,
            redactions,
            directory: logs.clone(),
            disk_limit: disk_limit.min(launch.manifest.spec.logs.max_disk_bytes),
        }));
        let mut command = limited_command(&executable, &execution.limits)?;
        let mut child = command
            .args(&execution.args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .envs(&env)
            .env("CRONHARBOR_LAUNCH_NONCE", &launch.launch_nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|_| Error::new("START_REJECTED", "Process could not be started"))?;
        let group = match i32::try_from(child.id()) {
            Ok(pid) => Pid::from_raw(pid),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::new(
                    "TERMINATION_UNCONFIRMED",
                    "Process identity could not be represented",
                ));
            }
        };
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(drain(stdout, Arc::clone(&capture)));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(drain(stderr, Arc::clone(&capture)));
        }
        Ok(Self {
            child,
            group,
            deadline: Instant::now() + Duration::from_secs(launch.manifest.spec.timeout_seconds),
            stopped_at: None,
            stop_state: None,
            exit: None,
            exit_observed_at: None,
            capture,
            readers,
            finished: false,
            stop_grace: Duration::from_secs(if launch.manifest.kind == "Service" {
                launch.manifest.spec.service.stop_grace_seconds
            } else {
                1
            }),
            service: if launch.manifest.kind == "Service" {
                Some(launch.manifest.spec.service.clone())
            } else {
                None
            },
            started: Instant::now(),
            readiness: launch.manifest.spec.service.readiness.as_ref().map(|p| {
                RunningProbe::new(p.clone(), execution.working_directory.clone(), env.clone())
            }),
            liveness: launch.manifest.spec.service.liveness.as_ref().map(|p| {
                RunningProbe::new(p.clone(), execution.working_directory.clone(), env.clone())
            }),
            health_error: false,
        })
    }

    pub fn take_probe_events(&mut self) -> Vec<crate::contracts::ProbeEventDto> {
        let mut result = Vec::new();
        for (kind, probe) in [
            ("readiness", &mut self.readiness),
            ("liveness", &mut self.liveness),
        ] {
            if let Some(probe) = probe {
                for (healthy, observed_at_us) in probe.events.drain(..) {
                    result.push(crate::contracts::ProbeEventDto {
                        kind: kind.into(),
                        healthy,
                        observed_at_us,
                    });
                }
            }
        }
        result
    }
    pub fn service_observation(&self) -> &'static str {
        if self.stopped_at.is_some() {
            return "STOPPING";
        }
        if self
            .liveness
            .as_ref()
            .is_some_and(|p| p.healthy == Some(false))
        {
            return "UNHEALTHY";
        }
        if self
            .readiness
            .as_ref()
            .is_none_or(|p| p.healthy == Some(true))
        {
            "READY"
        } else if self
            .readiness
            .as_ref()
            .is_some_and(|p| p.healthy == Some(false))
            && self.service.as_ref().is_some_and(|s| {
                self.started.elapsed() >= Duration::from_secs(s.startup_grace_seconds)
            })
        {
            "UNHEALTHY"
        } else {
            "STARTING"
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn cancel(&mut self) {
        self.stop(EndState::Cancelled);
    }

    fn stop(&mut self, state: EndState) {
        if self.stopped_at.is_none() {
            self.stopped_at = Some(Instant::now());
            self.stop_state = Some(state);
            let _ = killpg(self.group, Signal::SIGTERM);
        }
    }

    fn group_alive(&self) -> bool {
        !matches!(killpg(self.group, None), Err(Errno::ESRCH))
    }

    pub fn poll(&mut self) -> Result<Option<Outcome>> {
        if self.finished {
            return Ok(None);
        }
        if self.exit.is_none() {
            self.exit = self
                .child
                .try_wait()
                .map_err(|_| Error::new("EXECUTOR_UNAVAILABLE", "Cannot observe child process"))?;
            if self.exit.is_some() {
                self.exit_observed_at = Some(Instant::now());
            }
        }
        if self.service.is_some() && self.exit.is_none() && self.stopped_at.is_none() {
            if let Some(probe) = &mut self.readiness {
                probe.poll();
            }
            if let Some(probe) = &mut self.liveness {
                probe.poll();
            }
            if self.service.as_ref().is_some_and(|svc| {
                svc.restart_on_unhealthy
                    && self.started.elapsed() >= Duration::from_secs(svc.startup_grace_seconds)
            }) && self
                .liveness
                .as_ref()
                .is_some_and(|p| p.healthy == Some(false))
            {
                self.health_error = true;
                self.stop(EndState::Failed);
            }
        }
        let readers_done = self.readers.iter().all(JoinHandle::is_finished);
        let group_alive = self.group_alive();
        if self.exit.is_some() && !group_alive && readers_done {
            for reader in self.readers.drain(..) {
                if reader.join().is_err() {
                    return Err(Error::new("EXECUTOR_UNAVAILABLE", "Log capture failed"));
                }
            }
            let exit_code = self.exit.and_then(|status| status.code());
            let state = self.stop_state.unwrap_or_else(|| {
                if self.exit.is_some_and(|status| status.success()) {
                    EndState::Succeeded
                } else {
                    EndState::Failed
                }
            });
            let error_code = if self.health_error {
                Some("HEALTH_FAILED")
            } else {
                match state {
                    EndState::Succeeded => None,
                    EndState::TimedOut => Some("DEADLINE_EXCEEDED"),
                    EndState::Cancelled => Some("CANCELLED_BY_OPERATOR"),
                    _ if self.stop_state.is_some() => Some("DESCENDANTS_OUTLIVED_PARENT"),
                    _ => Some("EXIT_NONZERO"),
                }
            };
            let capture = self
                .capture
                .lock()
                .map_err(|_| Error::new("EXECUTOR_UNAVAILABLE", "Log capture failed"))?;
            self.finished = true;
            return Ok(Some(Outcome {
                state,
                error_code,
                exit_code,
                log_bytes: capture.bytes,
                log_truncated: capture.truncated,
            }));
        }
        if self.service.is_none() && Instant::now() >= self.deadline {
            self.stop(EndState::TimedOut);
        }
        if self.exit.is_some() && group_alive {
            self.stop(EndState::Failed);
        }
        // EOF may lag process exit; only unresolved capture after a bounded drain is uncertain.
        if !group_alive
            && !readers_done
            && self
                .exit_observed_at
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(3))
        {
            let capture = self
                .capture
                .lock()
                .map_err(|_| Error::new("EXECUTOR_UNAVAILABLE", "Log capture failed"))?;
            self.finished = true;
            return Ok(Some(Outcome {
                state: EndState::Unknown,
                error_code: Some("TERMINATION_UNCONFIRMED"),
                exit_code: None,
                log_bytes: capture.bytes,
                log_truncated: capture.truncated,
            }));
        }
        if let Some(stopped) = self.stopped_at {
            if stopped.elapsed() >= self.stop_grace {
                let _ = killpg(self.group, Signal::SIGKILL);
            }
            if stopped.elapsed() >= self.stop_grace + Duration::from_secs(2) {
                let capture = self
                    .capture
                    .lock()
                    .map_err(|_| Error::new("EXECUTOR_UNAVAILABLE", "Log capture failed"))?;
                self.finished = true;
                return Ok(Some(Outcome {
                    state: EndState::Unknown,
                    error_code: Some("TERMINATION_UNCONFIRMED"),
                    exit_code: None,
                    log_bytes: capture.bytes,
                    log_truncated: capture.truncated,
                }));
            }
        }
        Ok(None)
    }
}

impl Drop for RunningProcess {
    fn drop(&mut self) {
        if !self.finished {
            let _ = killpg(self.group, Signal::SIGKILL);
            let _ = self.child.wait();
        }
    }
}

fn drain(mut input: impl Read + Send + 'static, capture: Arc<Mutex<Capture>>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        let mut pending = Vec::new();
        let (redactions, tail) = match capture.lock() {
            Ok(log) => (
                log.redactions.clone(),
                log.redactions
                    .iter()
                    .map(Vec::len)
                    .max()
                    .unwrap_or(1)
                    .saturating_sub(1),
            ),
            Err(_) => return,
        };
        loop {
            let count = match input.read(&mut buffer) {
                Ok(count) => count,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    if let Ok(mut log) = capture.lock() {
                        log.truncated = true;
                    }
                    break;
                }
            };
            pending.extend_from_slice(&buffer[..count]);
            let mut consumed = 0;
            let mut output = Vec::new();
            while consumed < pending.len() && (count == 0 || pending.len() - consumed > tail) {
                if let Some(secret) = redactions
                    .iter()
                    .filter(|secret| !secret.is_empty() && pending[consumed..].starts_with(secret))
                    .max_by_key(|secret| secret.len())
                {
                    output.extend_from_slice(b"[REDACTED]");
                    consumed += secret.len();
                } else {
                    output.push(pending[consumed]);
                    consumed += 1;
                }
            }
            pending.drain(..consumed);
            if let Ok(mut log) = capture.lock() {
                let allowed = usize::try_from(log.limit.saturating_sub(log.bytes))
                    .unwrap_or(usize::MAX)
                    .min(output.len());
                if allowed < output.len() {
                    log.truncated = true;
                }
                if allowed > 0 {
                    let directory = log.directory.clone();
                    let disk_limit = log.disk_limit;
                    match write_log_chunk(&mut log.file, &directory, disk_limit, &output[..allowed])
                    {
                        Ok((written, truncated)) => {
                            log.bytes += u64::try_from(written).unwrap_or(0);
                            log.truncated |= truncated;
                        }
                        Err(_) => {
                            log.truncated = true;
                            log.limit = log.bytes;
                        }
                    }
                }
            } else {
                return;
            }
            if count == 0 {
                break;
            }
        }
    })
}

pub fn resolve_environment(
    execution: &crate::config::Execution,
) -> Result<BTreeMap<String, String>> {
    let mut env = execution.env.clone();
    for (key, reference) in &execution.secret_refs {
        let value = match reference {
            crate::config::SecretRef::Env { name } => std::env::var(name).map_err(|_| {
                Error::new(
                    "START_REJECTED",
                    "Referenced environment secret is unavailable",
                )
            })?,
            crate::config::SecretRef::File { path } => {
                let metadata = std::fs::symlink_metadata(path).map_err(|_| {
                    Error::new("START_REJECTED", "Referenced file secret is unavailable")
                })?;
                if !metadata.is_file()
                    || metadata.uid() != nix::unistd::geteuid().as_raw()
                    || metadata.mode() & 0o077 != 0
                    || metadata.nlink() != 1
                    || metadata.len() > 8192
                {
                    return Err(Error::new(
                        "START_REJECTED",
                        "Referenced file secret must be a private bounded owned file",
                    ));
                }
                let mut value = String::new();
                File::open(path)
                    .and_then(|file| file.take(8193).read_to_string(&mut value))
                    .map_err(|_| {
                        Error::new("START_REJECTED", "Referenced file secret is unavailable")
                    })?;
                value.trim_end_matches(['\n', '\r']).to_owned()
            }
        };
        if value.is_empty() || value.len() > 8192 || value.contains('\0') {
            return Err(Error::new(
                "START_REJECTED",
                "Referenced secret has an unsupported size or format",
            ));
        }
        env.insert(key.clone(), value);
    }
    Ok(env)
}
pub fn secret_values(
    execution: &crate::config::Execution,
    env: &BTreeMap<String, String>,
) -> Vec<Vec<u8>> {
    let mut values = execution
        .secret_refs
        .keys()
        .filter_map(|key| env.get(key))
        .map(|v| v.as_bytes().to_vec())
        .collect::<Vec<_>>();
    values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    values.dedup();
    values
}
pub fn redact(bytes: Vec<u8>, secrets: &[Vec<u8>]) -> Vec<u8> {
    redact_bounded(&bytes, secrets, usize::MAX).0
}

pub(crate) fn redact_bounded(
    bytes: &[u8],
    secrets: &[Vec<u8>],
    max_bytes: usize,
) -> (Vec<u8>, bool) {
    let mut output = Vec::with_capacity(bytes.len().min(max_bytes));
    let mut index = 0;
    while index < bytes.len() {
        let secret = secrets
            .iter()
            .filter(|secret| !secret.is_empty() && bytes[index..].starts_with(secret))
            .max_by_key(|secret| secret.len());
        let replacement = if let Some(secret) = secret {
            index += secret.len();
            b"[REDACTED]".as_slice()
        } else {
            index += 1;
            &bytes[index - 1..index]
        };
        let allowed = replacement
            .len()
            .min(max_bytes.saturating_sub(output.len()));
        output.extend_from_slice(&replacement[..allowed]);
        if allowed < replacement.len() {
            return (output, true);
        }
    }
    (output, false)
}
fn limited_command(executable: &Path, limits: &crate::config::ResourceLimits) -> Result<Command> {
    if limits.memory_bytes.is_none()
        && limits.cpu_seconds.is_none()
        && limits.processes.is_none()
        && limits.open_files.is_none()
    {
        return Ok(Command::new(executable));
    }
    #[cfg(target_os = "linux")]
    {
        if limits.processes.is_some() && nix::unistd::geteuid().is_root() {
            return Err(Error::new(
                "UNSUPPORTED_RESOURCE_LIMIT",
                "Linux process-count limits cannot constrain UID 0; run as an unprivileged UID",
            ));
        }
        let binary = Path::new("/usr/bin/prlimit");
        if !binary.is_file() {
            return Err(Error::new(
                "UNSUPPORTED_RESOURCE_LIMIT",
                "Native resource limits require /usr/bin/prlimit",
            ));
        }
        let mut command = Command::new(binary);
        for (flag, value) in [
            ("--as", limits.memory_bytes),
            ("--cpu", limits.cpu_seconds),
            ("--nproc", limits.processes.map(u64::from)),
            ("--nofile", limits.open_files),
        ] {
            if let Some(value) = value {
                if value == 0 {
                    return Err(Error::new(
                        "CONFIG_INVALID",
                        "Resource limits must be positive",
                    ));
                }
                command.arg(format!("{flag}={value}:{value}"));
            }
        }
        command.arg("--").arg(executable);
        Ok(command)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = executable;
        Err(Error::new(
            "UNSUPPORTED_RESOURCE_LIMIT",
            "Native resource limits are supported only on Linux",
        ))
    }
}
struct RunningProbe {
    spec: crate::config::Probe,
    cwd: String,
    env: BTreeMap<String, String>,
    child: Option<Child>,
    group: Option<Pid>,
    started: Option<Instant>,
    next: Instant,
    failures: u32,
    successes: u32,
    healthy: Option<bool>,
    events: Vec<(bool, i64)>,
    result: Option<bool>,
    cleanup_started: Option<Instant>,
    cleanup_reported: bool,
}
impl RunningProbe {
    fn new(spec: crate::config::Probe, cwd: String, env: BTreeMap<String, String>) -> Self {
        Self {
            spec,
            cwd,
            env,
            child: None,
            group: None,
            started: None,
            next: Instant::now(),
            failures: 0,
            successes: 0,
            healthy: None,
            events: Vec::new(),
            result: None,
            cleanup_started: None,
            cleanup_reported: false,
        }
    }
    fn record(&mut self, passed: bool) {
        self.events.push((passed, crate::now_us()));
        if passed {
            self.failures = 0;
            self.successes = self.successes.saturating_add(1);
            if self.successes >= self.spec.success_threshold {
                self.healthy = Some(true);
            }
        } else {
            self.successes = 0;
            self.failures = self.failures.saturating_add(1);
            if self.failures >= self.spec.failure_threshold {
                self.healthy = Some(false);
            }
        }
    }
    fn poll(&mut self) {
        let mut complete = None;
        if let (Some(child), Some(group)) = (&mut self.child, self.group) {
            if self.result.is_none() {
                self.result = probe_exit(child);
                if self.started.is_some_and(|at| {
                    at.elapsed() >= Duration::from_secs(self.spec.timeout_seconds)
                }) {
                    self.result = Some(false);
                }
            }
            if let Some(passed) = self.result {
                if self.cleanup_started.is_none() {
                    let _ = killpg(group, Signal::SIGKILL);
                    self.cleanup_started = Some(Instant::now());
                }
                // Linux observes with WNOWAIT above, so the leader pins its PID through the group signal.
                let reaped = child.try_wait().is_ok_and(|status| status.is_some());
                let absent = matches!(killpg(group, None), Err(Errno::ESRCH));
                if reaped && absent {
                    complete = Some(passed);
                } else if self
                    .cleanup_started
                    .is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
                {
                    if !self.cleanup_reported {
                        self.cleanup_reported = true;
                        self.healthy = Some(false);
                        self.events.push((false, crate::now_us()));
                    }
                } else {
                    self.healthy = None;
                }
            }
        }
        if let Some(passed) = complete {
            self.record(passed);
            self.child = None;
            self.group = None;
            self.started = None;
            self.result = None;
            self.cleanup_started = None;
            self.cleanup_reported = false;
            self.next = Instant::now() + Duration::from_secs(self.spec.interval_seconds);
        }
        if self.child.is_none() && self.group.is_none() && Instant::now() >= self.next {
            match Command::new(&self.spec.command)
                .args(&self.spec.args)
                .current_dir(&self.cwd)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .envs(&self.env)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
            {
                Ok(mut child) => match i32::try_from(child.id()) {
                    Ok(pid) => {
                        self.group = Some(Pid::from_raw(pid));
                        self.child = Some(child);
                        self.started = Some(Instant::now());
                    }
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        self.record(false);
                        self.next =
                            Instant::now() + Duration::from_secs(self.spec.interval_seconds);
                    }
                },
                Err(_) => {
                    self.record(false);
                    self.next = Instant::now() + Duration::from_secs(self.spec.interval_seconds);
                }
            }
        }
    }
}
fn probe_exit(child: &mut Child) -> Option<bool> {
    #[cfg(all(target_os = "linux", not(target_env = "uclibc")))]
    {
        use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid};
        let Ok(raw) = i32::try_from(child.id()) else {
            return Some(false);
        };
        match waitid(
            Id::Pid(Pid::from_raw(raw)),
            WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
        ) {
            Ok(WaitStatus::StillAlive) => None,
            Ok(WaitStatus::Exited(_, code)) => Some(code == 0),
            Ok(_) => Some(false),
            Err(Errno::EINTR) => None,
            Err(_) => Some(false),
        }
    }
    #[cfg(not(all(target_os = "linux", not(target_env = "uclibc"))))]
    {
        match child.try_wait() {
            Ok(Some(status)) => Some(status.success()),
            Ok(None) => None,
            Err(_) => Some(false),
        }
    }
}
impl Drop for RunningProbe {
    fn drop(&mut self) {
        if let Some(group) = self.group {
            let _ = killpg(group, Signal::SIGKILL);
        }
        if let Some(child) = &mut self.child {
            let deadline = Instant::now() + Duration::from_secs(1);
            while child.try_wait().is_ok_and(|status| status.is_none()) && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

static LOG_WRITE_LOCK: Mutex<()> = Mutex::new(());
pub fn write_log_chunk(
    file: &mut File,
    directory: &Path,
    disk_limit: u64,
    bytes: &[u8],
) -> std::io::Result<(usize, bool)> {
    let _guard = LOG_WRITE_LOCK
        .lock()
        .map_err(|_| std::io::Error::other("Log write lock unavailable"))?;
    let mut used = 0_u64;
    for entry in std::fs::read_dir(directory)? {
        let metadata = entry?.metadata()?;
        if metadata.is_file() {
            used = used.saturating_add(metadata.len());
        }
    }
    let allowed = bytes
        .len()
        .min(usize::try_from(disk_limit.saturating_sub(used)).unwrap_or(usize::MAX));
    file.write_all(&bytes[..allowed])?;
    Ok((allowed, allowed < bytes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ReadFailure;
    impl Read for ReadFailure {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("capture interrupted"))
        }
    }

    fn capture(directory: &Path, redactions: Vec<Vec<u8>>) -> Arc<Mutex<Capture>> {
        Arc::new(Mutex::new(Capture {
            file: secure_file(&directory.join("capture.log")).unwrap(),
            limit: 65_536,
            bytes: 0,
            truncated: false,
            redactions,
            directory: directory.to_owned(),
            disk_limit: 65_536,
        }))
    }

    #[test]
    fn bounded_redaction_caps_expansion_and_reports_only_omitted_output() {
        let secrets = [b"s".to_vec()];
        for (input, limit, expected, truncated) in [
            (b"s".as_slice(), 10, b"[REDACTED]".as_slice(), false),
            (b"ss".as_slice(), 10, b"[REDACTED]".as_slice(), true),
            (b"s".as_slice(), 4, b"[RED".as_slice(), true),
            (b"".as_slice(), 0, b"".as_slice(), false),
            (b"s".as_slice(), 0, b"".as_slice(), true),
        ] {
            let (bytes, actual_truncated) = redact_bounded(input, &secrets, limit);
            assert!(bytes.len() <= limit);
            assert_eq!(bytes, expected);
            assert_eq!(actual_truncated, truncated);
        }
    }

    #[test]
    fn read_failure_withholds_a_buffered_secret_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let capture = capture(directory.path(), vec![b"secret-value".to_vec()]);
        let input = std::io::Cursor::new(b"safe-prefix:secret-").chain(ReadFailure);
        drain(input, Arc::clone(&capture)).join().unwrap();
        let bytes = std::fs::read(directory.path().join("capture.log")).unwrap();
        assert!(
            b"safe-prefix:".starts_with(&bytes),
            "an incomplete secret was exposed: {bytes:?}"
        );
        assert!(capture.lock().unwrap().truncated);
    }

    #[test]
    fn native_capture_redacts_longest_secret_across_read_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let secret = b"s".repeat(8192);
        let mut input = b"public\n".repeat(1170);
        let mut expected = input.clone();
        input.extend_from_slice(&secret);
        input.extend_from_slice(b"\nsensitive-value\nsensitive\n");
        expected.extend_from_slice(b"[REDACTED]\n[REDACTED]\n[REDACTED]\n");
        let capture = capture(
            directory.path(),
            vec![secret, b"sensitive-value".to_vec(), b"sensitive".to_vec()],
        );
        drain(std::io::Cursor::new(input), Arc::clone(&capture))
            .join()
            .unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("capture.log")).unwrap(),
            expected
        );
        assert!(!capture.lock().unwrap().truncated);
    }
}
