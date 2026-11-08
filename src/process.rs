use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    io::Read,
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex, OnceLock},
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
    store::{Launch, Store},
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

const CAPTURE_MESSAGES: usize = 64;
const CAPTURE_CHUNK: usize = 16_384;
const MAX_CAPTURE_STREAMS: usize = 128;
static CAPTURE_SLOTS: OnceLock<Mutex<BTreeMap<PathBuf, usize>>> = OnceLock::new();
struct CaptureSlot {
    directory: PathBuf,
}
impl CaptureSlot {
    fn acquire(directory: &Path) -> Option<Self> {
        let mut slots = CAPTURE_SLOTS
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .ok()?;
        let count = slots.entry(directory.to_owned()).or_default();
        if *count >= MAX_CAPTURE_STREAMS {
            return None;
        }
        *count += 1;
        Some(Self {
            directory: directory.to_owned(),
        })
    }
}
impl Drop for CaptureSlot {
    fn drop(&mut self) {
        if let Ok(mut slots) = CAPTURE_SLOTS
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            && let Some(count) = slots.get_mut(&self.directory)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                slots.remove(&self.directory);
            }
        }
    }
}
#[derive(Default)]
struct CaptureControl {
    closed: bool,
    frozen: bool,
    discarded: u64,
    reason: Option<&'static str>,
    queue: VecDeque<Vec<u8>>,
}
struct CaptureShared {
    control: Mutex<CaptureControl>,
    _slot: Option<CaptureSlot>,
}
struct Capture {
    shared: Arc<CaptureShared>,
    launch: Launch,
    bytes: u64,
    truncated: bool,
    queue_empty: bool,
}
impl Capture {
    fn discard_queued(&mut self) {
        if let Ok(mut control) = self.shared.control.lock() {
            while let Some(bytes) = control.queue.pop_front() {
                control.discarded = control
                    .discarded
                    .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            }
        }
        self.queue_empty = true;
    }
    fn freeze(&mut self, store: &Store) -> Result<()> {
        if let Ok(mut control) = self.shared.control.lock() {
            control.closed = true;
            control.frozen = true;
        }
        self.discard_queued();
        store.freeze_log_capture(&self.launch.attempt_id)?;
        let discarded = if let Ok(mut control) = self.shared.control.lock() {
            std::mem::take(&mut control.discarded)
        } else {
            0
        };
        if discarded > 0 {
            store.stop_log_capture(&self.launch.attempt_id, "LOG_CAPTURE_FROZEN", discarded)?;
            self.truncated = true;
        }
        Ok(())
    }
    fn pump(&mut self, store: &Store, budget: &mut usize) -> Result<()> {
        let mut batch = Vec::new();
        {
            let mut control =
                self.shared.control.lock().map_err(|_| {
                    Error::new("CAPTURE_UNAVAILABLE", "Capture control unavailable")
                })?;
            while *budget > 0 {
                let Some(bytes) = control.queue.pop_front() else {
                    break;
                };
                *budget -= 1;
                batch.extend_from_slice(&bytes);
            }
            self.queue_empty = control.queue.is_empty();
        }
        if !batch.is_empty() {
            let before = store.log_info(&self.launch.attempt_id)?.written_end_offset;
            if store.log_info(&self.launch.attempt_id)?.capture_state
                == crate::contracts::LogCaptureState::Stopped
            {
                store.stop_log_capture(
                    &self.launch.attempt_id,
                    "LOG_CAPTURE_STOPPED",
                    u64::try_from(batch.len()).unwrap_or(u64::MAX),
                )?;
            } else {
                match store.append_log(&self.launch, &batch) {
                    Ok(written) => {
                        if written < batch.len() {
                            self.truncated = true;
                            if let Ok(mut control) = self.shared.control.lock() {
                                control.closed = true;
                                control.reason = Some("LOG_PREFIX_LIMIT");
                            }
                        }
                    }
                    Err(error) => {
                        let after = store.log_info(&self.launch.attempt_id)?.written_end_offset;
                        let lost = u64::try_from(batch.len())
                            .unwrap_or(u64::MAX)
                            .saturating_sub(after.saturating_sub(before));
                        store.stop_log_capture(&self.launch.attempt_id, &error.code, lost)?;
                        self.truncated = true;
                        if let Ok(mut control) = self.shared.control.lock() {
                            control.closed = true;
                            control.reason = Some("LOG_STORAGE_FAILURE");
                        }
                    }
                }
            }
        }
        // Readers may fill or close the queue while append synchronizes storage.
        // Closed admission plus an empty queue is the barrier for persisting STOPPED.
        let (reason, discarded) = {
            let mut control =
                self.shared.control.lock().map_err(|_| {
                    Error::new("CAPTURE_UNAVAILABLE", "Capture control unavailable")
                })?;
            self.queue_empty = control.queue.is_empty();
            if control.closed && self.queue_empty {
                (control.reason, std::mem::take(&mut control.discarded))
            } else {
                (None, 0)
            }
        };
        if let Some(reason) = reason {
            self.truncated = true;
            store.stop_log_capture(&self.launch.attempt_id, reason, discarded)?;
        }
        self.bytes = store.log_info(&self.launch.attempt_id)?.written_end_offset;
        Ok(())
    }
}

pub struct RunningProcess {
    child: Child,
    group: Pid,
    deadline: Instant,
    stopped_at: Option<Instant>,
    stop_state: Option<EndState>,
    exit: Option<ExitStatus>,
    exit_observed_at: Option<Instant>,
    capture: Capture,
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
    pub fn spawn(launch: &Launch, store: &mut Store) -> Result<Self> {
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
        let slot = CaptureSlot::acquire(&store.directory);
        let capture_available = slot.is_some();
        let shared = Arc::new(CaptureShared {
            control: Mutex::new(CaptureControl::default()),
            _slot: slot,
        });
        let mut capture = Capture {
            shared: Arc::clone(&shared),
            launch: launch.clone(),
            bytes: 0,
            truncated: false,
            queue_empty: true,
        };
        if !capture_available {
            crate::logs::ensure_stream(&store.connection, launch, crate::now_us())?;
            store.stop_log_capture(&launch.attempt_id, "CAPTURE_UNAVAILABLE", 0)?;
            capture.truncated = true;
        } else if let Err(error) = store.prepare_log_capture(launch) {
            crate::logs::ensure_stream(&store.connection, launch, crate::now_us())?;
            store.stop_log_capture(&launch.attempt_id, &error.code, 0)?;
            capture.truncated = true;
        }
        let capture_available = capture_available && !capture.truncated;
        let mut command = limited_command(&executable, &execution.limits)?;
        let mut child = command
            .args(&execution.args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .envs(&env)
            .env("CRONHARBOR_LAUNCH_NONCE", &launch.launch_nonce)
            .stdin(Stdio::null())
            .stdout(if capture_available {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(if capture_available {
                Stdio::piped()
            } else {
                Stdio::null()
            })
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
            readers.push(drain(stdout, Arc::clone(&shared), redactions.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(drain(stderr, Arc::clone(&shared), redactions));
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

    pub fn freeze_capture(&mut self, store: &Store) -> Result<()> {
        self.capture.freeze(store)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn cancel(&mut self) {
        if self.exit.is_some()
            && !self.group_alive()
            && self.readers.iter().all(JoinHandle::is_finished)
        {
            return;
        }
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

    pub fn poll(&mut self, store: &mut Store) -> Result<Option<Outcome>> {
        self.poll_with_capture_budget(store, &mut 8)
    }
    pub fn poll_with_capture_budget(
        &mut self,
        store: &mut Store,
        budget: &mut usize,
    ) -> Result<Option<Outcome>> {
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
        let capture_stalled = !group_alive
            && self.exit.is_some()
            && !readers_done
            && self
                .exit_observed_at
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(3));
        let unresolved = capture_stalled
            || (group_alive
                && self
                    .stopped_at
                    .is_some_and(|at| at.elapsed() >= self.stop_grace + Duration::from_secs(2)));
        if unresolved {
            self.capture.freeze(store)?;
            self.finished = true;
            return Ok(Some(Outcome {
                state: EndState::Unknown,
                error_code: Some("TERMINATION_UNCONFIRMED"),
                exit_code: None,
                log_bytes: self.capture.bytes,
                log_truncated: self.capture.truncated,
            }));
        }
        self.capture.pump(store, budget)?;
        if self.exit.is_some() && !group_alive && readers_done && self.capture.queue_empty {
            for reader in self.readers.drain(..) {
                if reader.is_finished() && reader.join().is_err() {
                    self.capture.truncated = true;
                    store.stop_log_capture(
                        &self.capture.launch.attempt_id,
                        "CAPTURE_UNAVAILABLE",
                        0,
                    )?;
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
            let capture = &self.capture;
            self.finished = true;
            return Ok(Some(Outcome {
                state,
                error_code,
                exit_code,
                log_bytes: capture.bytes,
                log_truncated: capture.truncated,
            }));
        }
        if self.service.is_none() && self.exit.is_none() && Instant::now() >= self.deadline {
            self.stop(EndState::TimedOut);
        }
        if self.exit.is_some() && group_alive {
            self.stop(EndState::Failed);
        }
        if let Some(stopped) = self.stopped_at
            && stopped.elapsed() >= self.stop_grace
        {
            let _ = killpg(self.group, Signal::SIGKILL);
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

fn drain(
    mut input: impl Read + Send + 'static,
    shared: Arc<CaptureShared>,
    redactions: Vec<Vec<u8>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        let mut pending = Vec::new();
        let tail = redactions
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(1)
            .saturating_sub(1);
        loop {
            let count = match input.read(&mut buffer) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    if let Ok(mut control) = shared.control.lock() {
                        control.closed = true;
                        control.reason = Some("CAPTURE_READ_FAILED");
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
            for chunk in output.chunks(CAPTURE_CHUNK) {
                let Ok(mut control) = shared.control.lock() else {
                    return;
                };
                if control.closed {
                    control.discarded = control
                        .discarded
                        .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                    continue;
                }
                if let Some(last) = control.queue.back_mut()
                    && last.len() + chunk.len() <= CAPTURE_CHUNK
                {
                    last.extend_from_slice(chunk);
                } else if control.queue.len() < CAPTURE_MESSAGES {
                    control.queue.push_back(chunk.to_vec());
                } else {
                    control.closed = true;
                    control.reason = Some("CAPTURE_BACKPRESSURE");
                    control.discarded = control
                        .discarded
                        .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_capacity_stays_bounded_and_releases_only_its_installation_reservation() {
        let directory = tempfile::tempdir().unwrap();
        let independent = tempfile::tempdir().unwrap();
        let mut held = (0..128)
            .map(|_| CaptureSlot::acquire(directory.path()))
            .collect::<Option<Vec<_>>>()
            .expect("independent captures must fit the bounded capacity");
        assert!(CaptureSlot::acquire(directory.path()).is_none());
        let other = CaptureSlot::acquire(independent.path()).unwrap();
        drop(held.pop());
        let reclaimed = CaptureSlot::acquire(directory.path()).unwrap();
        assert!(CaptureSlot::acquire(directory.path()).is_none());
        drop((held, reclaimed, other));
        assert!(CaptureSlot::acquire(directory.path()).is_some());
    }

    struct ReadFailure;
    impl Read for ReadFailure {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("capture interrupted"))
        }
    }

    fn capture(
        input: impl Read + Send + 'static,
        redactions: Vec<Vec<u8>>,
    ) -> (Vec<u8>, Arc<CaptureShared>) {
        let shared = Arc::new(CaptureShared {
            control: Mutex::new(CaptureControl::default()),
            _slot: None,
        });
        drain(input, Arc::clone(&shared), redactions)
            .join()
            .unwrap();
        let bytes = shared
            .control
            .lock()
            .unwrap()
            .queue
            .drain(..)
            .flatten()
            .collect();
        (bytes, shared)
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
        let input = std::io::Cursor::new(b"safe-prefix:secret-").chain(ReadFailure);
        let (bytes, shared) = capture(input, vec![b"secret-value".to_vec()]);
        assert!(
            b"safe-prefix:".starts_with(&bytes),
            "an incomplete secret was exposed: {bytes:?}"
        );
        assert_eq!(
            shared.control.lock().unwrap().reason,
            Some("CAPTURE_READ_FAILED")
        );
    }

    #[test]
    fn native_capture_redacts_longest_secret_across_read_boundaries() {
        let secret = b"s".repeat(8192);
        let mut input = b"public\n".repeat(1170);
        let mut expected = input.clone();
        input.extend_from_slice(&secret);
        input.extend_from_slice(b"\nsensitive-value\nsensitive\n");
        expected.extend_from_slice(b"[REDACTED]\n[REDACTED]\n[REDACTED]\n");
        let (bytes, shared) = capture(
            std::io::Cursor::new(input),
            vec![secret, b"sensitive-value".to_vec(), b"sensitive".to_vec()],
        );
        assert_eq!(bytes, expected);
        assert!(!shared.control.lock().unwrap().closed);
    }
    #[test]
    fn accepted_chunks_arriving_during_append_are_drained_before_stopped() {
        use std::{os::unix::fs::PermissionsExt, sync::mpsc};
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut store = Store::open(directory.path()).unwrap();
        let mut manifest =
            crate::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.spec.logs.max_run_bytes = 2 * 1024 * 1024;
        store.apply(&manifest, 0).unwrap();
        let launch = store.claim_due(60_000_000).unwrap().unwrap();
        store.start_requested(&launch.attempt_id, 0).unwrap();
        store.running(&launch, 1, 0).unwrap();
        store.prepare_log_capture(&launch).unwrap();
        let log = store.log_path(&launch.attempt_id).unwrap();
        let database = directory.path().join("state.db");
        let shared = Arc::new(CaptureShared {
            control: Mutex::new(CaptureControl::default()),
            _slot: None,
        });
        shared.control.lock().unwrap().queue.push_back(vec![b'a']);
        let other = Arc::clone(&shared);
        let (ready_sender, ready_receiver) = mpsc::channel();
        let producer = thread::spawn(move || {
            let database = rusqlite::Connection::open(database).unwrap();
            database.execute_batch("BEGIN IMMEDIATE").unwrap();
            ready_sender.send(()).unwrap();
            let start = Instant::now();
            while std::fs::metadata(&log).unwrap().len() != 1 {
                assert!(start.elapsed() < Duration::from_secs(3));
                thread::sleep(Duration::from_millis(1));
            }
            let mut control = other.control.lock().unwrap();
            for _ in 0..CAPTURE_MESSAGES {
                control.queue.push_back(vec![b'b'; CAPTURE_CHUNK]);
            }
            control.closed = true;
            control.reason = Some("CAPTURE_BACKPRESSURE");
            control.discarded = 7;
            let high_water = control.queue.len();
            drop(control);
            database.execute_batch("COMMIT").unwrap();
            high_water
        });
        ready_receiver.recv().unwrap();
        let mut capture = Capture {
            shared,
            launch: launch.clone(),
            bytes: 0,
            truncated: false,
            queue_empty: true,
        };
        let started = Instant::now();
        capture.pump(&store, &mut 8).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(producer.join().unwrap(), CAPTURE_MESSAGES);
        assert_eq!(
            store.log_info(&launch.attempt_id).unwrap().capture_state,
            crate::contracts::LogCaptureState::Capturing,
            "accepted generation must finish draining after append I/O"
        );
        for _ in 0..CAPTURE_MESSAGES / 8 {
            capture.pump(&store, &mut 8).unwrap();
        }
        let info = store.log_info(&launch.attempt_id).unwrap();
        assert_eq!(
            info.written_end_offset,
            1 + u64::try_from(CAPTURE_MESSAGES * CAPTURE_CHUNK).unwrap()
        );
        assert_eq!(info.known_discarded_bytes, 7);
        assert_eq!(
            info.capture_state,
            crate::contracts::LogCaptureState::Stopped
        );
        assert_eq!(
            info.last_error_code.as_deref(),
            Some("CAPTURE_BACKPRESSURE")
        );
        eprintln!(
            "append-race fixture queue_high_water={} payload_bound={} first_owner_pump_us={} accepted_bytes={} discarded_bytes={}",
            CAPTURE_MESSAGES,
            CAPTURE_MESSAGES * CAPTURE_CHUNK,
            elapsed.as_micros(),
            info.written_end_offset,
            info.known_discarded_bytes
        );
    }
    #[test]
    fn exited_eof_process_with_queued_bytes_is_not_timed_out_during_owner_drain() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut store = Store::open(directory.path()).unwrap();
        let manifest =
            crate::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        store.apply(&manifest, 0).unwrap();
        let launch = store.claim_due(60_000_000).unwrap().unwrap();
        store.start_requested(&launch.attempt_id, 0).unwrap();
        let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
        store.running(&launch, process.pid(), 0).unwrap();
        let start = Instant::now();
        while process.child.try_wait().unwrap().is_none()
            || !process.readers.iter().all(JoinHandle::is_finished)
        {
            assert!(start.elapsed() < Duration::from_secs(3));
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !process
                .capture
                .shared
                .control
                .lock()
                .unwrap()
                .queue
                .is_empty()
        );
        process.deadline = Instant::now() - Duration::from_millis(1);
        assert!(
            process
                .poll_with_capture_budget(&mut store, &mut 0)
                .unwrap()
                .is_none()
        );
        let outcome = process.poll(&mut store).unwrap().unwrap();
        assert_eq!(outcome.state, EndState::Succeeded);
        assert_eq!(outcome.error_code, None);
        assert!(outcome.log_bytes > 0);
    }
    #[test]
    fn cancel_after_verified_exit_and_eof_preserves_outcome_during_owner_drain() {
        use std::os::unix::fs::PermissionsExt;
        for (exit_code, expected) in [(0, EndState::Succeeded), (7, EndState::Failed)] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let mut store = Store::open(directory.path()).unwrap();
            let mut manifest =
                crate::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
            manifest.spec.execution.command = "/bin/sh".into();
            manifest.spec.execution.args =
                vec!["-c".into(), format!("printf queued; exit {exit_code}")];
            store.apply(&manifest, 0).unwrap();
            let launch = store.claim_due(60_000_000).unwrap().unwrap();
            store.start_requested(&launch.attempt_id, 0).unwrap();
            let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
            store.running(&launch, process.pid(), 0).unwrap();
            let start = Instant::now();
            while process.child.try_wait().unwrap().is_none()
                || !process.readers.iter().all(JoinHandle::is_finished)
            {
                assert!(start.elapsed() < Duration::from_secs(3));
                thread::sleep(Duration::from_millis(1));
            }
            assert!(
                !process
                    .capture
                    .shared
                    .control
                    .lock()
                    .unwrap()
                    .queue
                    .is_empty()
            );
            assert!(
                process
                    .poll_with_capture_budget(&mut store, &mut 0)
                    .unwrap()
                    .is_none()
            );
            assert!(process.exit.is_some());
            assert!(!process.group_alive());
            process.cancel();
            let outcome = process.poll(&mut store).unwrap().unwrap();
            assert_eq!(outcome.state, expected);
            assert_eq!(outcome.exit_code, Some(exit_code));
            assert_eq!(
                outcome.error_code,
                if exit_code == 0 {
                    None
                } else {
                    Some("EXIT_NONZERO")
                }
            );
            assert_eq!(
                store.read_log(&launch.attempt_id, 0, 65536).unwrap().text,
                "queued"
            );
        }
    }

    #[test]
    fn cancellation_still_controls_live_descendants_after_parent_exit_and_eof() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut store = Store::open(directory.path()).unwrap();
        let mut manifest =
            crate::config::Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.spec.execution.command = "/bin/sh".into();
        manifest.spec.execution.args = vec![
            "-c".into(),
            "/bin/sleep 30 </dev/null >/dev/null 2>&1 & printf queued".into(),
        ];
        store.apply(&manifest, 0).unwrap();
        let launch = store.claim_due(60_000_000).unwrap().unwrap();
        store.start_requested(&launch.attempt_id, 0).unwrap();
        let mut process = RunningProcess::spawn(&launch, &mut store).unwrap();
        store.running(&launch, process.pid(), 0).unwrap();
        let start = Instant::now();
        loop {
            process.exit = process.child.try_wait().unwrap();
            if process.exit.is_some() && process.readers.iter().all(JoinHandle::is_finished) {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(3));
            thread::sleep(Duration::from_millis(1));
        }
        assert!(process.group_alive());
        process.cancel();
        assert_eq!(process.stop_state, Some(EndState::Cancelled));
        assert!(process.stopped_at.is_some());
    }
}
