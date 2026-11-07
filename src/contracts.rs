//! Public response types. TypeScript declarations are generated from these types.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer,
    Operator,
    Editor,
    Admin,
    Observe,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct PrincipalDto {
    pub actor_id: String,
    pub role: Role,
    pub namespaces: Vec<String>,
    pub capabilities: Vec<String>,
}
pub type Principal = PrincipalDto;

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct AdapterHealthDto {
    pub adapter: String,
    pub enabled: bool,
    pub state: String,
    #[ts(type = "number | null")]
    pub last_observed_at_us: Option<i64>,
    pub detail: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct ProbeEventDto {
    pub kind: String,
    pub healthy: bool,
    #[ts(type = "number")]
    pub observed_at_us: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct PreviewInstantDto {
    #[ts(type = "number")]
    pub utc_us: i64,
    pub local: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct PreviewDto {
    pub items: Vec<PreviewInstantDto>,
    pub timezone: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct ValidationDto {
    pub valid: bool,
    pub revision_hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct AuditDto {
    #[ts(type = "number")]
    pub seq: i64,
    pub actor_id: String,
    pub action: String,
    pub target_id: Option<String>,
    pub result: String,
    pub request_id: String,
    #[ts(type = "number")]
    pub created_at_us: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct WorkloadDto {
    pub id: String,
    pub namespace: String,
    pub name: String,
    pub kind: String,
    pub source_kind: String,
    pub revision_id: String,
    pub enabled: bool,
    pub paused: bool,
    pub executor: String,
    pub schedule_description: Option<String>,
    #[ts(type = "number | null")]
    pub next_due_at_us: Option<i64>,
    pub desired_state: Option<String>,
    pub observed_state: Option<String>,
    pub restart_count: u32,
    pub definition: String,
    pub probe_history: Vec<ProbeEventDto>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct StatusDto {
    pub sqlite_version: String,
    pub workloads: u32,
    pub active: u32,
    pub unknown: u32,
    pub paused: bool,
    pub restore_required: bool,
    pub adapter_health: Vec<AdapterHealthDto>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct ExecutionDto {
    pub execution_id: String,
    pub attempt_id: String,
    pub workload_id: String,
    pub namespace: String,
    pub name: String,
    pub state: String,
    #[ts(type = "number | null")]
    pub due_at_us: Option<i64>,
    #[ts(type = "number | null")]
    pub started_at_us: Option<i64>,
    #[ts(type = "number | null")]
    pub finished_at_us: Option<i64>,
    pub error_code: Option<String>,
    pub exit_code: Option<i32>,
    pub log_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct OperationDto {
    pub id: String,
    pub state: String,
    pub target_id: String,
    pub kind: String,
    #[ts(type = "number")]
    pub created_at_us: i64,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct LogDto {
    pub attempt_id: String,
    pub text: String,
    pub response_truncated: bool,
    pub log_truncated: bool,
    #[ts(type = "number")]
    pub next_offset: u64,
    pub untrusted: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct EventDto {
    #[ts(type = "number")]
    pub seq: i64,
    pub event_kind: String,
    pub workload_id: Option<String>,
    pub execution_id: Option<String>,
    pub payload_json: String,
    #[ts(type = "number")]
    pub created_at_us: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct DiagnosticDto {
    pub execution_id: String,
    pub state: String,
    pub observed_causes: Vec<String>,
    pub hypotheses: Vec<String>,
    pub evidence: Vec<EventDto>,
    pub safe_to_retry_automatically: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct PlanDto {
    pub current_revision: Option<String>,
    pub proposed_hash: String,
    pub changed: bool,
    pub source_kind: String,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct TokenDto {
    pub id: String,
    pub actor_id: String,
    pub role: Role,
    pub namespaces: Vec<String>,
    pub capabilities: Vec<String>,
    #[ts(type = "number | null")]
    pub expires_at_us: Option<i64>,
    pub revoked: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct CreatedTokenDto {
    pub token: String,
    pub metadata: TokenDto,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppCommand {
    Identity,
    RecentFailures {
        window_seconds: u64,
        limit: u32,
    },
    ServiceHealth {
        id: String,
    },
    Status,
    Workloads {
        namespace: Option<String>,
        cursor: Option<String>,
        limit: u32,
    },
    Workload {
        id: String,
    },
    Executions {
        workload_id: Option<String>,
        cursor: Option<String>,
        limit: u32,
    },
    Execution {
        id: String,
    },
    Logs {
        attempt_id: String,
        offset: u64,
        max_bytes: u32,
    },
    Preview {
        id: String,
        count: u32,
    },
    Validate {
        definition: String,
    },
    Plan {
        definition: String,
    },
    Apply {
        definition: String,
        expected_revision: Option<String>,
        source_kind: String,
        idempotency_key: String,
    },
    ApplyBatch {
        definitions: Vec<String>,
        expected_revisions: Vec<Option<String>>,
        source_kind: String,
        idempotency_key: String,
    },
    Run {
        id: String,
        idempotency_key: String,
    },
    Pause {
        id: String,
        paused: bool,
        idempotency_key: String,
    },
    ServiceDesired {
        id: String,
        desired_state: String,
        expected_revision: Option<String>,
        idempotency_key: String,
    },
    ServiceRestart {
        id: String,
        expected_revision: Option<String>,
        idempotency_key: String,
    },
    Cancel {
        execution_id: String,
        idempotency_key: String,
    },
    Reconcile {
        execution_id: String,
        state: String,
        evidence: String,
        idempotency_key: String,
    },
    Operation {
        id: String,
    },
    Events {
        after: i64,
        limit: u32,
    },
    Diagnostics {
        execution_id: String,
    },
    CreateToken {
        role: Role,
        namespaces: Vec<String>,
        capabilities: Vec<String>,
        ttl_seconds: u64,
    },
    Tokens {
        cursor: Option<String>,
        limit: u32,
    },
    RevokeToken {
        id: String,
    },
    Audit {
        cursor: Option<String>,
        limit: u32,
    },
    Backup {
        name: String,
    },
    GlobalPause {
        paused: bool,
        evidence: String,
    },
    RestoreResume {
        cutoff_us: i64,
        evidence: String,
    },
}

pub fn export_types(path: &std::path::Path) -> crate::Result<()> {
    let declarations = [
        Role::decl(&ts_rs::Config::default()),
        PrincipalDto::decl(&ts_rs::Config::default()),
        AdapterHealthDto::decl(&ts_rs::Config::default()),
        ProbeEventDto::decl(&ts_rs::Config::default()),
        PreviewInstantDto::decl(&ts_rs::Config::default()),
        PreviewDto::decl(&ts_rs::Config::default()),
        ValidationDto::decl(&ts_rs::Config::default()),
        AuditDto::decl(&ts_rs::Config::default()),
        WorkloadDto::decl(&ts_rs::Config::default()),
        StatusDto::decl(&ts_rs::Config::default()),
        ExecutionDto::decl(&ts_rs::Config::default()),
        OperationDto::decl(&ts_rs::Config::default()),
        LogDto::decl(&ts_rs::Config::default()),
        EventDto::decl(&ts_rs::Config::default()),
        DiagnosticDto::decl(&ts_rs::Config::default()),
        PlanDto::decl(&ts_rs::Config::default()),
        TokenDto::decl(&ts_rs::Config::default()),
        CreatedTokenDto::decl(&ts_rs::Config::default()),
        Page::<WorkloadDto>::decl(&ts_rs::Config::default()),
    ];
    let text = format!(
        "// Generated from src/contracts.rs. Do not edit.\n{}\n",
        declarations
            .iter()
            .map(|item| format!("export {item}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| {
            crate::Error::new("EXPORT_FAILED", "Cannot create generated type directory")
        })?;
    }
    std::fs::write(path, text)
        .map_err(|_| crate::Error::new("EXPORT_FAILED", "Cannot write generated types"))
}
