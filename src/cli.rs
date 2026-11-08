use clap::{Parser, Subcommand};
use cronharbor::{
    Error, Result,
    config::{AlertConfig, Manifest},
    contracts::{AppCommand, LogDto, LogStoragePolicyInput, Role},
    server::{self, Request},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Parser)]
#[command(version, about = "CronHarbor scheduler and service control plane")]
struct Cli {
    #[arg(long, global = true, default_value = ".cronharbor")]
    state_dir: PathBuf,
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Server {
        #[arg(short, long)]
        file: Vec<PathBuf>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        paused: bool,
        #[cfg(feature = "api")]
        #[arg(long)]
        listen: Option<std::net::SocketAddr>,
        #[cfg(feature = "api")]
        #[arg(long)]
        ui_dir: Option<PathBuf>,
        #[cfg(feature = "api")]
        #[arg(long)]
        allowed_origin: Vec<String>,
    },
    Status,
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Jobs {
        #[command(subcommand)]
        command: JobCommand,
    },
    Services {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    Executions {
        #[command(subcommand)]
        command: Option<ExecutionCommand>,
    },
    Operations {
        #[command(subcommand)]
        command: OperationCommand,
    },
    Logs {
        attempt_id: String,
        #[arg(long)]
        follow: bool,
        #[arg(long, default_value_t = 65_536)]
        max_bytes: u32,
        #[arg(long, conflicts_with = "follow")]
        info: bool,
    },
    LogStorage {
        #[command(subcommand)]
        command: LogStorageCommand,
    },
    Alerts {
        #[command(subcommand)]
        command: AlertCommand,
    },
    Tokens {
        #[command(subcommand)]
        command: TokenCommand,
    },
    Audit {
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Events {
        #[arg(long, default_value_t = 0)]
        after: i64,
    },
    Backup {
        name: String,
    },
    Restore {
        backup: PathBuf,
        #[arg(long)]
        cutoff_us: Option<i64>,
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        replace: bool,
    },
    Resume {
        #[arg(long)]
        cutoff_us: Option<i64>,
        #[arg(long)]
        evidence: String,
    },
    Pause {
        #[arg(long, default_value = "operator pause")]
        evidence: String,
    },
    Import {
        #[command(subcommand)]
        command: ImportCommand,
    },
    ExportTypes {
        #[arg(long, default_value = "web/dashboard/src/generated/contracts.ts")]
        output: PathBuf,
    },
    ExportSchema {
        #[arg(long, default_value = "schema/workload.schema.json")]
        output: PathBuf,
    },
    #[cfg(feature = "mcp")]
    Mcp,
    #[cfg(feature = "api")]
    Api {
        #[arg(long, default_value = "127.0.0.1:8090")]
        listen: std::net::SocketAddr,
        #[arg(long)]
        ui_dir: Option<PathBuf>,
        #[arg(long)]
        allowed_origin: Vec<String>,
    },
}
#[derive(Subcommand)]
enum ConfigCommand {
    Validate {
        #[arg(short, long)]
        file: PathBuf,
    },
    Plan {
        #[arg(short, long)]
        file: PathBuf,
    },
    Apply {
        #[arg(short, long)]
        file: PathBuf,
        #[arg(long)]
        expected_revision: Option<String>,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
}
#[derive(Subcommand)]
enum LogStorageCommand {
    Status,
    Set {
        #[arg(long)]
        max_disk_bytes: u64,
        #[arg(long)]
        max_segments: u32,
        #[arg(long)]
        retention_seconds: u64,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
}
#[derive(Subcommand)]
enum AlertCommand {
    Status,
    List {
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        cursor: Option<String>,
    },
    Retry {
        id: String,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
}
#[derive(Subcommand)]
enum JobCommand {
    List {
        #[arg(long)]
        namespace: Option<String>,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Run {
        id: String,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    Preview {
        id: String,
        #[arg(long, default_value_t = 10)]
        count: u32,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
}
#[derive(Subcommand)]
enum ServiceCommand {
    List {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Start {
        id: String,
    },
    Stop {
        id: String,
    },
    Restart {
        id: String,
    },
}
#[derive(Subcommand)]
enum ExecutionCommand {
    List {
        #[arg(long)]
        workload_id: Option<String>,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Inspect {
        id: String,
    },
    Cancel {
        id: String,
    },
    Reconcile {
        id: String,
        #[arg(long)]
        state: String,
        #[arg(long)]
        evidence: String,
    },
    Diagnostics {
        id: String,
    },
}
#[derive(Subcommand)]
enum OperationCommand {
    Inspect { id: String },
}
#[derive(Subcommand)]
enum TokenCommand {
    Create {
        #[arg(long, default_value = "observe")]
        role: String,
        #[arg(long)]
        namespace: Vec<String>,
        #[arg(long)]
        capability: Vec<String>,
        #[arg(long, default_value_t = 86400)]
        ttl_seconds: u64,
        #[arg(long)]
        output_file: Option<PathBuf>,
    },
    List {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Revoke {
        id: String,
    },
}
#[derive(Subcommand)]
enum ImportCommand {
    Cronicle {
        file: PathBuf,
        #[arg(long, default_value = "imported")]
        namespace: String,
        #[arg(long, default_value = "json")]
        report: String,
        #[arg(long)]
        dry_run: bool,
    },
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct DaemonConfig {
    workloads: Vec<PathBuf>,
    paused: bool,
    alerts: Option<AlertConfig>,
    #[cfg(feature = "api")]
    listen: Option<std::net::SocketAddr>,
    #[cfg(feature = "api")]
    ui_directory: Option<PathBuf>,
    #[cfg(feature = "api")]
    allowed_origins: Vec<String>,
}
fn key(value: Option<String>) -> String {
    value.unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}
fn fail(code: &str, message: &str) -> Error {
    Error::new(code, message)
}
fn token(path: Option<&Path>) -> Result<Option<String>> {
    let Some(path) = path else { return Ok(None) };
    let m = fs::symlink_metadata(path)
        .map_err(|_| fail("TOKEN_UNAVAILABLE", "Cannot read token file"))?;
    if !m.is_file()
        || m.uid() != nix::unistd::geteuid().as_raw()
        || m.mode() & 0o077 != 0
        || m.len() > 4096
    {
        return Err(fail(
            "TOKEN_UNAVAILABLE",
            "Token must be an owned private regular file",
        ));
    }
    let text = fs::read_to_string(path)
        .map_err(|_| fail("TOKEN_UNAVAILABLE", "Cannot read token file"))?
        .trim()
        .to_owned();
    if text.is_empty() {
        return Err(fail("TOKEN_UNAVAILABLE", "Token file is empty"));
    }
    Ok(Some(text))
}
fn definitions(path: &Path) -> Result<Vec<Manifest>> {
    let mut paths = if path.is_dir() {
        fs::read_dir(path)
            .map_err(|_| fail("CONFIG_INVALID", "Cannot read manifest directory"))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|_| fail("CONFIG_INVALID", "Cannot enumerate manifests"))?
            .into_iter()
            .filter(|p| {
                p.is_file()
                    && matches!(
                        p.extension().and_then(|s| s.to_str()),
                        Some("yaml" | "yml" | "json")
                    )
            })
            .collect::<Vec<_>>()
    } else {
        vec![path.to_owned()]
    };
    paths.sort();
    if paths.is_empty() || paths.len() > 1000 {
        return Err(fail("CONFIG_INVALID", "Expected 1 to 1000 manifests"));
    }
    paths.iter().map(|path| Manifest::read(path)).collect()
}
pub fn run() -> Result<Value> {
    let cli = Cli::parse();
    let directory = cli.state_dir;
    let token_path = cli.token_file.clone();
    let credential = token(token_path.as_deref())?;
    let call = |command| {
        server::client(
            &directory,
            Request::Execute {
                command: Box::new(command),
                token: credential.clone(),
            },
        )
    };
    match cli.command {
        Command::Server {
            file,
            config,
            paused,
            #[cfg(feature = "api")]
            listen,
            #[cfg(feature = "api")]
            ui_dir,
            #[cfg(feature = "api")]
            allowed_origin,
        } => {
            let mut settings = if let Some(path) = config {
                let file = fs::File::open(path)
                    .map_err(|_| fail("CONFIG_INVALID", "Cannot read daemon configuration"))?;
                let mut bytes = Vec::new();
                file.take((cronharbor::config::MAX_CONFIG_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(|_| fail("CONFIG_INVALID", "Cannot read daemon configuration"))?;
                cronharbor::config::parse_yaml::<DaemonConfig>(&bytes)?
            } else {
                DaemonConfig::default()
            };
            server::validate_alert_config(settings.alerts.as_ref())?;
            let alerts = settings.alerts.take();
            settings.workloads.extend(file);
            let mut manifests = Vec::new();
            for path in &settings.workloads {
                manifests.extend(definitions(path)?);
            }
            let paused = paused || settings.paused;
            let shutdown = Arc::new(AtomicBool::new(false));
            #[cfg(feature = "api")]
            if let Some(listen) = listen.or(settings.listen) {
                let ui = ui_dir.or(settings.ui_directory).or_else(|| {
                    [
                        PathBuf::from("web/dashboard/dist"),
                        PathBuf::from("/usr/share/cronharbor/ui"),
                    ]
                    .into_iter()
                    .find(|p| p.join("index.html").is_file())
                });
                let mut origins = settings.allowed_origins;
                origins.extend(allowed_origin);
                let flag = Arc::clone(&shutdown);
                let core_dir = directory.clone();
                let core = thread::spawn(move || {
                    let result = server::run_with_settings(
                        &core_dir,
                        &manifests,
                        paused,
                        alerts,
                        Arc::clone(&flag),
                    );
                    flag.store(true, Ordering::Relaxed);
                    result
                });
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .map_err(|_| fail("RUNTIME_UNAVAILABLE", "Cannot start runtime"))?;
                let result = runtime.block_on(cronharbor::api::serve(
                    directory,
                    listen,
                    ui,
                    origins,
                    Arc::clone(&shutdown),
                ));
                shutdown.store(true, Ordering::Relaxed);
                let core_result = core
                    .join()
                    .map_err(|_| fail("RUNTIME_UNAVAILABLE", "Scheduler failed"))?;
                result?;
                core_result?;
                return Ok(json!({"stopped":true}));
            }
            server::run_with_settings(&directory, &manifests, paused, alerts, shutdown)?;
            Ok(json!({"stopped":true}))
        }
        Command::Status => call(AppCommand::Status),
        Command::Alerts { command } => match command {
            AlertCommand::Status => call(AppCommand::AlertStatus),
            AlertCommand::List { cursor, limit } => {
                call(AppCommand::AlertDeliveries { cursor, limit })
            }
            AlertCommand::Retry {
                id,
                idempotency_key,
            } => call(AppCommand::RetryAlert {
                id,
                idempotency_key: key(idempotency_key),
            }),
        },
        Command::LogStorage { command } => match command {
            LogStorageCommand::Status => call(AppCommand::LogStorage),
            LogStorageCommand::Set {
                max_disk_bytes,
                max_segments,
                retention_seconds,
                expected_revision,
                idempotency_key,
            } => call(AppCommand::SetLogStorage {
                policy: LogStoragePolicyInput {
                    max_disk_bytes,
                    max_segments,
                    retention_seconds,
                },
                expected_revision,
                idempotency_key: key(idempotency_key),
            }),
        },
        Command::Config { command } => match command {
            ConfigCommand::Validate { file } => {
                let items=definitions(&file)?.iter().map(|m|m.canonical().map(|(_,hash)|json!({"name":m.metadata.name,"valid":true,"revisionHash":hash}))).collect::<Result<Vec<_>>>()?;
                Ok(json!({"valid":true,"items":items}))
            }
            ConfigCommand::Plan { file } => {
                let mut items = Vec::new();
                for manifest in definitions(&file)? {
                    items.push(call(AppCommand::Plan {
                        definition: manifest.canonical()?.0,
                    })?);
                }
                Ok(json!({"items":items}))
            }
            ConfigCommand::Apply {
                file,
                expected_revision,
                idempotency_key,
            } => {
                let manifests = definitions(&file)?;
                let mut definitions = Vec::new();
                for manifest in &manifests {
                    definitions.push(manifest.canonical()?.0);
                }
                if manifests.len() == 1 {
                    call(AppCommand::Apply {
                        definition: definitions.remove(0),
                        expected_revision,
                        source_kind: "FILE".into(),
                        idempotency_key: key(idempotency_key),
                    })
                } else {
                    if expected_revision.is_some() {
                        return Err(fail(
                            "INPUT_INVALID",
                            "A single revision cannot apply to multiple workloads",
                        ));
                    }
                    call(AppCommand::ApplyBatch {
                        expected_revisions: vec![None; definitions.len()],
                        definitions,
                        source_kind: "FILE".into(),
                        idempotency_key: key(idempotency_key),
                    })
                }
            }
        },
        Command::Jobs { command } => match command {
            JobCommand::List {
                namespace,
                cursor,
                limit,
            } => call(AppCommand::Workloads {
                namespace,
                kind: None,
                cursor,
                limit,
            }),
            JobCommand::Run {
                id,
                idempotency_key,
            } => call(AppCommand::Run {
                id,
                idempotency_key: key(idempotency_key),
            }),
            JobCommand::Preview { id, count } => call(AppCommand::Preview { id, count }),
            JobCommand::Pause { id } => call(AppCommand::Pause {
                id,
                paused: true,
                idempotency_key: key(None),
            }),
            JobCommand::Resume { id } => call(AppCommand::Pause {
                id,
                paused: false,
                idempotency_key: key(None),
            }),
        },
        Command::Services { command } => match command {
            ServiceCommand::List { cursor, limit } => call(AppCommand::Workloads {
                namespace: None,
                kind: Some("managed_service".into()),
                cursor,
                limit,
            }),
            other => {
                let (id, state) = match other {
                    ServiceCommand::Start { id } => (id, Some("RUNNING")),
                    ServiceCommand::Stop { id } => (id, Some("STOPPED")),
                    ServiceCommand::Restart { id } => (id, None),
                    ServiceCommand::List { .. } => {
                        return Err(fail("INPUT_INVALID", "Invalid service command"));
                    }
                };
                let workload = call(AppCommand::Workload { id: id.clone() })?;
                let revision = workload["revision_id"]
                    .as_str()
                    .ok_or_else(|| fail("STATE_CONFLICT", "No current revision"))?
                    .to_owned();
                if let Some(state) = state {
                    call(AppCommand::ServiceDesired {
                        id,
                        desired_state: state.into(),
                        expected_revision: Some(revision),
                        idempotency_key: key(None),
                    })
                } else {
                    call(AppCommand::ServiceRestart {
                        id,
                        expected_revision: Some(revision),
                        idempotency_key: key(None),
                    })
                }
            }
        },
        Command::Executions { command } => match command {
            Some(ExecutionCommand::List {
                workload_id,
                cursor,
                limit,
            }) => call(AppCommand::Executions {
                workload_id,
                cursor,
                limit,
            }),
            None => {
                let page = call(AppCommand::Executions {
                    workload_id: None,
                    cursor: None,
                    limit: 100,
                })?;
                Ok(page.get("items").cloned().unwrap_or(page))
            }
            Some(ExecutionCommand::Inspect { id }) => call(AppCommand::Execution { id }),
            Some(ExecutionCommand::Cancel { id }) => call(AppCommand::Cancel {
                execution_id: id,
                idempotency_key: key(None),
            }),
            Some(ExecutionCommand::Reconcile {
                id,
                state,
                evidence,
            }) => call(AppCommand::Reconcile {
                execution_id: id,
                state,
                evidence,
                idempotency_key: key(None),
            }),
            Some(ExecutionCommand::Diagnostics { id }) => {
                call(AppCommand::Diagnostics { execution_id: id })
            }
        },
        Command::Operations {
            command: OperationCommand::Inspect { id },
        } => call(AppCommand::Operation { id }),
        Command::Logs {
            attempt_id,
            follow,
            max_bytes,
            info,
        } => {
            if info {
                return call(AppCommand::LogInfo { attempt_id });
            }
            let stop = Arc::new(AtomicBool::new(false));
            if follow {
                signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stop))
                    .map_err(|_| fail("SIGNAL_UNAVAILABLE", "Cannot follow logs"))?;
            }
            let mut cursor = None;
            let mut recovering = false;
            loop {
                let command = if let Some(offset) = cursor {
                    AppCommand::Logs {
                        attempt_id: attempt_id.clone(),
                        offset,
                        max_bytes,
                    }
                } else {
                    AppCommand::LatestLogs {
                        attempt_id: attempt_id.clone(),
                        max_bytes,
                    }
                };
                let response = match call(command) {
                    Ok(response) => response,
                    Err(error) if follow && error.code == "LOG_CURSOR_EXPIRED" => {
                        cursor = None;
                        recovering = true;
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let chunk: LogDto = serde_json::from_value(response.clone())
                    .map_err(|_| fail("LOG_UNAVAILABLE", "Log response is invalid"))?;
                if chunk.attempt_id != attempt_id || chunk.stream.attempt_id != attempt_id {
                    return Err(fail(
                        "LOG_UNAVAILABLE",
                        "Log response selected a different attempt",
                    ));
                }
                if cursor.is_none() && chunk.stream.retained_from_offset > 0 {
                    let start = chunk.stream.retained_from_offset;
                    if recovering {
                        eprintln!(
                            "Log history expired during follow; resuming at retained byte {start}."
                        );
                    } else {
                        eprintln!(
                            "Earlier log history expired; starting at retained byte {start}."
                        );
                    }
                }
                recovering = false;
                if !follow {
                    return Ok(response);
                }
                if !chunk.text.is_empty() {
                    println!("{response}");
                }
                cursor = Some(chunk.next_offset);
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(500));
            }
            Ok(json!({"stopped":true}))
        }
        Command::Tokens { command } => match command {
            TokenCommand::List { cursor, limit } => call(AppCommand::Tokens { cursor, limit }),
            TokenCommand::Revoke { id } => call(AppCommand::RevokeToken { id }),
            TokenCommand::Create {
                role,
                namespace,
                capability,
                ttl_seconds,
                output_file,
            } => {
                let role: Role = serde_json::from_value(json!(role))
                    .map_err(|_| fail("INPUT_INVALID", "Unknown role"))?;
                let mut response = call(AppCommand::CreateToken {
                    role,
                    namespaces: namespace,
                    capabilities: capability,
                    ttl_seconds,
                })?;
                if let Some(path) = output_file {
                    let secret = response["token"]
                        .as_str()
                        .ok_or_else(|| fail("TOKEN_UNAVAILABLE", "No token returned"))?;
                    let mut file = fs::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .mode(0o600)
                        .open(path)
                        .map_err(|_| {
                            fail(
                                "TOKEN_UNAVAILABLE",
                                "Token output must be a new private file",
                            )
                        })?;
                    file.write_all(secret.as_bytes())
                        .and_then(|()| file.sync_all())
                        .map_err(|_| fail("TOKEN_UNAVAILABLE", "Cannot save token"))?;
                    if let Some(map) = response.as_object_mut() {
                        map.remove("token");
                    }
                }
                Ok(response)
            }
        },
        Command::Audit { limit } => call(AppCommand::Audit {
            cursor: None,
            limit,
        }),
        Command::Events { after } => call(AppCommand::Events { after, limit: 100 }),
        Command::Backup { name } => call(AppCommand::Backup { name }),
        Command::Restore {
            backup,
            cutoff_us,
            evidence,
            replace,
        } => serde_json::to_value(cronharbor::ops::restore(
            &directory, &backup, cutoff_us, &evidence, replace,
        )?)
        .map_err(|_| fail("INTERNAL_ERROR", "Cannot encode restore")),
        Command::Resume {
            cutoff_us,
            evidence,
        } => {
            if let Some(cutoff_us) = cutoff_us {
                call(AppCommand::RestoreResume {
                    cutoff_us,
                    evidence,
                })
            } else {
                call(AppCommand::GlobalPause {
                    paused: false,
                    evidence,
                })
            }
        }
        Command::Pause { evidence } => call(AppCommand::GlobalPause {
            paused: true,
            evidence,
        }),
        Command::Import {
            command:
                ImportCommand::Cronicle {
                    file,
                    namespace,
                    report,
                    dry_run: _,
                },
        } => {
            let mut text = String::new();
            fs::File::open(file)
                .map_err(|_| fail("IMPORT_INVALID", "Cannot read export"))?
                .take(4_194_305)
                .read_to_string(&mut text)
                .map_err(|_| fail("IMPORT_INVALID", "Export is not UTF-8"))?;
            let result = cronharbor::importer::inspect(&text, &namespace, cronharbor::now_us())?;
            if report == "markdown" {
                println!("{}", cronharbor::importer::markdown(&result));
                Ok(Value::Null)
            } else if report == "json" {
                serde_json::to_value(result)
                    .map_err(|_| fail("IMPORT_INVALID", "Cannot encode report"))
            } else {
                Err(fail("INPUT_INVALID", "Report must be json or markdown"))
            }
        }
        Command::ExportTypes { output } => {
            cronharbor::contracts::export_types(&output)?;
            Ok(json!({"output":output}))
        }
        Command::ExportSchema { output } => {
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)
                    .map_err(|_| fail("EXPORT_FAILED", "Cannot create schema directory"))?;
            }
            fs::write(
                &output,
                serde_json::to_vec_pretty(&schemars::schema_for!(Manifest))
                    .map_err(|_| fail("EXPORT_FAILED", "Cannot generate schema"))?,
            )
            .map_err(|_| fail("EXPORT_FAILED", "Cannot save schema"))?;
            Ok(json!({"output":output}))
        }
        #[cfg(feature = "mcp")]
        Command::Mcp => {
            let token_file = token_path.ok_or_else(|| {
                fail(
                    "TOKEN_UNAVAILABLE",
                    "MCP requires --token-file with an observe credential",
                )
            })?;
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| fail("RUNTIME_UNAVAILABLE", "Cannot start runtime"))?
                .block_on(cronharbor::mcp::serve(directory, token_file))?;
            Ok(Value::Null)
        }
        #[cfg(feature = "api")]
        Command::Api {
            listen,
            ui_dir,
            allowed_origin,
        } => {
            let shutdown = Arc::new(AtomicBool::new(false));
            for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
                signal_hook::flag::register(signal, Arc::clone(&shutdown))
                    .map_err(|_| fail("SIGNAL_UNAVAILABLE", "Cannot register shutdown"))?;
            }
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| fail("RUNTIME_UNAVAILABLE", "Cannot start runtime"))?
                .block_on(cronharbor::api::serve(
                    directory,
                    listen,
                    ui_dir,
                    allowed_origin,
                    shutdown,
                ))?;
            Ok(json!({"stopped":true}))
        }
    }
}
