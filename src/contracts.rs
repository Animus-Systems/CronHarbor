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
pub struct AdmissionStatusDto {
    #[ts(type = "number")]
    pub database_bytes: u64,
    #[ts(type = "number")]
    pub wal_bytes: u64,
    pub policy: crate::admission::AdmissionPolicy,
    pub warning: bool,
    pub paused: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub admission: Option<AdmissionStatusDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub log_policy: Option<LogStoragePolicyDto>,
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    JobFailed,
    JobTimedOut,
    ExecutionUnknown,
    ServiceUnhealthy,
    ServiceFailed,
    ServiceRecovered,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Info,
    Error,
    Critical,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertConfigurationState {
    Disabled,
    Ready,
    CredentialUnavailable,
    WorkerUnavailable,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AlertDeliveryState {
    Pending,
    InFlight,
    Delivered,
    Failed,
    Blocked,
    Expired,
    DestinationChanged,
    RestoreHeld,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AlertErrorCode {
    CredentialUnavailable,
    TlsUnavailable,
    InvalidDestination,
    DnsUnavailable,
    TransportUnavailable,
    RequestTimeout,
    HttpRejected,
    RetryExhausted,
    DeliveryExpired,
    DestinationChanged,
    ManualReceiptUnknown,
    RestoreReplaySuppressed,
    WorkerUnavailable,
    InvalidPayload,
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct AlertEnvelope {
    pub schema_version: u32,
    pub alert_id: String,
    pub instance_id: String,
    pub incident_id: String,
    pub incident_version: u32,
    pub kind: AlertKind,
    pub severity: AlertSeverity,
    #[ts(type = "number")]
    pub occurred_at_us: i64,
    #[ts(type = "number")]
    pub incident_opened_at_us: i64,
    pub workload_id: String,
    pub namespace: String,
    pub workload_name: String,
    pub revision_id: String,
    pub execution_id: Option<String>,
    pub attempt_id: Option<String>,
    pub error_code: Option<String>,
    pub summary: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct AlertStatusDto {
    pub configuration_state: AlertConfigurationState,
    pub pending: u32,
    pub in_flight: u32,
    pub blocked: u32,
    pub failed: u32,
    pub outstanding: u32,
    #[ts(type = "number | null")]
    pub oldest_pending_at_us: Option<i64>,
    #[ts(type = "number | null")]
    pub last_acknowledged_at_us: Option<i64>,
    #[ts(type = "number")]
    pub queue_overflow_count: u64,
    #[ts(type = "number")]
    pub history_eviction_count: u64,
    #[ts(type = "number")]
    pub restore_suppressed_count: u64,
    pub last_error_code: Option<AlertErrorCode>,
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct AlertDeliveryDto {
    pub alert_id: String,
    pub incident_id: String,
    pub incident_version: u32,
    pub kind: AlertKind,
    pub workload_id: String,
    pub workload_name: String,
    pub namespace: String,
    pub state: AlertDeliveryState,
    pub attempt_count: u32,
    #[ts(type = "number")]
    pub occurred_at_us: i64,
    #[ts(type = "number")]
    pub created_at_us: i64,
    #[ts(type = "number | null")]
    pub next_attempt_at_us: Option<i64>,
    #[ts(type = "number | null")]
    pub finished_at_us: Option<i64>,
    #[ts(type = "number | null")]
    pub delivered_at_us: Option<i64>,
    pub http_status: Option<u16>,
    pub last_error_code: Option<AlertErrorCode>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LogCaptureMode {
    Prefix,
    Rolling,
    ExternalSnapshot,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LogCaptureState {
    Prepared,
    Capturing,
    Sealed,
    Frozen,
    Stopped,
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct LogStoragePolicyInput {
    #[ts(type = "number")]
    pub max_disk_bytes: u64,
    pub max_segments: u32,
    #[ts(type = "number")]
    pub retention_seconds: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct LogStoragePolicyDto {
    // Upgrade provenance: legacy active-write and retention policy sets differed.
    pub imported_conservative: bool,
    #[ts(type = "number")]
    pub revision: u64,
    #[ts(type = "number")]
    pub max_disk_bytes: u64,
    pub max_segments: u32,
    #[ts(type = "number")]
    pub retention_seconds: u64,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, TS)]
pub struct LogStorageUsageDto {
    #[ts(type = "number")]
    pub retained_bytes: u64,
    pub retained_segments: u32,
    #[ts(type = "number")]
    pub reserved_bytes: u64,
    pub reserved_segments: u32,
    #[ts(type = "number")]
    pub protected_bytes: u64,
    pub protected_segments: u32,
    #[ts(type = "number")]
    pub unexplained_bytes: u64,
    pub unexplained_segments: u32,
    pub capture_error_count: u32,
    pub under_pressure: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct LogStorageDto {
    pub policy: LogStoragePolicyDto,
    pub usage: LogStorageUsageDto,
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct LogStreamDto {
    pub attempt_id: String,
    pub capture_mode: LogCaptureMode,
    pub capture_state: LogCaptureState,
    #[ts(type = "number")]
    pub retained_from_offset: u64,
    #[ts(type = "number")]
    pub written_end_offset: u64,
    #[ts(type = "number")]
    pub retained_bytes: u64,
    #[ts(type = "number")]
    pub known_discarded_bytes: u64,
    pub history_evicted: bool,
    pub log_truncated: bool,
    pub last_error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
pub struct LogDto {
    pub stream: LogStreamDto,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
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
    LatestLogs {
        attempt_id: String,
        max_bytes: u32,
    },
    LogInfo {
        attempt_id: String,
    },
    LogStorage,
    AlertStatus,
    AlertDeliveries {
        cursor: Option<String>,
        limit: u32,
    },
    RetryAlert {
        id: String,
        idempotency_key: String,
    },
    SetLogStorage {
        policy: LogStoragePolicyInput,
        expected_revision: u64,
        idempotency_key: String,
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
        crate::admission::AdmissionPolicy::decl(&ts_rs::Config::default()),
        AdmissionStatusDto::decl(&ts_rs::Config::default()),
        StatusDto::decl(&ts_rs::Config::default()),
        ExecutionDto::decl(&ts_rs::Config::default()),
        OperationDto::decl(&ts_rs::Config::default()),
        AlertKind::decl(&ts_rs::Config::default()),
        AlertSeverity::decl(&ts_rs::Config::default()),
        AlertConfigurationState::decl(&ts_rs::Config::default()),
        AlertDeliveryState::decl(&ts_rs::Config::default()),
        AlertErrorCode::decl(&ts_rs::Config::default()),
        AlertEnvelope::decl(&ts_rs::Config::default()),
        AlertStatusDto::decl(&ts_rs::Config::default()),
        AlertDeliveryDto::decl(&ts_rs::Config::default()),
        LogCaptureMode::decl(&ts_rs::Config::default()),
        LogCaptureState::decl(&ts_rs::Config::default()),
        LogStoragePolicyInput::decl(&ts_rs::Config::default()),
        LogStoragePolicyDto::decl(&ts_rs::Config::default()),
        LogStorageUsageDto::decl(&ts_rs::Config::default()),
        LogStorageDto::decl(&ts_rs::Config::default()),
        LogStreamDto::decl(&ts_rs::Config::default()),
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
