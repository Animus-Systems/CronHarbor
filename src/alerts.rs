//! Durable, metadata-only alert capture. All writes run on the single daemon owner.
use crate::{Error, Result, config::AlertConfig, contracts::*, store::Store};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const MAX_OUTSTANDING: u32 = 1000;
pub const MAX_HISTORY: u32 = 1000;
pub const MAX_ATTEMPTS: u32 = 12;
pub const DELIVERY_LIFETIME_US: i64 = 86_400_000_000;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const OUTSTANDING: &str = "state NOT IN ('DELIVERED','FAILED','EXPIRED')";

#[derive(Clone, Default)]
pub(crate) struct AlertCapturePolicy {
    destination_hash: Option<String>,
}
impl AlertCapturePolicy {
    pub(crate) fn from_config(config: Option<&AlertConfig>) -> Result<Self> {
        crate::server::validate_alert_config(config)?;
        #[cfg(feature = "webhooks")]
        if let Some(config) = config {
            return Ok(Self {
                destination_hash: Some(crate::webhooks::destination_hash(config)?),
            });
        }
        Ok(Self::default())
    }
}

#[derive(Clone, Debug)]
pub struct AlertDispatch {
    pub alert_id: String,
    pub nonce: String,
    pub destination_hash: String,
    pub payload_json: String,
}
#[derive(Clone, Copy, Debug)]
pub enum AlertReceipt {
    Accepted {
        http_status: u16,
    },
    Retry {
        code: AlertErrorCode,
        http_status: Option<u16>,
        retry_after_seconds: Option<u32>,
    },
    Failed {
        code: AlertErrorCode,
        http_status: Option<u16>,
    },
    Blocked {
        code: AlertErrorCode,
    },
}
#[derive(Clone, Debug)]
pub struct AlertResult {
    pub alert_id: String,
    pub nonce: String,
    pub destination_hash: String,
    pub receipt: AlertReceipt,
}

fn serialize(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(|_| Error::new("STATE_CORRUPT", "Invalid alert state"))
}
fn enum_name(value: &impl Serialize) -> Result<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| Error::new("STATE_CORRUPT", "Invalid alert state"))
}
fn parse_enum<T: DeserializeOwned>(value: String) -> rusqlite::Result<T> {
    serde_json::from_value(serde_json::Value::String(value))
        .map_err(|_| rusqlite::Error::InvalidQuery)
}
fn setting_counter(connection: &Connection, key: &str) -> Result<u64> {
    let value: String = connection.query_row(
        "SELECT value FROM runtime_settings WHERE key=?1",
        [key],
        |r| r.get(0),
    )?;
    value
        .parse::<u64>()
        .ok()
        .filter(|v| *v <= MAX_SAFE_INTEGER)
        .ok_or_else(|| Error::new("STATE_CORRUPT", "Invalid alert counter"))
}
fn increment_counter(connection: &Connection, name: &str, count: u64, now: i64) -> Result<()> {
    let key = format!("alert.{name}_count");
    let next = setting_counter(connection, &key)?
        .saturating_add(count)
        .min(MAX_SAFE_INTEGER);
    connection.execute(
        "UPDATE runtime_settings SET value=?1 WHERE key=?2",
        params![next.to_string(), key],
    )?;
    connection.execute("INSERT INTO runtime_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![format!("alert.{name}_at_us"),now.to_string()])?;
    Ok(())
}
fn restored(connection: &Connection) -> Result<bool> {
    Ok(connection.query_row("SELECT COALESCE((SELECT value='true' FROM runtime_settings WHERE key='restore_required'),0)", [], |r| r.get(0))?)
}

#[derive(Debug)]
struct Source {
    workload_id: String,
    namespace: String,
    name: String,
    revision_id: String,
    execution_id: Option<String>,
    attempt_id: Option<String>,
    error_code: Option<String>,
}
fn source_for_attempt(connection: &Connection, attempt: &str) -> Result<Source> {
    Ok(connection.query_row("SELECT w.id,w.namespace,w.name,e.revision_id,e.id,a.id,a.error_code FROM attempts a JOIN executions e ON e.id=a.execution_id JOIN workloads w ON w.id=e.workload_id WHERE a.id=?1", [attempt], |r| Ok(Source {workload_id:r.get(0)?,namespace:r.get(1)?,name:r.get(2)?,revision_id:r.get(3)?,execution_id:r.get(4)?,attempt_id:r.get(5)?,error_code:r.get(6)?}))?)
}
fn safe_source_error(value: Option<String>) -> Option<String> {
    value.filter(|s| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    })
}
struct Incident<'a> {
    id: &'a str,
    version: u32,
    kind: AlertKind,
    opened: i64,
}
fn condition(kind: AlertKind) -> (AlertSeverity, &'static str) {
    match kind {
        AlertKind::JobFailed => (
            AlertSeverity::Error,
            "Job failed after its permitted attempts.",
        ),
        AlertKind::JobTimedOut => (AlertSeverity::Error, "Job exceeded its execution deadline."),
        AlertKind::ExecutionUnknown => (
            AlertSeverity::Critical,
            "Execution outcome is unknown; operator reconciliation is required.",
        ),
        AlertKind::ServiceUnhealthy => (
            AlertSeverity::Error,
            "Service health checks report an unhealthy service.",
        ),
        AlertKind::ServiceFailed => (
            AlertSeverity::Error,
            "Service failed and has no automatic restart pending.",
        ),
        AlertKind::ServiceRecovered => (AlertSeverity::Info, "Service readiness has recovered."),
    }
}
fn enqueue(
    connection: &Connection,
    policy: &AlertCapturePolicy,
    source: Source,
    source_key: &str,
    incident: Incident<'_>,
    now: i64,
) -> Result<&'static str> {
    let Some(destination) = &policy.destination_hash else {
        return Ok("NONE");
    };
    let count: u32 = connection.query_row(
        &format!("SELECT count(*) FROM alert_deliveries WHERE {OUTSTANDING}"),
        [],
        |r| r.get(0),
    )?;
    if count >= MAX_OUTSTANDING {
        increment_counter(connection, "queue_overflow", 1, now)?;
        return Ok("SUPPRESSED");
    }
    let id = Uuid::new_v4().to_string();
    let instance: String = connection.query_row(
        "SELECT value FROM runtime_settings WHERE key='instance_id'",
        [],
        |r| r.get(0),
    )?;
    let (severity, summary) = condition(incident.kind);
    let envelope = AlertEnvelope {
        schema_version: 1,
        alert_id: id.clone(),
        instance_id: instance,
        incident_id: incident.id.into(),
        incident_version: incident.version,
        kind: incident.kind,
        severity,
        occurred_at_us: now,
        incident_opened_at_us: incident.opened,
        workload_id: source.workload_id.clone(),
        namespace: source.namespace,
        workload_name: source.name,
        revision_id: source.revision_id,
        execution_id: source.execution_id.clone(),
        attempt_id: source.attempt_id.clone(),
        error_code: safe_source_error(source.error_code),
        summary: summary.into(),
    };
    let payload = serialize(&envelope)?;
    if payload.len() > 8192
        || !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&now)
        || !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&incident.opened)
    {
        return Err(Error::new(
            "STATE_CORRUPT",
            "Alert metadata exceeds its bounded contract",
        ));
    }
    connection.execute("INSERT INTO alert_deliveries(id,source_key,incident_id,incident_version,kind,destination_hash,workload_id,execution_id,attempt_id,payload_json,state,created_at_us,updated_at_us,expires_at_us,next_attempt_at_us) VALUES(?1,?2,?3,?4,?12,?5,?6,?7,?8,?9,'PENDING',?10,?10,?11,?10)", params![id,source_key,incident.id,incident.version,destination,source.workload_id,source.execution_id,source.attempt_id,payload,now,now.saturating_add(DELIVERY_LIFETIME_US),enum_name(&incident.kind)?])?;
    Ok("QUEUED")
}

pub(crate) fn capture_result(
    connection: &Connection,
    policy: &AlertCapturePolicy,
    attempt: &str,
    now: i64,
) -> Result<()> {
    if policy.destination_hash.is_none() || restored(connection)? {
        return Ok(());
    }
    let (state,unknown_marker,execution_state,failure_marker,kind):(String,String,String,String,String)=connection.query_row("SELECT a.state,a.alert_unknown_outcome,e.state,e.alert_failure_outcome,w.kind FROM attempts a JOIN executions e ON e.id=a.execution_id JOIN workloads w ON w.id=e.workload_id WHERE a.id=?1", [attempt], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    if state == "UNKNOWN" && unknown_marker == "NONE" {
        let incident = Uuid::new_v4().to_string();
        let marker = enqueue(
            connection,
            policy,
            source_for_attempt(connection, attempt)?,
            &format!("unknown:{attempt}"),
            Incident {
                id: &incident,
                version: 1,
                kind: AlertKind::ExecutionUnknown,
                opened: now,
            },
            now,
        )?;
        connection.execute(
            "UPDATE attempts SET alert_unknown_outcome=?1 WHERE id=?2",
            params![marker, attempt],
        )?;
    }
    if kind != "managed_service"
        && matches!(execution_state.as_str(), "FAILED" | "TIMED_OUT")
        && failure_marker == "NONE"
    {
        let source = source_for_attempt(connection, attempt)?;
        let execution = source
            .execution_id
            .clone()
            .ok_or_else(|| Error::new("STATE_CORRUPT", "Alert execution is missing"))?;
        let incident = Uuid::new_v4().to_string();
        let marker = enqueue(
            connection,
            policy,
            source,
            &format!("failure:{execution}"),
            Incident {
                id: &incident,
                version: 1,
                kind: if execution_state == "TIMED_OUT" {
                    AlertKind::JobTimedOut
                } else {
                    AlertKind::JobFailed
                },
                opened: now,
            },
            now,
        )?;
        connection.execute(
            "UPDATE executions SET alert_failure_outcome=?1 WHERE id=?2",
            params![marker, execution],
        )?;
    }
    if kind == "managed_service" {
        let workload:String=connection.query_row("SELECT workload_id FROM executions WHERE id=(SELECT execution_id FROM attempts WHERE id=?1)",[attempt],|r|r.get(0))?;
        capture_service(connection, policy, &workload, now)?;
    }
    Ok(())
}

pub(crate) fn capture_service(
    connection: &Connection,
    policy: &AlertCapturePolicy,
    workload: &str,
    now: i64,
) -> Result<()> {
    if policy.destination_hash.is_none() || restored(connection)? {
        return Ok(());
    }
    let (desired,state,incident,opened):(String,String,Option<String>,Option<i64>)=connection.query_row("SELECT desired_state,observed_state,alert_incident_id,alert_incident_opened_at_us FROM service_instances WHERE workload_id=?1",[workload],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    if desired != "RUNNING"
        || (!matches!(state.as_str(), "UNHEALTHY" | "FAILED")
            && !(state == "READY" && incident.is_some()))
    {
        return Ok(());
    }
    let source=connection.query_row("SELECT w.id,w.namespace,w.name,si.revision_id FROM service_instances si JOIN workloads w ON w.id=si.workload_id WHERE si.workload_id=?1",[workload],|r|Ok(Source{workload_id:r.get(0)?,namespace:r.get(1)?,name:r.get(2)?,revision_id:r.get(3)?,execution_id:None,attempt_id:None,error_code:None}))?;
    if let Some(incident) = incident {
        if state == "READY" {
            let opened = opened.ok_or_else(|| {
                Error::new("STATE_CORRUPT", "Alert incident opening time is missing")
            })?;
            enqueue(
                connection,
                policy,
                source,
                &format!("service:{incident}:2"),
                Incident {
                    id: &incident,
                    version: 2,
                    kind: AlertKind::ServiceRecovered,
                    opened,
                },
                now,
            )?;
            connection.execute("UPDATE service_instances SET alert_incident_id=NULL,alert_incident_opened_at_us=NULL,alert_open_outcome='NONE' WHERE workload_id=?1",[workload])?;
        }
    } else if state != "READY" {
        let incident = Uuid::new_v4().to_string();
        let marker = enqueue(
            connection,
            policy,
            source,
            &format!("service:{incident}:1"),
            Incident {
                id: &incident,
                version: 1,
                kind: if state == "FAILED" {
                    AlertKind::ServiceFailed
                } else {
                    AlertKind::ServiceUnhealthy
                },
                opened: now,
            },
            now,
        )?;
        connection.execute("UPDATE service_instances SET alert_incident_id=?1,alert_incident_opened_at_us=?2,alert_open_outcome=?3 WHERE workload_id=?4",params![incident,now,marker,workload])?;
    }
    Ok(())
}

fn enforce_history(connection: &Connection, now: i64) -> Result<()> {
    let removed=connection.execute("DELETE FROM alert_deliveries WHERE id IN(SELECT id FROM alert_deliveries WHERE state IN('DELIVERED','FAILED','EXPIRED') ORDER BY finished_at_us DESC,id DESC LIMIT -1 OFFSET 1000)",[])?;
    if removed > 0 {
        increment_counter(
            connection,
            "history_eviction",
            u64::try_from(removed).unwrap_or(u64::MAX),
            now,
        )?;
    }
    Ok(())
}
/// A manual send is never replayed after a crash: the receiver may already have accepted it.
pub(crate) fn recover_manual_deliveries(connection: &Connection, now: i64) -> Result<()> {
    connection.execute("UPDATE operations SET state='FAILED',error_code='MANUAL_RECEIPT_UNKNOWN',updated_at_us=?1 WHERE id IN(SELECT manual_operation_id FROM alert_deliveries WHERE manual_once=1 AND state='IN_FLIGHT')",[now])?;
    connection.execute("UPDATE alert_deliveries SET state='FAILED',nonce=NULL,next_attempt_at_us=NULL,finished_at_us=?1,updated_at_us=?1,last_error_code='MANUAL_RECEIPT_UNKNOWN',manual_once=0,manual_operation_id=NULL WHERE manual_once=1 AND state='IN_FLIGHT'",[now])?;
    // Acceptance commits before the effect savepoint; an interrupted, unlinked intent never queued a send.
    connection.execute("UPDATE operations SET state='FAILED',error_code='START_INTERRUPTED',updated_at_us=?1 WHERE operation_kind='alert_retry' AND state IN('ACCEPTED','RUNNING') AND NOT EXISTS(SELECT 1 FROM alert_deliveries WHERE manual_operation_id=operations.id)",[now])?;
    enforce_history(connection, now)
}

pub fn retry_delay_seconds(id: &str, attempt: u32, retry_after: Option<u32>) -> u32 {
    let delay = 30_u32
        .saturating_mul(
            1_u32
                .checked_shl(attempt.saturating_sub(1).min(20))
                .unwrap_or(u32::MAX),
        )
        .min(3600);
    let digest = Sha256::digest(format!("{id}:{attempt}"));
    let jitter = u32::from(u16::from_be_bytes([digest[0], digest[1]])) * delay / (5 * 65_535);
    delay
        .saturating_add(jitter)
        .max(retry_after.unwrap_or(0).min(3600))
}

struct PendingAlert {
    id: String,
    payload: String,
    incident_id: String,
    incident_version: u32,
    kind: AlertKind,
    workload_id: String,
    execution_id: Option<String>,
    attempt_id: Option<String>,
    created_at_us: i64,
    manual: bool,
    operation: Option<String>,
}
fn valid_payload(connection: &Connection, row: &PendingAlert) -> Result<bool> {
    if row.payload.len() > 8192 {
        return Ok(false);
    }
    let Ok(envelope) = serde_json::from_str::<AlertEnvelope>(&row.payload) else {
        return Ok(false);
    };
    if [
        &envelope.alert_id,
        &envelope.instance_id,
        &envelope.incident_id,
        &envelope.workload_id,
        &envelope.revision_id,
    ]
    .into_iter()
    .chain(envelope.execution_id.iter())
    .chain(envelope.attempt_id.iter())
    .any(|id| Uuid::parse_str(id).is_err())
    {
        return Ok(false);
    }
    let (severity, summary) = condition(envelope.kind);
    let instance: String = connection.query_row(
        "SELECT value FROM runtime_settings WHERE key='instance_id'",
        [],
        |r| r.get(0),
    )?;
    let (namespace, name): (String, String) = connection.query_row(
        "SELECT namespace,name FROM workloads WHERE id=?1",
        [&row.workload_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let revision: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM workload_revisions WHERE workload_id=?1 AND id=?2)",
        params![row.workload_id, envelope.revision_id],
        |r| r.get(0),
    )?;
    let execution = if let Some(execution) = &row.execution_id {
        connection.query_row("SELECT EXISTS(SELECT 1 FROM executions WHERE id=?1 AND workload_id=?2 AND revision_id=?3)", params![execution,row.workload_id,envelope.revision_id], |r| r.get::<_,bool>(0))?
    } else {
        true
    };
    let safe_time = |time: i64| (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&time);
    Ok(envelope.schema_version == 1
        && envelope.alert_id == row.id
        && envelope.instance_id == instance
        && envelope.incident_id == row.incident_id
        && envelope.incident_version == row.incident_version
        && envelope.incident_version
            == if envelope.kind == AlertKind::ServiceRecovered {
                2
            } else {
                1
            }
        && envelope.kind == row.kind
        && envelope.workload_id == row.workload_id
        && envelope.namespace == namespace
        && envelope.workload_name == name
        && envelope.execution_id == row.execution_id
        && envelope.attempt_id == row.attempt_id
        && envelope.occurred_at_us == row.created_at_us
        && safe_time(envelope.occurred_at_us)
        && safe_time(envelope.incident_opened_at_us)
        && envelope.incident_opened_at_us <= envelope.occurred_at_us
        && envelope.severity == severity
        && envelope.summary == summary
        && revision
        && execution
        && safe_source_error(envelope.error_code.clone()) == envelope.error_code
        && serialize(&envelope)? == row.payload)
}

impl Store {
    pub fn alert_status(&self) -> Result<AlertStatusDto> {
        let (pending,in_flight,blocked,failed,outstanding,oldest,last):(u32,u32,u32,u32,u32,Option<i64>,Option<i64>)=self.connection.query_row("SELECT COALESCE(sum(state='PENDING'),0),COALESCE(sum(state='IN_FLIGHT'),0),COALESCE(sum(state='BLOCKED'),0),COALESCE(sum(state='FAILED'),0),COALESCE(sum(state NOT IN('DELIVERED','FAILED','EXPIRED')),0),min(CASE WHEN state='PENDING' THEN created_at_us END),max(delivered_at_us) FROM alert_deliveries",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
        let error:Option<String>=self.connection.query_row("SELECT last_error_code FROM alert_deliveries WHERE attempt_count>0 ORDER BY updated_at_us DESC,id DESC LIMIT 1",[],|r|r.get::<_,Option<String>>(0)).optional()?.flatten();
        let error = error.map(parse_enum).transpose()?;
        let configuration_state = if self.alert_policy.destination_hash.is_none() {
            AlertConfigurationState::Disabled
        } else if self.alert_worker_unavailable {
            AlertConfigurationState::WorkerUnavailable
        } else if error == Some(AlertErrorCode::CredentialUnavailable) {
            AlertConfigurationState::CredentialUnavailable
        } else {
            AlertConfigurationState::Ready
        };
        Ok(AlertStatusDto {
            configuration_state,
            pending,
            in_flight,
            blocked,
            failed,
            outstanding,
            oldest_pending_at_us: oldest,
            last_acknowledged_at_us: last,
            queue_overflow_count: setting_counter(&self.connection, "alert.queue_overflow_count")?,
            history_eviction_count: setting_counter(
                &self.connection,
                "alert.history_eviction_count",
            )?,
            restore_suppressed_count: setting_counter(
                &self.connection,
                "alert.restore_suppressed_count",
            )?,
            last_error_code: if self.alert_worker_unavailable {
                Some(AlertErrorCode::WorkerUnavailable)
            } else {
                error
            },
        })
    }
    pub fn alert_deliveries(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<AlertDeliveryDto>> {
        if !(1..=100).contains(&limit) {
            return Err(Error::new(
                "INPUT_INVALID",
                "Page limit must be between 1 and 100",
            ));
        }
        let boundary = cursor
            .map(|v| {
                if v.len() > 128 {
                    return Err(Error::new("INPUT_INVALID", "Invalid alert page cursor"));
                }
                let (created, id) = v
                    .split_once(':')
                    .ok_or_else(|| Error::new("INPUT_INVALID", "Invalid alert page cursor"))?;
                let created = created
                    .parse::<i64>()
                    .map_err(|_| Error::new("INPUT_INVALID", "Invalid alert page cursor"))?;
                let id = Uuid::parse_str(id)
                    .map_err(|_| Error::new("INPUT_INVALID", "Invalid alert page cursor"))?
                    .to_string();
                Ok((created, id))
            })
            .transpose()?;
        let (created, id) = boundary.unwrap_or((i64::MAX, String::new()));
        let mut query=self.connection.prepare("SELECT d.id,d.incident_id,d.incident_version,d.kind,d.workload_id,w.name,w.namespace,d.state,d.attempt_count,d.created_at_us,d.created_at_us,d.next_attempt_at_us,d.finished_at_us,d.delivered_at_us,d.http_status,d.last_error_code FROM alert_deliveries d JOIN workloads w ON w.id=d.workload_id WHERE d.created_at_us<?1 OR (d.created_at_us=?1 AND d.id<?2) ORDER BY d.created_at_us DESC,d.id DESC LIMIT ?3")?;
        let mut items = query
            .query_map(params![created, id, limit + 1], |r| {
                Ok(AlertDeliveryDto {
                    alert_id: r.get(0)?,
                    incident_id: r.get(1)?,
                    incident_version: r.get(2)?,
                    kind: parse_enum(r.get(3)?)?,
                    workload_id: r.get(4)?,
                    workload_name: r.get(5)?,
                    namespace: r.get(6)?,
                    state: parse_enum(r.get(7)?)?,
                    attempt_count: r.get(8)?,
                    occurred_at_us: r.get(9)?,
                    created_at_us: r.get(10)?,
                    next_attempt_at_us: r.get(11)?,
                    finished_at_us: r.get(12)?,
                    delivered_at_us: r.get(13)?,
                    http_status: r.get(14)?,
                    last_error_code: r
                        .get::<_, Option<String>>(15)?
                        .map(parse_enum)
                        .transpose()?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let more = items.len() > usize::try_from(limit).unwrap_or(100);
        items.truncate(usize::try_from(limit).unwrap_or(100));
        let next_cursor = if more {
            items
                .last()
                .map(|r| format!("{}:{}", r.created_at_us, r.alert_id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }
    pub(crate) fn retry_alert(&mut self, id: &str, operation: &str, now: i64) -> Result<()> {
        let Some(destination) = self.alert_policy.destination_hash.as_ref() else {
            return Err(Error::new(
                "ALERTS_DISABLED",
                "Outbound alerts are not configured",
            ));
        };
        if self.restore_required()? {
            return Err(Error::new(
                "RESTORE_REQUIRED",
                "Resume the restored installation before retrying alerts",
            ));
        }
        if self.alert_worker_unavailable {
            return Err(Error::new(
                "WORKER_UNAVAILABLE",
                "Outbound alert worker is unavailable",
            ));
        }
        let tx = self.connection.savepoint()?;
        let row: Option<(String, String, u32)> = tx
            .query_row(
                "SELECT state,destination_hash,attempt_count FROM alert_deliveries WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((state, old_destination, count)) = row else {
            return Err(Error::new(
                "NOT_FOUND",
                "The requested alert does not exist",
            ));
        };
        if !matches!(state.as_str(), "FAILED" | "BLOCKED" | "EXPIRED")
            || &old_destination != destination
            || count >= 2_147_483_647
        {
            return Err(Error::new(
                "STATE_CONFLICT",
                "Alert is not eligible for an individual retry",
            ));
        }
        let queued: u32 = tx.query_row(
            &format!("SELECT count(*) FROM alert_deliveries WHERE {OUTSTANDING}"),
            [],
            |r| r.get(0),
        )?;
        if state != "BLOCKED" && queued >= MAX_OUTSTANDING {
            return Err(Error::new(
                "ALERT_QUEUE_FULL",
                "Outbound alert queue is full",
            ));
        }
        tx.execute("UPDATE alert_deliveries SET state='PENDING',nonce=NULL,finished_at_us=NULL,delivered_at_us=NULL,next_attempt_at_us=?1,updated_at_us=?1,manual_once=1,manual_operation_id=?2,last_error_code=NULL,http_status=NULL WHERE id=?3",params![now,operation,id])?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn recover_alert_deliveries(&mut self, now: i64) -> Result<()> {
        let restore = self.restore_required()?;
        let tx = self.connection.transaction()?;
        if restore {
            tx.execute("UPDATE operations SET state='FAILED',error_code='RESTORE_REPLAY_SUPPRESSED',updated_at_us=?1 WHERE id IN(SELECT manual_operation_id FROM alert_deliveries WHERE manual_once=1)",[now])?;
            tx.execute("UPDATE alert_deliveries SET state='RESTORE_HELD',nonce=NULL,next_attempt_at_us=NULL,updated_at_us=?1,manual_once=0,manual_operation_id=NULL WHERE state NOT IN('DELIVERED','FAILED','EXPIRED')",[now])?;
        } else {
            tx.execute("UPDATE alert_deliveries SET state='PENDING',nonce=NULL,next_attempt_at_us=?1,updated_at_us=?1 WHERE state='IN_FLIGHT'",[now])?;
            if let Some(destination) = &self.alert_policy.destination_hash {
                tx.execute("UPDATE operations SET state='FAILED',error_code='DESTINATION_CHANGED',updated_at_us=?1 WHERE id IN(SELECT manual_operation_id FROM alert_deliveries WHERE manual_once=1 AND destination_hash!=?2)",params![now,destination])?;
                tx.execute("UPDATE alert_deliveries SET manual_once=0,manual_operation_id=NULL,updated_at_us=?1 WHERE manual_once=1 AND destination_hash!=?2",params![now,destination])?;
                tx.execute("UPDATE alert_deliveries SET state='DESTINATION_CHANGED',nonce=NULL,next_attempt_at_us=NULL,updated_at_us=?1,last_error_code='DESTINATION_CHANGED' WHERE destination_hash!=?2 AND state NOT IN('DELIVERED','FAILED','EXPIRED','RESTORE_HELD')",params![now,destination])?;
            }
        }
        enforce_history(&tx, now)?;
        tx.commit()?;
        self.maintain_alerts(now)
    }
    /// Bounded current-condition scan; terminal job history is intentionally excluded.
    pub fn scan_current_alerts(&mut self, now: i64) -> Result<()> {
        if self.alert_policy.destination_hash.is_none() || self.restore_required()? {
            return Ok(());
        }
        let attempts = {
            let mut q=self.connection.prepare("SELECT id FROM attempts WHERE state='UNKNOWN' AND alert_unknown_outcome='NONE' ORDER BY id LIMIT 100")?;
            q.query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let remaining = 100_usize.saturating_sub(attempts.len());
        let services = {
            let mut q=self.connection.prepare("SELECT workload_id FROM service_instances WHERE desired_state='RUNNING' AND ((observed_state IN('UNHEALTHY','FAILED') AND alert_incident_id IS NULL) OR (observed_state='READY' AND alert_incident_id IS NOT NULL)) ORDER BY workload_id LIMIT ?1")?;
            q.query_map([u32::try_from(remaining).unwrap_or(0)], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let tx = self.connection.transaction()?;
        for attempt in attempts {
            // UNKNOWN is the only execution condition scanned after enabling.
            capture_result(&tx, &self.alert_policy, &attempt, now)?;
        }
        for workload in services {
            capture_service(&tx, &self.alert_policy, &workload, now)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn maintain_alerts(&mut self, now: i64) -> Result<()> {
        let tx = self.connection.transaction()?;
        // Quarantined rows remain bounded evidence until explicit resume records restore suppression.
        tx.execute("UPDATE alert_deliveries SET state='EXPIRED',nonce=NULL,next_attempt_at_us=NULL,finished_at_us=?1,updated_at_us=?1,last_error_code=CASE WHEN expires_at_us<=?1 THEN 'DELIVERY_EXPIRED' ELSE 'RETRY_EXHAUSTED' END WHERE state NOT IN('DELIVERED','FAILED','EXPIRED','RESTORE_HELD') AND manual_once=0 AND (expires_at_us<=?1 OR (state!='IN_FLIGHT' AND attempt_count>=12))",[now])?;
        enforce_history(&tx, now)?;
        let removed=tx.execute("DELETE FROM alert_deliveries WHERE id IN(SELECT id FROM alert_deliveries WHERE state IN('DELIVERED','FAILED','EXPIRED') AND finished_at_us<?1 ORDER BY finished_at_us,id LIMIT 100)",[now.saturating_sub(604_800_000_000)])?;
        if removed > 0 {
            increment_counter(
                &tx,
                "history_eviction",
                u64::try_from(removed).unwrap_or(u64::MAX),
                now,
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn claim_alert(&mut self, now: i64) -> Result<Option<AlertDispatch>> {
        let Some(destination) = self.alert_policy.destination_hash.as_ref() else {
            return Ok(None);
        };
        if self.restore_required()? || self.alert_worker_unavailable {
            return Ok(None);
        }
        let tx = self.connection.transaction()?;
        let busy: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM alert_deliveries WHERE state='IN_FLIGHT')",
            [],
            |r| r.get(0),
        )?;
        if busy {
            return Ok(None);
        }
        let row:Option<PendingAlert>=tx.query_row("SELECT id,payload_json,incident_id,incident_version,kind,workload_id,execution_id,attempt_id,created_at_us,manual_once,manual_operation_id FROM alert_deliveries WHERE state='PENDING' AND destination_hash=?1 AND next_attempt_at_us<=?2 AND (manual_once=1 OR (attempt_count<12 AND expires_at_us>?2)) AND attempt_count<2147483647 ORDER BY next_attempt_at_us,created_at_us,id LIMIT 1",params![destination,now],|r|Ok(PendingAlert{id:r.get(0)?,payload:r.get(1)?,incident_id:r.get(2)?,incident_version:r.get(3)?,kind:parse_enum(r.get(4)?)?,workload_id:r.get(5)?,execution_id:r.get(6)?,attempt_id:r.get(7)?,created_at_us:r.get(8)?,manual:r.get(9)?,operation:r.get(10)?})).optional()?;
        let Some(row) = row else { return Ok(None) };
        if !valid_payload(&tx, &row)? {
            tx.execute("UPDATE alert_deliveries SET state=?1,last_error_code='INVALID_PAYLOAD',next_attempt_at_us=NULL,finished_at_us=?2,updated_at_us=?3,manual_once=0,manual_operation_id=NULL WHERE id=?4",params![if row.manual{"FAILED"}else{"BLOCKED"},if row.manual{Some(now)}else{None},now,row.id])?;
            if let Some(operation) = row.operation {
                tx.execute("UPDATE operations SET state='FAILED',error_code='INVALID_PAYLOAD',updated_at_us=?1 WHERE id=?2",params![now,operation])?;
                enforce_history(&tx, now)?;
            }
            tx.commit()?;
            return Ok(None);
        }
        let alert_id = row.id;
        let payload_json = row.payload;
        let nonce = Uuid::new_v4().to_string();
        tx.execute("UPDATE alert_deliveries SET state='IN_FLIGHT',nonce=?1,attempt_count=attempt_count+1,updated_at_us=?2,next_attempt_at_us=NULL WHERE id=?3",params![nonce,now,alert_id])?;
        tx.execute("UPDATE operations SET state='RUNNING',updated_at_us=?1 WHERE id=(SELECT manual_operation_id FROM alert_deliveries WHERE id=?2)",params![now,alert_id])?;
        tx.commit()?;
        Ok(Some(AlertDispatch {
            alert_id,
            nonce,
            destination_hash: destination.clone(),
            payload_json,
        }))
    }
    pub fn complete_alert(&mut self, result: AlertResult, now: i64) -> Result<bool> {
        let tx = self.connection.transaction()?;
        let row:Option<(u32,i64,bool,Option<String>)>=tx.query_row("SELECT attempt_count,expires_at_us,manual_once,manual_operation_id FROM alert_deliveries WHERE id=?1 AND state='IN_FLIGHT' AND nonce=?2 AND destination_hash=?3",params![result.alert_id,result.nonce,result.destination_hash],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let Some((count, expires, manual, operation)) = row else {
            return Ok(false);
        };
        let (state, error, http, next) = match result.receipt {
            AlertReceipt::Accepted { http_status } => ("DELIVERED", None, Some(http_status), None),
            AlertReceipt::Blocked { code } if !manual => ("BLOCKED", Some(code), None, None),
            AlertReceipt::Blocked { code }
            | AlertReceipt::Failed {
                code,
                http_status: None,
            } => ("FAILED", Some(code), None, None),
            AlertReceipt::Failed { code, http_status } => ("FAILED", Some(code), http_status, None),
            AlertReceipt::Retry {
                code,
                http_status,
                retry_after_seconds,
            } => {
                if manual {
                    ("FAILED", Some(code), http_status, None)
                } else if now >= expires {
                    (
                        "EXPIRED",
                        Some(AlertErrorCode::DeliveryExpired),
                        http_status,
                        None,
                    )
                } else if count >= MAX_ATTEMPTS {
                    (
                        "FAILED",
                        Some(AlertErrorCode::RetryExhausted),
                        http_status,
                        None,
                    )
                } else {
                    (
                        "PENDING",
                        Some(code),
                        http_status,
                        Some(now.saturating_add(
                            i64::from(retry_delay_seconds(
                                &result.alert_id,
                                count,
                                retry_after_seconds,
                            )) * 1_000_000,
                        )),
                    )
                }
            }
        };
        let terminal = matches!(state, "DELIVERED" | "FAILED" | "EXPIRED");
        let error = error.map(|e| enum_name(&e)).transpose()?;
        tx.execute("UPDATE alert_deliveries SET state=?1,nonce=NULL,next_attempt_at_us=?2,finished_at_us=?3,delivered_at_us=?4,http_status=?5,last_error_code=?6,updated_at_us=?7,manual_once=0,manual_operation_id=NULL WHERE id=?8",params![state,next,if terminal{Some(now)}else{None},if state=="DELIVERED"{Some(now)}else{None},http,error,now,result.alert_id])?;
        if let Some(operation) = operation {
            tx.execute(
                "UPDATE operations SET state=?1,error_code=?2,updated_at_us=?3 WHERE id=?4",
                params![
                    if state == "DELIVERED" {
                        "SUCCEEDED"
                    } else {
                        "FAILED"
                    },
                    error,
                    now,
                    operation
                ],
            )?;
        }
        if terminal {
            enforce_history(&tx, now)?;
        }
        tx.commit()?;
        Ok(true)
    }
    pub(crate) fn suppress_restored_alerts(connection: &Connection, now: i64) -> Result<()> {
        let count=connection.execute("UPDATE alert_deliveries SET state='EXPIRED',nonce=NULL,next_attempt_at_us=NULL,finished_at_us=?1,updated_at_us=?1,last_error_code='RESTORE_REPLAY_SUPPRESSED',manual_once=0,manual_operation_id=NULL WHERE state='RESTORE_HELD'",[now])?;
        if count > 0 {
            increment_counter(
                connection,
                "restore_suppressed",
                u64::try_from(count).unwrap_or(u64::MAX),
                now,
            )?;
        }
        enforce_history(connection, now)
    }
}
