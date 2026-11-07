use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Error, Result,
    config::Manifest,
    process::{EndState, Outcome, RunningProcess},
    store::{Launch, Store, protect_socket},
};

const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Status,
    Apply {
        manifest: Box<Manifest>,
    },
    Executions,
    Logs {
        attempt_id: String,
    },
    Execute {
        #[serde(rename = "app_command")]
        command: Box<crate::contracts::AppCommand>,
        token: Option<String>,
    },
}

#[derive(Deserialize, Serialize)]
struct Response {
    data: Option<Value>,
    error: Option<Error>,
}

struct SocketGuard(PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn transport_error() -> Error {
    Error::new(
        "LOCAL_TRANSPORT_UNAVAILABLE",
        "Cannot communicate with the local daemon",
    )
}

pub fn client(directory: &Path, request: Request) -> Result<Value> {
    let mut stream =
        UnixStream::connect(directory.join("control.sock")).map_err(|_| transport_error())?;
    let mut encoded = serde_json::to_vec(&request).map_err(|_| transport_error())?;
    if encoded.len() + 1 > MAX_REQUEST_BYTES {
        return Err(Error::new(
            "INPUT_INVALID",
            "Request exceeds the local command limit",
        ));
    }
    encoded.push(b'\n');
    write_frame(&mut stream, &encoded, Duration::from_secs(5))?;
    let line = read_line(&mut stream, MAX_RESPONSE_BYTES + 1, Duration::from_secs(5))
        .map_err(|_| transport_error())?;
    let response: Response = serde_json::from_slice(&line).map_err(|_| transport_error())?;
    if let Some(error) = response.error {
        return Err(error);
    }
    response.data.ok_or_else(transport_error)
}

fn read_line(stream: &mut UnixStream, cap: u64, budget: Duration) -> Result<Vec<u8>> {
    let cap = usize::try_from(cap).map_err(|_| transport_error())?;
    stream
        .set_nonblocking(true)
        .map_err(|_| transport_error())?;
    let deadline = Instant::now() + budget;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if Instant::now() >= deadline {
            return Err(transport_error());
        }
        let count = match stream.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(transport_error()),
        };
        if count == 0 || bytes.len() + count > cap {
            return Err(Error::new(
                "INPUT_INVALID",
                "Expected one bounded newline-delimited JSON message",
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
        if buffer[..count].contains(&b'\n') {
            if bytes.last() != Some(&b'\n') {
                return Err(Error::new(
                    "INPUT_INVALID",
                    "Only one local command is allowed per connection",
                ));
            }
            return Ok(bytes);
        }
    }
}

fn write_frame(stream: &mut UnixStream, mut bytes: &[u8], budget: Duration) -> Result<()> {
    stream
        .set_nonblocking(true)
        .map_err(|_| transport_error())?;
    let deadline = Instant::now() + budget;
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err(transport_error());
        }
        let count = match stream.write(bytes) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(transport_error()),
        };
        if count == 0 {
            return Err(transport_error());
        }
        bytes = &bytes[count..];
    }
    Ok(())
}

pub fn run(directory: &Path, manifest: Option<&Manifest>, paused: bool) -> Result<()> {
    run_with_shutdown(
        directory,
        &manifest.cloned().into_iter().collect::<Vec<_>>(),
        paused,
        Arc::new(AtomicBool::new(false)),
    )
}

pub fn run_with_shutdown(
    directory: &Path,
    manifests: &[Manifest],
    paused: bool,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut store = Store::open(directory)?;
    let mut identities = std::collections::BTreeSet::new();
    for manifest in manifests {
        manifest.validate()?;
        if !identities.insert(format!(
            "{}/{}",
            manifest.metadata.namespace, manifest.metadata.name
        )) {
            return Err(Error::new(
                "CONFIG_INVALID",
                "Configuration contains duplicate workload identities",
            ));
        }
    }
    store.connection.execute_batch("SAVEPOINT startup_config")?;
    let applied = (|| {
        for manifest in manifests {
            store.apply(manifest, crate::now_us())?;
        }
        Ok(())
    })();
    match applied {
        Ok(()) => store.connection.execute_batch("RELEASE startup_config")?,
        Err(error) => {
            store
                .connection
                .execute_batch("ROLLBACK TO startup_config; RELEASE startup_config")?;
            return Err(error);
        }
    }
    if paused {
        store.set_setting("global_paused", "true")?;
    }
    store.retain_logs()?;
    let socket = store.directory.join("control.sock");
    match fs::symlink_metadata(&socket) {
        Ok(meta)
            if meta.file_type().is_socket() && meta.uid() == nix::unistd::geteuid().as_raw() =>
        {
            fs::remove_file(&socket).map_err(|_| transport_error())?
        }
        Ok(_) => {
            return Err(Error::new(
                "UNSAFE_STATE_PATH",
                "Refusing to remove a non-socket control path",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(transport_error()),
    }
    let listener = UnixListener::bind(&socket).map_err(|_| transport_error())?;
    let _guard = SocketGuard(socket.clone());
    protect_socket(&socket)?;
    listener
        .set_nonblocking(true)
        .map_err(|_| transport_error())?;
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&shutdown))
            .map_err(|_| Error::new("SIGNAL_UNAVAILABLE", "Cannot register shutdown handler"))?;
    }
    let mut native: Vec<(Launch, RunningProcess)> = Vec::new();
    let mut pending: Vec<PendingStart> = Vec::new();
    let mut external: Vec<ExternalWatch> = Vec::new();
    let mut cleanup: Option<(String, thread::JoinHandle<bool>)> = None;
    let mut recovered_at = Instant::now() - Duration::from_secs(10);
    loop {
        let stopping = shutdown.load(Ordering::Relaxed);
        let now = crate::now_us();
        let mut index = 0;
        while index < native.len() {
            let requested = store.cancel_requested(&native[index].0.attempt_id)?
                || store.service_should_stop(&native[index].0)?;
            let cancel = stopping || requested;
            if cancel {
                native[index].1.cancel();
            }
            let polled = native[index].1.poll();
            let probe_events = native[index].1.take_probe_events();
            store.record_probe_events(&native[index].0, &probe_events)?;
            match polled {
                Ok(Some(mut outcome)) => {
                    if stopping && !requested && outcome.state == EndState::Cancelled {
                        outcome.error_code = Some("DAEMON_SHUTDOWN");
                    }
                    store.finish(&native[index].0, &outcome, now)?;
                    native.swap_remove(index);
                }
                Ok(None) => {
                    let state = native[index].1.service_observation();
                    store.service_observation(&native[index].0, state, now)?;
                    index += 1;
                }
                Err(_) => {
                    let outcome = unknown_outcome("EXECUTOR_UNAVAILABLE");
                    store.finish(&native[index].0, &outcome, now)?;
                    native.swap_remove(index);
                }
            }
        }
        let mut index = 0;
        while index < pending.len() {
            if pending[index].task.is_finished() {
                let item = pending.swap_remove(index);
                match item.task.join() {
                    Ok(Ok(handle)) => {
                        let encoded =
                            serde_json::to_string(&handle).map_err(|_| transport_error())?;
                        store.running_handle(&item.launch, &encoded, now)?;
                        let started = store.attempt_started_at(&item.launch.attempt_id)?;
                        external.push(ExternalWatch::new(
                            item.launch,
                            handle,
                            item.secrets,
                            started,
                            item.disk_limit,
                        ));
                    }
                    Ok(Err(error)) => {
                        let outcome = if matches!(
                            error.code.as_str(),
                            "START_REJECTED" | "CONFIG_INVALID" | "NOT_AUTHORIZED"
                        ) {
                            Outcome::rejected()
                        } else {
                            unknown_outcome("EXECUTOR_UNAVAILABLE")
                        };
                        store.finish(&item.launch, &outcome, now)?;
                    }
                    Err(_) => {
                        store.finish(&item.launch, &unknown_outcome("EXECUTOR_UNAVAILABLE"), now)?
                    }
                }
            } else {
                index += 1;
            }
        }
        let mut index = 0;
        while index < external.len() {
            if !store.attempt_is_observable(&external[index].launch.attempt_id)? {
                external[index].retired.store(true, Ordering::Relaxed);
                if external[index]
                    .task
                    .as_ref()
                    .is_none_or(thread::JoinHandle::is_finished)
                {
                    if let Some(task) = external[index].task.take() {
                        let _ = task.join();
                    }
                    external.swap_remove(index);
                } else {
                    index += 1;
                }
                continue;
            }
            let stop = store.cancel_requested(&external[index].launch.attempt_id)?
                || store.service_should_stop(&external[index].launch)?;
            match external[index].poll(stop, &store.directory) {
                Some(ExternalResult::Retired) => {
                    external.swap_remove(index);
                }
                Some(ExternalResult::Running(healthy)) => {
                    if store.attempt_is_unknown(&external[index].launch.attempt_id)? {
                        let encoded = serde_json::to_string(&external[index].handle)
                            .map_err(|_| transport_error())?;
                        store.running_handle(&external[index].launch, &encoded, now)?;
                    }
                    let state = if healthy == Some(false) {
                        "UNHEALTHY"
                    } else if healthy == Some(true)
                        || external[index].launch.manifest.spec.execution.adapter == "docker"
                    {
                        "READY"
                    } else {
                        "STARTING"
                    };
                    store.record_adapter_observation(
                        &external[index].launch.manifest.spec.execution.adapter,
                        "AVAILABLE",
                        "Owned executor identity was observed",
                        now,
                    )?;
                    store.service_observation(&external[index].launch, state, now)?;
                    index += 1;
                }
                Some(ExternalResult::Finished(outcome, logs_persisted)) => {
                    store.finish_external(
                        &external[index].launch,
                        &outcome,
                        logs_persisted,
                        &external[index].handle,
                        now,
                    )?;
                    external.swap_remove(index);
                }
                None => index += 1,
            }
        }
        if cleanup.as_ref().is_some_and(|(_, task)| task.is_finished())
            && let Some((attempt, task)) = cleanup.take()
        {
            store.finish_external_cleanup(&attempt, task.join().unwrap_or(false))?;
        }
        if !stopping
            && !store.restore_required()?
            && cleanup.is_none()
            && let Some((attempt, handle)) = store.next_external_cleanup()?
        {
            let directory = store.directory.clone();
            cleanup = Some((
                attempt,
                thread::spawn(move || {
                    crate::external::load_policy(&directory)
                        .and_then(|policy| crate::external::remove_finished(&policy, &handle))
                        .is_ok()
                }),
            ));
        }
        if stopping
            && native.is_empty()
            && pending.is_empty()
            && external
                .iter()
                .all(|watch| watch.task.is_none() && !watch.stopping)
        {
            break;
        }
        // Reattach only to observation of durable, executor-verified identities.
        if !stopping
            && !store.restore_required()?
            && recovered_at.elapsed() >= Duration::from_secs(10)
        {
            recovered_at = Instant::now();
            for (launch, handle) in store.recoverable_external()? {
                if external
                    .iter()
                    .any(|watch| watch.launch.attempt_id == launch.attempt_id)
                {
                    continue;
                }
                let secrets = if matches!(handle, crate::external::ExternalHandle::Systemd { .. })
                    && launch.manifest.spec.execution.secret_refs.is_empty()
                {
                    Some(Vec::new())
                } else {
                    None
                };
                let started = store.attempt_started_at(&launch.attempt_id)?;
                external.push(ExternalWatch::new(
                    launch,
                    handle,
                    secrets,
                    started,
                    store.log_disk_limit()?,
                ));
            }
        }
        for _ in 0..16 {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let result = if peer_is_owner(&stream) {
                        handle(&mut store, &mut stream, stopping)
                    } else {
                        Err(Error::new(
                            "NOT_AUTHORIZED",
                            "Local socket peer must be the daemon owner",
                        ))
                    };
                    for watch in &external {
                        if !store.attempt_is_observable(&watch.launch.attempt_id)? {
                            watch.retired.store(true, Ordering::Relaxed);
                        }
                    }
                    let response = match result {
                        Ok(data) => Response {
                            data: Some(data),
                            error: None,
                        },
                        Err(error) => Response {
                            data: None,
                            error: Some(error),
                        },
                    };
                    if let Ok(mut encoded) = serde_json::to_vec(&response) {
                        if encoded.len()
                            > usize::try_from(MAX_RESPONSE_BYTES).unwrap_or(2 * 1024 * 1024)
                        {
                            encoded = serde_json::to_vec(&Response {
                                data: None,
                                error: Some(Error::new(
                                    "RESOURCE_EXHAUSTED",
                                    "Response exceeds the bounded transport budget",
                                )),
                            })
                            .map_err(|_| transport_error())?;
                        }
                        encoded.push(b'\n');
                        let _ = write_frame(&mut stream, &encoded, Duration::from_secs(2));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return Err(transport_error()),
            }
        }
        if !stopping {
            for _ in 0..4 {
                let Some(launch) = store.claim_due(crate::now_us())? else {
                    break;
                };
                dispatch(&mut store, launch, &mut native, &mut pending)?;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
fn peer_is_owner(stream: &UnixStream) -> bool {
    #[cfg(target_os = "linux")]
    {
        nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
            .is_ok_and(|credentials| credentials.uid() == nix::unistd::geteuid().as_raw())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        true
    }
}
fn unknown_outcome(code: &'static str) -> Outcome {
    Outcome {
        state: EndState::Unknown,
        error_code: Some(code),
        exit_code: None,
        log_bytes: 0,
        log_truncated: false,
    }
}
struct PendingStart {
    launch: Launch,
    task: thread::JoinHandle<Result<crate::external::ExternalHandle>>,
    secrets: Option<Vec<Vec<u8>>>,
    disk_limit: u64,
}
fn dispatch(
    store: &mut Store,
    launch: Launch,
    native: &mut Vec<(Launch, RunningProcess)>,
    pending: &mut Vec<PendingStart>,
) -> Result<()> {
    if launch.manifest.spec.execution.adapter == "process" {
        store.retain_logs()?;
        store.start_requested(&launch.attempt_id, crate::now_us())?;
        match RunningProcess::spawn_with_log_limit(
            &launch,
            &store.directory,
            store.log_disk_limit()?,
        ) {
            Ok(process) => {
                store.running(&launch, process.pid(), crate::now_us())?;
                native.push((launch, process));
            }
            Err(error) => {
                let mut outcome = Outcome::rejected();
                if error.code == "TERMINATION_UNCONFIRMED" {
                    outcome = unknown_outcome("TERMINATION_UNCONFIRMED");
                } else if error.code == "UNSUPPORTED_RESOURCE_LIMIT" {
                    outcome.error_code = Some("UNSUPPORTED_RESOURCE_LIMIT");
                }
                store.finish(&launch, &outcome, crate::now_us())?;
            }
        }
        return Ok(());
    }
    let identity = store.external_identity(&launch)?;
    let execution = launch.manifest.spec.execution.clone();
    let handle = if execution.adapter == "docker" {
        crate::external::docker_handle(&identity)
    } else {
        crate::external::ExternalHandle::Systemd {
            since_us: Some(crate::now_us()),
            unit: execution
                .systemd
                .as_ref()
                .ok_or_else(|| Error::new("CONFIG_INVALID", "Missing systemd specification"))?
                .unit
                .clone(),
        }
    };
    store.persist_external_intent(&launch.attempt_id, &handle)?;
    store.start_requested(&launch.attempt_id, crate::now_us())?;
    let env = match crate::process::resolve_environment(&execution) {
        Ok(env) => env,
        Err(_) => {
            store.finish(&launch, &Outcome::rejected(), crate::now_us())?;
            return Ok(());
        }
    };
    let secrets = Some(crate::process::secret_values(&execution, &env));
    let directory = store.directory.clone();
    let task = thread::spawn(move || {
        let policy = crate::external::load_policy(&directory)?;
        if let Some(docker) = execution.docker {
            crate::external::start_docker(
                &policy,
                &docker,
                &identity,
                crate::external::DockerContext {
                    command: &execution.command,
                    args: &execution.args,
                    env: &env,
                    working_directory: &execution.working_directory,
                },
                &directory,
            )
        } else if let Some(systemd) = execution.systemd {
            crate::external::start_systemd(&policy, &systemd)
        } else {
            Err(Error::new(
                "CONFIG_INVALID",
                "Missing external executor specification",
            ))
        }
    });
    pending.push(PendingStart {
        launch,
        task,
        secrets,
        disk_limit: store.log_disk_limit()?,
    });
    Ok(())
}
enum ExternalResult {
    Retired,
    Running(Option<bool>),
    Finished(Outcome, bool),
}
struct ExternalWatch {
    retired: Arc<AtomicBool>,
    launch: Launch,
    handle: crate::external::ExternalHandle,
    secrets: Option<Vec<Vec<u8>>>,
    task: Option<thread::JoinHandle<ExternalResult>>,
    next: Instant,
    started_at_us: i64,
    disk_limit: u64,
    stopping: bool,
}
impl ExternalWatch {
    fn new(
        launch: Launch,
        handle: crate::external::ExternalHandle,
        secrets: Option<Vec<Vec<u8>>>,
        started_at_us: i64,
        disk_limit: u64,
    ) -> Self {
        Self {
            retired: Arc::new(AtomicBool::new(false)),
            launch,
            handle,
            secrets,
            task: None,
            next: Instant::now(),
            started_at_us,
            disk_limit,
            stopping: false,
        }
    }
    fn poll(&mut self, cancel: bool, directory: &Path) -> Option<ExternalResult> {
        let timed_out = self.launch.manifest.kind != "Service"
            && crate::now_us().saturating_sub(self.started_at_us)
                >= i64::try_from(self.launch.manifest.spec.timeout_seconds)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(1_000_000);
        self.stopping |= cancel || timed_out;
        if self
            .task
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
        {
            let task = self.task.take()?;
            self.next = Instant::now() + Duration::from_secs(1);
            return Some(task.join().unwrap_or_else(|_| {
                ExternalResult::Finished(unknown_outcome("EXECUTOR_UNAVAILABLE"), false)
            }));
        }
        if self.task.is_some() || Instant::now() < self.next {
            return None;
        }
        let stopping = self.stopping;
        let retired = Arc::clone(&self.retired);
        let handle = self.handle.clone();
        let directory = directory.to_owned();
        let secrets = self.secrets.clone();
        let secret_names = self
            .launch
            .manifest
            .spec
            .execution
            .secret_refs
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let cap = self.launch.manifest.spec.logs.max_run_bytes.min(1_048_576);
        let disk_cap = self.disk_limit;
        let attempt = self.launch.attempt_id.clone();
        let grace = self.launch.manifest.spec.service.stop_grace_seconds;
        let stop_state = if timed_out {
            EndState::TimedOut
        } else {
            EndState::Cancelled
        };
        self.task = Some(thread::spawn(move || {
            let policy = match crate::external::load_policy(&directory) {
                Ok(policy) => policy,
                Err(_) => {
                    return ExternalResult::Finished(
                        unknown_outcome("EXECUTOR_UNAVAILABLE"),
                        false,
                    );
                }
            };
            if retired.load(Ordering::Relaxed) {
                return ExternalResult::Retired;
            }
            if stopping {
                match crate::external::stop_if_current(&policy, &handle, grace, || {
                    !retired.load(Ordering::Relaxed)
                }) {
                    Ok(true) => {}
                    _ => {
                        return ExternalResult::Finished(
                            unknown_outcome("TERMINATION_UNCONFIRMED"),
                            false,
                        );
                    }
                }
            }
            let observed = match crate::external::inspect(&policy, &handle) {
                Ok(value) => value,
                Err(_) => {
                    return ExternalResult::Finished(
                        unknown_outcome("EXECUTOR_UNAVAILABLE"),
                        false,
                    );
                }
            };
            if retired.load(Ordering::Relaxed) {
                return ExternalResult::Retired;
            }
            match observed.state {
                crate::external::ExternalState::Running => {
                    ExternalResult::Running(observed.healthy)
                }
                crate::external::ExternalState::Unknown => {
                    ExternalResult::Finished(unknown_outcome("EXECUTOR_UNAVAILABLE"), false)
                }
                crate::external::ExternalState::Absent => ExternalResult::Finished(
                    if stopping {
                        Outcome {
                            state: stop_state,
                            error_code: Some("CANCELLED_BY_OPERATOR"),
                            exit_code: None,
                            log_bytes: 0,
                            log_truncated: true,
                        }
                    } else {
                        unknown_outcome("EXTERNAL_IDENTITY_ABSENT")
                    },
                    false,
                ),
                crate::external::ExternalState::Exited => {
                    let secrets = secrets.or_else(|| {
                        crate::external::original_secret_values(&policy, &handle, &secret_names)
                            .ok()
                            .map(|values| {
                                let mut values = values
                                    .into_iter()
                                    .map(String::into_bytes)
                                    .collect::<Vec<_>>();
                                values.sort_by_key(|value| std::cmp::Reverse(value.len()));
                                values.dedup();
                                values
                            })
                    });
                    let context_missing = secrets.is_none();
                    if retired.load(Ordering::Relaxed) {
                        return ExternalResult::Retired;
                    }
                    let (bytes, truncated, logs_received) = if let Some(secrets) = secrets {
                        match crate::external::logs_if_current(
                            &policy,
                            &handle,
                            usize::try_from(cap).unwrap_or(1_048_576),
                            &secrets,
                            || !retired.load(Ordering::Relaxed),
                        ) {
                            Ok((bytes, truncated)) => (bytes, truncated, true),
                            Err(_) => (Vec::new(), true, false),
                        }
                    } else {
                        (Vec::new(), true, false)
                    };
                    if retired.load(Ordering::Relaxed) {
                        return ExternalResult::Retired;
                    }
                    let saved = (|| -> Result<u64> {
                        let logs = crate::store::private_directory(&directory.join("logs"))?;
                        let mut file =
                            crate::store::secure_file(&logs.join(format!("{attempt}.log")))?;
                        let bytes =
                            &bytes[..bytes.len().min(usize::try_from(cap).unwrap_or(1_048_576))];
                        let (written, _) =
                            crate::process::write_log_chunk(&mut file, &logs, disk_cap, bytes)
                                .map_err(|_| transport_error())?;
                        file.sync_all().map_err(|_| transport_error())?;
                        Ok(u64::try_from(written).unwrap_or(0))
                    })();
                    let log_failed = saved.is_err();
                    let state = if stopping {
                        stop_state
                    } else {
                        match observed.outcome {
                            Some(crate::external::TerminalOutcome::Succeeded) => {
                                EndState::Succeeded
                            }
                            Some(crate::external::TerminalOutcome::Failed) => EndState::Failed,
                            None => EndState::Unknown,
                        }
                    };
                    ExternalResult::Finished(
                        Outcome {
                            state,
                            error_code: match state {
                                EndState::Succeeded if context_missing => {
                                    Some("LOG_REDACTION_CONTEXT_UNAVAILABLE")
                                }
                                EndState::Succeeded => None,
                                EndState::Failed
                                    if observed.exit_code.is_some_and(|code| code != 0) =>
                                {
                                    Some("EXIT_NONZERO")
                                }
                                EndState::Failed => Some("EXTERNAL_EXECUTION_FAILED"),
                                EndState::TimedOut => Some("DEADLINE_EXCEEDED"),
                                EndState::Cancelled => Some("CANCELLED_BY_OPERATOR"),
                                EndState::Unknown => Some("RECONCILIATION_REQUIRED"),
                            },
                            exit_code: observed.exit_code,
                            log_bytes: saved.as_ref().copied().unwrap_or(0),
                            log_truncated: truncated
                                || log_failed
                                || saved.as_ref().is_ok_and(|written| {
                                    *written < u64::try_from(bytes.len()).unwrap_or(u64::MAX)
                                }),
                        },
                        !log_failed && logs_received,
                    )
                }
            }
        }));
        None
    }
}

fn handle(store: &mut Store, stream: &mut UnixStream, paused: bool) -> Result<Value> {
    let bytes = read_line(
        stream,
        u64::try_from(MAX_REQUEST_BYTES).unwrap_or(2 * 1024 * 1024),
        Duration::from_millis(200),
    )?;
    let request: Request = serde_json::from_slice(&bytes)
        .map_err(|_| Error::new("INPUT_INVALID", "Invalid local command"))?;
    match request {
        Request::Execute { command, token } => {
            crate::application::execute(store, *command, token.as_deref())
        }
        Request::Status => {
            serde_json::to_value(store.status(paused)?).map_err(|_| transport_error())
        }
        Request::Executions => {
            serde_json::to_value(store.history()?).map_err(|_| transport_error())
        }
        Request::Apply { manifest } => {
            Ok(json!({"revisionId":store.apply(&manifest,crate::now_us())?}))
        }
        Request::Logs { attempt_id } => {
            let path = store.log_path(&attempt_id)?;
            let file = File::open(path).map_err(|_| {
                Error::new("LOG_UNAVAILABLE", "Logs are not available for this attempt")
            })?;
            let mut bytes = Vec::new();
            file.take(65_537)
                .read_to_end(&mut bytes)
                .map_err(|_| Error::new("LOG_UNAVAILABLE", "Cannot read logs"))?;
            let truncated = bytes.len() > 65_536;
            bytes.truncate(65_536);
            Ok(
                json!({"attemptId":attempt_id,"text":String::from_utf8_lossy(&bytes),"responseTruncated":truncated,"untrusted":true}),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn accepts_a_full_log_response_after_json_escaping() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let listener = UnixListener::bind(dir.path().join("control.sock")).unwrap();
        let encoded =
            serde_json::to_vec(&json!({"data":{"text":"\0".repeat(65_536)},"error":null})).unwrap();
        let sender = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_line(&mut stream, 65_536, Duration::from_secs(5));
            let mut frame = encoded;
            frame.push(b'\n');
            let _ = write_frame(&mut stream, &frame, Duration::from_secs(5));
        });
        let decoded = client(
            dir.path(),
            Request::Logs {
                attempt_id: "example".to_owned(),
            },
        );
        sender.join().unwrap();
        assert_eq!(decoded.unwrap()["text"].as_str().unwrap().len(), 65_536);
    }

    #[test]
    fn response_payload_limit_accounts_for_the_newline_delimiter() {
        for extra in [0, 1] {
            let directory = tempfile::tempdir().unwrap();
            let listener = UnixListener::bind(directory.path().join("control.sock")).unwrap();
            let empty = serde_json::to_vec(&json!({"data":{"text":""},"error":null})).unwrap();
            let limit = usize::try_from(MAX_RESPONSE_BYTES).unwrap();
            let size = limit + extra - empty.len();
            let mut frame =
                serde_json::to_vec(&json!({"data":{"text":"x".repeat(size)},"error":null}))
                    .unwrap();
            assert_eq!(frame.len(), limit + extra);
            frame.push(b'\n');
            let sender = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = read_line(&mut stream, 65_536, Duration::from_secs(5));
                let _ = write_frame(&mut stream, &frame, Duration::from_secs(5));
            });
            let response = client(directory.path(), Request::Status);
            sender.join().unwrap();
            if extra == 0 {
                assert_eq!(response.unwrap()["text"].as_str().unwrap().len(), size);
            } else {
                assert_eq!(response.unwrap_err().code, "LOCAL_TRANSPORT_UNAVAILABLE");
            }
        }
    }

    #[test]
    fn a_drip_feed_cannot_extend_the_total_request_deadline() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        let sender = thread::spawn(move || {
            for _ in 0..100 {
                if writer.write_all(b" ").is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
            let _ = writer.write_all(b"\n");
        });
        let started = Instant::now();
        assert!(read_line(&mut reader, 65_536, Duration::from_millis(150)).is_err());
        assert!(started.elapsed() < Duration::from_millis(400));
        drop(reader);
        sender.join().unwrap();
    }
}
