use std::{
    collections::BTreeMap,
    fs::{self},
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
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
    run_with_settings(directory, manifests, paused, None, shutdown)
}

pub fn validate_alert_config(alerts: Option<&crate::config::AlertConfig>) -> Result<()> {
    if let Some(config) = alerts {
        #[cfg(feature = "webhooks")]
        return crate::webhooks::validate_config(config);
        #[cfg(not(feature = "webhooks"))]
        {
            let _ = config;
            return Err(Error::new(
                "FEATURE_UNAVAILABLE",
                "Outbound alerts require the webhooks feature",
            ));
        }
    }
    Ok(())
}

pub fn run_with_settings(
    directory: &Path,
    manifests: &[Manifest],
    paused: bool,
    alerts: Option<crate::config::AlertConfig>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    validate_alert_config(alerts.as_ref())?;
    let mut store = Store::open_with_alerts(directory, alerts.as_ref())?;
    #[cfg(feature = "webhooks")]
    let mut alert_worker = alerts
        .map(crate::webhooks::WebhookWorker::spawn)
        .transpose()?;
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
    // UNKNOWN keeps its capacity reservation and original deadline across reattachment.
    let mut external_deadlines = BTreeMap::new();
    let mut cleanup: Option<(String, thread::JoinHandle<bool>)> = None;
    let mut recovered_at = Instant::now() - Duration::from_secs(10);
    let mut retained_at = Instant::now();
    loop {
        let stopping = shutdown.load(Ordering::Relaxed);
        let now = crate::now_us();
        #[cfg(feature = "webhooks")]
        if let Some(worker) = &mut alert_worker {
            if stopping {
                worker.shutdown();
            }
            if let Some(result) = worker.try_result() {
                store.complete_alert(result, now)?;
            }
            if !stopping && !worker.available() {
                store.alert_worker_unavailable = true;
            }
            if !stopping
                && worker.idle()
                && let Some(job) = store.claim_alert(now)?
                && worker.try_send(job).is_err()
            {
                store.alert_worker_unavailable = true;
            }
        }
        if !stopping {
            store.scan_current_alerts(now)?;
        }
        // Observe every source and revoke uncertain captures before any stream may rotate or reclaim.
        let mut native_observations = Vec::with_capacity(native.len());
        for (launch, process) in &mut native {
            if stopping
                || store.cancel_requested(&launch.attempt_id)?
                || store.service_should_stop(launch)?
            {
                process.cancel();
            }
            let observed = process.poll_with_capture_budget(&mut store, &mut 0);
            if observed.is_err() {
                process.freeze_capture(&store)?;
            }
            native_observations.push(Some(observed));
        }
        let mut capture_budget = 8;
        let mut index = 0;
        while index < native.len() {
            let requested = store.cancel_requested(&native[index].0.attempt_id)?
                || store.service_should_stop(&native[index].0)?;
            let cancel = stopping || requested;
            if cancel {
                native[index].1.cancel();
            }
            let observed = native_observations
                .get_mut(index)
                .and_then(Option::take)
                .ok_or_else(|| Error::new("INTERNAL_ERROR", "Missing process observation"))?;
            let polled = match observed {
                Ok(None) => native[index]
                    .1
                    .poll_with_capture_budget(&mut store, &mut capture_budget),
                other => other,
            };
            let probe_events = native[index].1.take_probe_events();
            store.record_probe_events(&native[index].0, &probe_events)?;
            match polled {
                Ok(Some(mut outcome)) => {
                    if stopping && !requested && outcome.state == EndState::Cancelled {
                        outcome.error_code = Some("DAEMON_SHUTDOWN");
                    }
                    store.finish(&native[index].0, &outcome, now)?;
                    native.swap_remove(index);
                    native_observations.swap_remove(index);
                }
                Ok(None) => {
                    let state = native[index].1.service_observation();
                    store.service_observation(&native[index].0, state, now)?;
                    index += 1;
                }
                Err(_) => {
                    native[index].1.freeze_capture(&store)?;
                    let outcome = unknown_outcome("EXECUTOR_UNAVAILABLE");
                    store.finish(&native[index].0, &outcome, now)?;
                    native.swap_remove(index);
                    native_observations.swap_remove(index);
                }
            }
        }
        if native.len() > 1 {
            native.rotate_left(1);
        }
        let mut index = 0;
        while index < pending.len() {
            if pending[index].task.is_finished() {
                let item = pending.swap_remove(index);
                let (result, reference) = match item.task.join() {
                    Ok(value) => value,
                    Err(_) => (
                        Err(Error::new(
                            "EXECUTOR_UNAVAILABLE",
                            "External start receipt is unavailable",
                        )),
                        None,
                    ),
                };
                match result {
                    Ok(handle) => {
                        let encoded =
                            serde_json::to_string(&handle).map_err(|_| transport_error())?;
                        store.running_handle(&item.launch, &encoded, now)?;
                        let mut watch =
                            ExternalWatch::new(item.launch, handle, item.secrets, item.deadline);
                        watch.reference = Arc::new(Mutex::new(reference));
                        external.push(watch);
                    }
                    Err(error) => {
                        let outcome = if matches!(
                            error.code.as_str(),
                            "START_REJECTED" | "CONFIG_INVALID" | "NOT_AUTHORIZED"
                        ) {
                            Outcome::rejected()
                        } else {
                            unknown_outcome("EXECUTOR_UNAVAILABLE")
                        };
                        store.finish(&item.launch, &outcome, now)?;
                        if outcome.state != EndState::Unknown {
                            external_deadlines.remove(&item.launch.attempt_id);
                        } else if reference.is_some() {
                            let mut watch = ExternalWatch::new(
                                item.launch,
                                item.handle,
                                item.secrets,
                                item.deadline,
                            );
                            watch.reference = Arc::new(Mutex::new(reference));
                            external.push(watch);
                        }
                    }
                }
            } else {
                index += 1;
            }
        }
        let mut index = 0;
        while index < external.len() {
            if !store.attempt_is_observable(&external[index].launch.attempt_id)? {
                external_deadlines.remove(&external[index].launch.attempt_id);
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
                    external_deadlines.remove(&external[index].launch.attempt_id);
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
                Some(ExternalResult::Finished(mut outcome, snapshot)) => {
                    let logs_persisted = if let Some(bytes) = snapshot {
                        let launch = &external[index].launch;
                        let saved = store
                            .prepare_external_log_capture(launch)
                            .and_then(|()| store.append_log(launch, &bytes));
                        match saved {
                            Ok(saved) => {
                                outcome.log_bytes = u64::try_from(saved).unwrap_or(0);
                                outcome.log_truncated |= saved < bytes.len();
                                saved == bytes.len()
                            }
                            Err(error) => {
                                store.stop_log_capture(&launch.attempt_id, &error.code, 0)?;
                                outcome.log_truncated = true;
                                false
                            }
                        }
                    } else {
                        false
                    };
                    store.finish_external(
                        &external[index].launch,
                        &outcome,
                        logs_persisted,
                        &external[index].handle,
                        now,
                    )?;
                    if outcome.state != EndState::Unknown {
                        external_deadlines.remove(&external[index].launch.attempt_id);
                    }
                    if outcome.state == EndState::Unknown
                        && !stopping
                        && external[index]
                            .reference
                            .lock()
                            .is_ok_and(|held| held.is_some())
                    {
                        // Preserve oneshot metadata while its ownership remains uncertain.
                        external[index].next = Instant::now() + Duration::from_secs(10);
                        index += 1;
                    } else {
                        external.swap_remove(index);
                    }
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
            prune_external_deadlines(&store, &mut external_deadlines)?;
            for (launch, handle) in store.recoverable_external()? {
                if external
                    .iter()
                    .any(|watch| watch.launch.attempt_id == launch.attempt_id)
                {
                    continue;
                }
                let secrets = if matches!(
                    handle,
                    crate::external::ExternalHandle::Systemd { .. }
                        | crate::external::ExternalHandle::SystemdV2 { .. }
                ) && launch.manifest.spec.execution.secret_refs.is_empty()
                {
                    Some(Vec::new())
                } else {
                    None
                };
                let started = store.attempt_started_at(&launch.attempt_id)?;
                external.push(ExternalWatch::recover(
                    launch,
                    handle,
                    secrets,
                    started,
                    crate::now_us(),
                    Instant::now(),
                    &mut external_deadlines,
                ));
            }
        }
        if retained_at.elapsed() >= Duration::from_secs(1) {
            store.retain_logs()?;
            store.maintain_alerts(now)?;
            retained_at = Instant::now();
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
                dispatch(
                    &mut store,
                    launch,
                    &mut native,
                    &mut pending,
                    &mut external_deadlines,
                )?;
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
    task: thread::JoinHandle<(
        Result<crate::external::ExternalHandle>,
        Option<crate::external::SystemdReference>,
    )>,
    handle: crate::external::ExternalHandle,
    secrets: Option<Vec<Vec<u8>>>,
    deadline: Option<Instant>,
}
fn external_deadline(manifest: &Manifest, now: Instant, elapsed: Duration) -> Option<Instant> {
    (manifest.kind != "Service")
        .then(|| now + Duration::from_secs(manifest.spec.timeout_seconds).saturating_sub(elapsed))
}
fn prune_external_deadlines(
    store: &Store,
    deadlines: &mut BTreeMap<String, Option<Instant>>,
) -> Result<()> {
    for attempt in deadlines.keys().cloned().collect::<Vec<_>>() {
        if !store.attempt_is_observable(&attempt)? {
            deadlines.remove(&attempt);
        }
    }
    Ok(())
}
fn dispatch(
    store: &mut Store,
    launch: Launch,
    native: &mut Vec<(Launch, RunningProcess)>,
    pending: &mut Vec<PendingStart>,
    external_deadlines: &mut BTreeMap<String, Option<Instant>>,
) -> Result<()> {
    if launch.manifest.spec.execution.adapter == "process" {
        store.retain_logs()?;
        store.start_requested(&launch.attempt_id, crate::now_us())?;
        match RunningProcess::spawn(&launch, store) {
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
    prune_external_deadlines(store, external_deadlines)?;
    let identity = store.external_identity(&launch)?;
    let execution = launch.manifest.spec.execution.clone();
    let handle = if execution.adapter == "docker" {
        crate::external::docker_handle(&identity)
    } else {
        let spec = execution
            .systemd
            .as_ref()
            .ok_or_else(|| Error::new("CONFIG_INVALID", "Missing systemd specification"))?;
        let prepared = crate::external::load_policy(&store.directory).and_then(|policy| {
            crate::external::systemd_intent(&policy, spec, launch.manifest.kind == "Service")
        });
        match prepared {
            Ok(handle) => handle,
            Err(_) => {
                store.finish(&launch, &Outcome::rejected(), crate::now_us())?;
                return Ok(());
            }
        }
    };
    if let Err(error) = store.persist_external_intent(&launch.attempt_id, &handle) {
        if error.code == "START_REJECTED" {
            store.finish(&launch, &Outcome::rejected(), crate::now_us())?;
            return Ok(());
        }
        return Err(error);
    }
    let deadline = external_deadline(&launch.manifest, Instant::now(), Duration::ZERO);
    store.start_requested(&launch.attempt_id, crate::now_us())?;
    external_deadlines.insert(launch.attempt_id.clone(), deadline);
    let env = match crate::process::resolve_environment(&execution) {
        Ok(env) => env,
        Err(_) => {
            store.finish(&launch, &Outcome::rejected(), crate::now_us())?;
            external_deadlines.remove(&launch.attempt_id);
            return Ok(());
        }
    };
    let secrets = Some(crate::process::secret_values(&execution, &env));
    let directory = store.directory.clone();
    let prepared_handle = handle.clone();
    let task = thread::spawn(move || {
        let mut reference = None;
        let result = (|| {
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
            } else if execution.systemd.is_some() {
                reference = crate::external::systemd_reference(&policy, &prepared_handle)?;
                crate::external::start_systemd_intent(&policy, &prepared_handle)
            } else {
                Err(Error::new(
                    "CONFIG_INVALID",
                    "Missing external executor specification",
                ))
            }
        })();
        (result, reference)
    });
    pending.push(PendingStart {
        launch,
        handle,
        task,
        secrets,
        deadline,
    });
    Ok(())
}
enum ExternalResult {
    Retired,
    Running(Option<bool>),
    Finished(Outcome, Option<Vec<u8>>),
}
struct ExternalWatch {
    reference: Arc<Mutex<Option<crate::external::SystemdReference>>>,
    retired: Arc<AtomicBool>,
    launch: Launch,
    handle: crate::external::ExternalHandle,
    secrets: Option<Vec<Vec<u8>>>,
    task: Option<thread::JoinHandle<ExternalResult>>,
    next: Instant,
    deadline: Option<Instant>,
    timed_out: bool,
    stopping: bool,
}
impl ExternalWatch {
    fn recover(
        launch: Launch,
        handle: crate::external::ExternalHandle,
        secrets: Option<Vec<Vec<u8>>>,
        started_at_us: i64,
        now_us: i64,
        now: Instant,
        deadlines: &mut BTreeMap<String, Option<Instant>>,
    ) -> Self {
        let elapsed =
            Duration::from_micros(u64::try_from(now_us.saturating_sub(started_at_us)).unwrap_or(0));
        let deadline = *deadlines
            .entry(launch.attempt_id.clone())
            .or_insert_with(|| external_deadline(&launch.manifest, now, elapsed));
        Self::new(launch, handle, secrets, deadline)
    }
    fn new(
        launch: Launch,
        handle: crate::external::ExternalHandle,
        secrets: Option<Vec<Vec<u8>>>,
        deadline: Option<Instant>,
    ) -> Self {
        Self {
            reference: Arc::new(Mutex::new(None)),
            retired: Arc::new(AtomicBool::new(false)),
            launch,
            handle,
            secrets,
            task: None,
            next: Instant::now(),
            deadline,
            timed_out: false,
            stopping: false,
        }
    }
    fn poll(&mut self, cancel: bool, directory: &Path) -> Option<ExternalResult> {
        self.poll_at(cancel, directory, Instant::now())
    }
    fn poll_at(&mut self, cancel: bool, directory: &Path, now: Instant) -> Option<ExternalResult> {
        self.timed_out |= self.deadline.is_some_and(|deadline| now >= deadline);
        self.stopping |= cancel || self.timed_out;
        if self
            .task
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
        {
            let task = self.task.take()?;
            self.next = now + Duration::from_secs(1);
            return Some(task.join().unwrap_or_else(|_| {
                ExternalResult::Finished(unknown_outcome("EXECUTOR_UNAVAILABLE"), None)
            }));
        }
        if self.task.is_some() || now < self.next {
            return None;
        }
        let stopping = self.stopping;
        let reference = Arc::clone(&self.reference);
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
        let grace = self.launch.manifest.spec.service.stop_grace_seconds;
        let stop_state = if self.timed_out {
            EndState::TimedOut
        } else {
            EndState::Cancelled
        };
        self.task = Some(thread::spawn(move || {
            let policy = match crate::external::load_policy(&directory) {
                Ok(policy) => policy,
                Err(_) => {
                    return ExternalResult::Finished(unknown_outcome("EXECUTOR_UNAVAILABLE"), None);
                }
            };
            if retired.load(Ordering::Relaxed) {
                return ExternalResult::Retired;
            }
            let held = reference.lock().map(|mut held| -> Result<()> {
                if held.is_none() {
                    *held = crate::external::systemd_reference(&policy, &handle)?;
                }
                Ok(())
            });
            if !matches!(held, Ok(Ok(()))) {
                return ExternalResult::Finished(unknown_outcome("EXECUTOR_UNAVAILABLE"), None);
            }
            if stopping {
                match crate::external::stop_if_current(&policy, &handle, grace, || {
                    !retired.load(Ordering::Relaxed)
                }) {
                    Ok(true)
                        if matches!(
                            handle,
                            crate::external::ExternalHandle::SystemdV2 { service: true, .. }
                        ) =>
                    {
                        return ExternalResult::Finished(
                            Outcome {
                                state: stop_state,
                                error_code: Some("CANCELLED_BY_OPERATOR"),
                                exit_code: None,
                                log_bytes: 0,
                                log_truncated: true,
                            },
                            None,
                        );
                    }
                    Ok(true) => {}
                    _ => {
                        return ExternalResult::Finished(
                            unknown_outcome("TERMINATION_UNCONFIRMED"),
                            None,
                        );
                    }
                }
            }
            let observed = match crate::external::inspect(&policy, &handle) {
                Ok(value) => value,
                Err(_) => {
                    return ExternalResult::Finished(unknown_outcome("EXECUTOR_UNAVAILABLE"), None);
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
                    ExternalResult::Finished(unknown_outcome("EXECUTOR_UNAVAILABLE"), None)
                }
                crate::external::ExternalState::Absent => ExternalResult::Finished(
                    if stopping {
                        Outcome {
                            state: stop_state,
                            error_code: Some(if stop_state == EndState::TimedOut {
                                "DEADLINE_EXCEEDED"
                            } else {
                                "CANCELLED_BY_OPERATOR"
                            }),
                            exit_code: None,
                            log_bytes: 0,
                            log_truncated: true,
                        }
                    } else {
                        unknown_outcome("EXTERNAL_IDENTITY_ABSENT")
                    },
                    None,
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
                            log_bytes: 0,
                            log_truncated: truncated,
                        },
                        if logs_received { Some(bytes) } else { None },
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
            let log = store.read_latest_log(&attempt_id, 65_536)?;
            Ok(
                json!({"attemptId":attempt_id,"text":log.text,"responseTruncated":log.response_truncated,"logTruncated":log.log_truncated,"retainedFromOffset":log.stream.retained_from_offset,"historyEvicted":log.stream.history_evicted,"nextOffset":log.next_offset,"untrusted":true}),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    fn external_test_launch() -> Launch {
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.kind = "OneShotJob".into();
        manifest.spec.timeout_seconds = 5;
        Launch {
            attempt_id: "attempt".into(),
            execution_id: "execution".into(),
            launch_nonce: "nonce".into(),
            due_at_us: 0,
            manifest,
        }
    }
    fn external_test_handle() -> crate::external::ExternalHandle {
        crate::external::ExternalHandle::Systemd {
            unit: "worker.service".into(),
            since_us: Some(1),
        }
    }

    fn external_test_policy(directory: &Path) -> PathBuf {
        let helper = directory.join("systemctl");
        fs::write(&helper, r##"#!/bin/sh
case "$1" in --user|--system) shift;; esac
if [ "$1" = --no-block ]; then shift; fi
case "$1" in
start)
  : > "$0.starting"
  count=0
  while [ ! -f "$0.release" ] && [ "$count" -lt 200 ]; do sleep 0.01; count=$((count + 1)); done
  ;;
show)
  if [ -f "$0.block-show" ]; then
    : > "$0.observing"
    count=0
    while [ ! -f "$0.release" ] && [ "$count" -lt 200 ]; do sleep 0.01; count=$((count + 1)); done
    rm "$0.block-show"
  fi
  active=active; id=11111111111111111111111111111111
  if [ -f "$0.fresh-start" ] && [ ! -f "$0.starting" ]; then active=inactive; id=; fi
  if [ -f "$0.stopped" ]; then active=inactive; fi
  printf 'LoadState=loaded\nActiveState=%s\nResult=success\nExecMainCode=1\nExecMainStatus=0\nInvocationID=%s\nJob=\n' "$active" "$id"
  ;;
stop) : > "$0.stopped" ;;
--no-pager) printf 'safe evidence\n' ;;
*) exit 1 ;;
esac
"##).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let reference = directory.join("reference-python");
        fs::write(
            &reference,
            include_str!("../tests/fixtures/systemd-python.sh"),
        )
        .unwrap();
        fs::set_permissions(&reference, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = crate::external::ExecutorPolicy {
            systemd_reference_python: reference.to_string_lossy().into_owned(),
            systemd_enabled: true,
            systemctl_binary: helper.to_string_lossy().into_owned(),
            journalctl_binary: helper.to_string_lossy().into_owned(),
            legacy_systemd_scope: Some(crate::external::SystemdScope::System),
            units: vec!["worker.service".into()],
            ..crate::external::ExecutorPolicy::default()
        };
        let path = directory.join("executors.json");
        fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        helper
    }

    fn wait_for_external_test(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "external helper did not make progress"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn fresh_external_budget_includes_pending_start_and_reconciled_entries_are_retired() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let mut store = Store::open(directory.path()).unwrap();
        let helper = external_test_policy(directory.path());
        fs::write(helper.with_extension("fresh-start"), "").unwrap();
        let mut manifest = external_test_launch().manifest;
        manifest.spec.execution.adapter = "systemd".into();
        manifest.spec.execution.command.clear();
        manifest.spec.execution.args.clear();
        manifest.spec.execution.systemd = Some(crate::external::SystemdSpec {
            unit: "worker.service".into(),
        });
        store.apply(&manifest, crate::now_us()).unwrap();
        crate::application::execute(
            &mut store,
            crate::contracts::AppCommand::Run {
                id: "local/hello".into(),
                idempotency_key: "start-budget".into(),
            },
            None,
        )
        .unwrap();
        let launch = store.claim_due(crate::now_us()).unwrap().unwrap();
        let attempt = launch.attempt_id.clone();
        let execution = launch.execution_id.clone();
        let before = Instant::now();
        let mut deadlines = BTreeMap::new();
        let mut pending = Vec::new();
        dispatch(
            &mut store,
            launch,
            &mut Vec::new(),
            &mut pending,
            &mut deadlines,
        )
        .unwrap();
        let after = Instant::now();
        let item = pending.pop().unwrap();
        assert!(
            item.deadline
                .is_some_and(|deadline| deadline >= before + Duration::from_secs(5)
                    && deadline <= after + Duration::from_secs(5))
        );
        assert_eq!(deadlines[&attempt], item.deadline);
        wait_for_external_test(|| helper.with_extension("starting").exists());
        assert!(!item.task.is_finished());
        fs::write(helper.with_extension("release"), "").unwrap();
        let (handle, reference) = item.task.join().unwrap();
        let handle = handle.unwrap();
        drop(reference);
        let mut watch = ExternalWatch::new(item.launch, handle, item.secrets, item.deadline);
        watch.next = before + Duration::from_secs(20);
        assert!(
            watch
                .poll_at(false, directory.path(), after + Duration::from_secs(5))
                .is_none()
        );
        assert!(watch.timed_out);
        store
            .finish(
                &watch.launch,
                &unknown_outcome("EXECUTOR_UNAVAILABLE"),
                crate::now_us(),
            )
            .unwrap();
        prune_external_deadlines(&store, &mut deadlines).unwrap();
        assert!(
            deadlines.contains_key(&attempt),
            "UNKNOWN lost its reserved deadline"
        );
        drop(watch);
        crate::application::execute(
            &mut store,
            crate::contracts::AppCommand::Reconcile {
                execution_id: execution,
                state: "SUCCEEDED".into(),
                evidence: "verified external completion".into(),
                idempotency_key: "retire-budget".into(),
            },
            None,
        )
        .unwrap();
        prune_external_deadlines(&store, &mut deadlines).unwrap();
        assert!(
            deadlines.is_empty(),
            "reconciled UNKNOWN without a watch retained historical cache state"
        );
    }

    #[test]
    fn deadline_during_an_active_helper_stays_timed_out_through_async_stop() {
        let directory = tempfile::tempdir().unwrap();
        let helper = external_test_policy(directory.path());
        fs::write(helper.with_extension("block-show"), "").unwrap();
        let now = Instant::now();
        let mut watch = ExternalWatch::new(
            external_test_launch(),
            external_test_handle(),
            Some(Vec::new()),
            Some(now + Duration::from_secs(5)),
        );
        watch.next = now;
        assert!(watch.poll_at(false, directory.path(), now).is_none());
        wait_for_external_test(|| helper.with_extension("observing").exists());
        assert!(
            watch
                .poll_at(false, directory.path(), now + Duration::from_secs(5))
                .is_none()
        );
        assert!(watch.timed_out);
        fs::write(helper.with_extension("release"), "").unwrap();
        wait_for_external_test(|| {
            watch
                .task
                .as_ref()
                .is_some_and(thread::JoinHandle::is_finished)
        });
        assert!(matches!(
            watch.poll_at(false, directory.path(), now + Duration::from_secs(5)),
            Some(ExternalResult::Running(_))
        ));
        assert!(
            watch
                .poll_at(true, directory.path(), now + Duration::from_secs(6))
                .is_none()
        );
        wait_for_external_test(|| {
            watch
                .task
                .as_ref()
                .is_some_and(thread::JoinHandle::is_finished)
        });
        let Some(ExternalResult::Finished(outcome, _)) =
            watch.poll_at(true, directory.path(), now + Duration::from_secs(7))
        else {
            panic!("missing terminal external result")
        };
        assert_eq!(outcome.state, EndState::TimedOut);
        assert_eq!(outcome.error_code, Some("DEADLINE_EXCEEDED"));
        assert!(helper.with_extension("stopped").exists());
    }

    #[test]
    fn recovered_jobs_consume_elapsed_time_once_and_services_have_no_deadline() {
        let now = Instant::now();
        let directory = tempfile::tempdir().unwrap();
        for (started, wall_now, expected) in [
            (18_000_000, 20_000_000, 3),
            (18_000_000, 30_000_000, 0),
            (i64::MIN, i64::MAX, 0),
        ] {
            let watch = ExternalWatch::recover(
                external_test_launch(),
                external_test_handle(),
                Some(Vec::new()),
                started,
                wall_now,
                now,
                &mut BTreeMap::new(),
            );
            assert_eq!(watch.deadline, Some(now + Duration::from_secs(expected)));
        }
        let mut launch = external_test_launch();
        launch.manifest.kind = "Service".into();
        let mut watch = ExternalWatch::recover(
            launch,
            external_test_handle(),
            Some(Vec::new()),
            i64::MIN,
            i64::MAX,
            now,
            &mut BTreeMap::new(),
        );
        assert_eq!(watch.deadline, None);
        watch.next = now + Duration::from_secs(172_800);
        assert!(
            watch
                .poll_at(false, directory.path(), now + Duration::from_secs(86_400))
                .is_none()
        );
        assert!(!watch.stopping);
    }

    #[test]
    fn uncertain_reattachment_reuses_the_first_monotonic_deadline() {
        let now = Instant::now();
        let mut deadlines = BTreeMap::new();
        let first = ExternalWatch::recover(
            external_test_launch(),
            external_test_handle(),
            Some(Vec::new()),
            18_000_000,
            20_000_000,
            now,
            &mut deadlines,
        );
        let reattached = ExternalWatch::recover(
            external_test_launch(),
            external_test_handle(),
            Some(Vec::new()),
            18_000_000,
            -86_400_000_000,
            now + Duration::from_secs(2),
            &mut deadlines,
        );
        assert_eq!(first.deadline, Some(now + Duration::from_secs(3)));
        assert_eq!(
            reattached.deadline, first.deadline,
            "a backward UTC step reset the remaining runtime on reattachment"
        );
    }

    #[test]
    fn future_recovered_utc_start_cannot_extend_the_execution_budget() {
        let mut manifest = Manifest::parse(include_str!("../examples/hello.yaml")).unwrap();
        manifest.kind = "OneShotJob".into();
        manifest.spec.timeout_seconds = 1;
        let launch = Launch {
            attempt_id: "attempt".into(),
            execution_id: "execution".into(),
            launch_nonce: "nonce".into(),
            due_at_us: 0,
            manifest,
        };
        let handle = crate::external::ExternalHandle::Systemd {
            unit: "worker.service".into(),
            since_us: Some(1),
        };
        let now = Instant::now();
        let mut watch = ExternalWatch::recover(
            launch,
            handle,
            Some(Vec::new()),
            86_401_000_000,
            1_000_000,
            now,
            &mut BTreeMap::new(),
        );
        watch.next = now + Duration::from_secs(10);
        let directory = tempfile::tempdir().unwrap();
        assert!(
            watch
                .poll_at(false, directory.path(), now + Duration::from_millis(999))
                .is_none()
        );
        assert!(!watch.stopping);
        assert!(
            watch
                .poll_at(false, directory.path(), now + Duration::from_secs(1))
                .is_none()
        );
        assert!(
            watch.stopping,
            "future durable UTC time extended a one-second in-process budget"
        );
    }

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
