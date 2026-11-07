//! Shared command/query service. Transport permissions never replace these checks.
use crate::{
    Error, Result,
    config::Manifest,
    contracts::*,
    policy,
    store::{Store, audit_actor, event},
};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    str::FromStr,
};
fn encode(value: impl serde::Serialize) -> Result<Value> {
    serde_json::to_value(value)
        .map_err(|_| Error::new("INTERNAL_ERROR", "Cannot encode application response"))
}
fn input(message: &'static str) -> Error {
    Error::new("INPUT_INVALID", message)
}
fn missing() -> Error {
    Error::new("NOT_FOUND", "The requested resource does not exist")
}
fn scopes(principal: &Principal) -> Result<String> {
    serde_json::to_string(&principal.namespaces)
        .map_err(|_| Error::new("INTERNAL_ERROR", "Cannot encode identity scope"))
}
fn wildcard(principal: &Principal) -> bool {
    principal.namespaces.iter().any(|n| n == "*")
}
fn page_limit(value: u32) -> Result<u32> {
    if (1..=100).contains(&value) {
        Ok(value)
    } else {
        Err(input("Page limit must be between 1 and 100"))
    }
}
fn cursor_id(cursor: Option<String>) -> Result<String> {
    let value = cursor.unwrap_or_default();
    if value.len() > 128 || value.contains('\0') {
        return Err(input("Invalid page cursor"));
    }
    Ok(value)
}
fn audit_command(
    store: &mut Store,
    principal: &Principal,
    action: &str,
    target: &str,
    result: &str,
) -> Result<()> {
    let tx = store.connection.transaction()?;
    audit_actor(
        &tx,
        &principal.actor_id,
        action,
        target,
        result,
        crate::now_us(),
    )?;
    tx.commit()?;
    Ok(())
}
pub fn authenticate(store: &mut Store, token: Option<&str>) -> Result<Principal> {
    let Some(token) = token else {
        return Ok(policy::local_owner());
    };
    if token.len() < 16 || token.len() > 512 {
        store.record_auth_rejection()?;
        return Err(Error::new(
            "TOKEN_INVALID",
            "Bearer credential is invalid, expired or revoked",
        ));
    }
    let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
    let row:Option<(String,String,String,String)>=store.connection.query_row("SELECT actor_id,role,namespaces_json,capabilities_json FROM api_tokens WHERE hash=?1 AND revoked_at_us IS NULL AND (expires_at_us IS NULL OR expires_at_us>?2)",params![hash,crate::now_us()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((actor, role, namespaces, capabilities)) = row else {
        store.record_auth_rejection()?;
        return Err(Error::new(
            "TOKEN_INVALID",
            "Bearer credential is invalid, expired or revoked",
        ));
    };
    Ok(Principal {
        actor_id: actor,
        role: serde_json::from_str(&role).map_err(|_| policy::denied())?,
        namespaces: serde_json::from_str(&namespaces).map_err(|_| policy::denied())?,
        capabilities: serde_json::from_str(&capabilities).map_err(|_| policy::denied())?,
    })
}
pub fn execute(store: &mut Store, command: AppCommand, token: Option<&str>) -> Result<Value> {
    let principal = authenticate(store, token)?;
    let digest = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&command).map_err(|_| input("Cannot normalize command"))?
        )
    );
    let observed = principal.role == Role::Observe && !matches!(command, AppCommand::Identity);
    if observed {
        let action = serde_json::to_value(&command)
            .ok()
            .and_then(|value| {
                value
                    .get("action")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "query".into());
        audit_command(
            store,
            &principal,
            &format!("mcp.{action}"),
            "command",
            "REQUESTED",
        )?;
    }
    let result = dispatch(store, &principal, command, &digest, token.is_none());
    if let Err(error) = &result
        && error.code == "NOT_AUTHORIZED"
    {
        audit_command(store, &principal, "policy.deny", "command", "DENIED")?;
    }
    result
}
fn control_capability(manifest: &Manifest) -> &'static str {
    if manifest.kind == "Service" {
        "service.control_preapproved"
    } else {
        "workload.run_preapproved"
    }
}
fn require_instance_operations(principal: &Principal) -> Result<()> {
    policy::require(principal, "operations.manage", None)?;
    if wildcard(principal) {
        Ok(())
    } else {
        Err(policy::denied())
    }
}
fn require_log_policy(
    store: &Store,
    principal: &Principal,
    old: Option<&Manifest>,
    new: &Manifest,
) -> Result<()> {
    let policy = |m: &Manifest| {
        (
            m.spec.logs.max_disk_bytes,
            m.spec.logs.max_segments,
            m.spec.logs.retention_seconds,
        )
    };
    let changed = if let Some(old) = old {
        policy(old) != policy(new)
    } else {
        let defaults = crate::config::Logs::default();
        let (segments, age): (u32, i64) = store.connection.query_row(
            "SELECT MIN(?1,COALESCE(MIN(json_extract(r.spec_json,'$.spec.logs.maxSegments')),?1)),MIN(?2,COALESCE(MIN(json_extract(r.spec_json,'$.spec.logs.retentionSeconds')),?2)) FROM workloads w JOIN workload_revisions r ON r.id=w.current_revision_id",
            params![defaults.max_segments, i64::try_from(defaults.retention_seconds).unwrap_or(i64::MAX)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        new.spec.logs.max_disk_bytes < store.log_disk_limit()?
            || new.spec.logs.max_segments < segments
            || new.spec.logs.retention_seconds
                < u64::try_from(age).map_err(|_| {
                    Error::new("STATE_CORRUPT", "Invalid installation log retention")
                })?
    };
    // These workload fields control deletion and capture across the entire installation.
    if changed {
        require_instance_operations(principal)?;
    }
    Ok(())
}
fn authorized_workload(
    store: &Store,
    p: &Principal,
    id: &str,
    capability: &str,
) -> Result<(String, String, Manifest)> {
    let (id, revision, manifest) = store.manifest_for(id)?;
    policy::namespace(p, &manifest.metadata.namespace)?;
    policy::require(p, capability, Some(&id))?;
    Ok((id, revision, manifest))
}
fn authorized_execution(
    store: &Store,
    p: &Principal,
    id: &str,
    capability: &str,
) -> Result<ExecutionDto> {
    let row = execution(store, id)?;
    policy::namespace(p, &row.namespace)?;
    policy::require(p, capability, Some(&row.workload_id))?;
    Ok(row)
}
fn executor_permission(p: &Principal, m: &Manifest, id: Option<&str>) -> Result<()> {
    match m.spec.execution.adapter.as_str() {
        "docker" => policy::require(p, "docker.manage", id),
        "systemd" => policy::require(p, "systemd.manage", id),
        _ => Ok(()),
    }
}
fn dispatch(
    store: &mut Store,
    p: &Principal,
    command: AppCommand,
    digest: &str,
    local: bool,
) -> Result<Value> {
    match command {
        AppCommand::Identity => encode(p),
        AppCommand::Status => {
            if !policy::has(p, "instance.read", None) {
                policy::require(p, "workload.read", None)?;
            }
            let ns = scopes(p)?;
            let all = wildcard(p);
            let workloads=store.connection.query_row("SELECT count(*) FROM workloads WHERE ?1 OR namespace IN(SELECT value FROM json_each(?2))",params![all,ns],|r|r.get(0))?;
            let (active,unknown)=store.connection.query_row("SELECT COALESCE(SUM(a.state IN('LAUNCH_INTENT_RECORDED','START_REQUESTED','RUNNING','UNKNOWN')),0),COALESCE(SUM(a.state='UNKNOWN'),0) FROM attempts a JOIN executions e ON e.id=a.execution_id JOIN workloads w ON w.id=e.workload_id WHERE ?1 OR w.namespace IN(SELECT value FROM json_each(?2))",params![all,ns],|r|Ok((r.get(0)?,r.get(1)?)))?;
            encode(StatusDto {
                sqlite_version: rusqlite::version().into(),
                workloads,
                active,
                unknown,
                paused: store.paused()?,
                restore_required: store.restore_required()?,
                adapter_health: adapter_health(store)?,
            })
        }
        AppCommand::Workloads {
            namespace,
            cursor,
            limit,
        } => {
            policy::require(p, "workload.read", None)?;
            if let Some(ns) = &namespace {
                policy::namespace(p, ns)?;
            }
            encode(workloads(store, p, namespace.as_deref(), cursor, limit)?)
        }
        AppCommand::Workload { id } => {
            let (id, _, _) = authorized_workload(store, p, &id, "workload.read")?;
            encode(workload(store, &id)?)
        }
        AppCommand::ServiceHealth { id } => {
            let (id, _, m) = authorized_workload(store, p, &id, "workload.read")?;
            if m.kind != "Service" {
                return Err(input("Workload is not a managed service"));
            }
            encode(workload(store, &id)?)
        }
        AppCommand::Executions {
            workload_id,
            cursor,
            limit,
        } => {
            policy::require(p, "execution.read", None)?;
            let id = workload_id
                .map(|id| authorized_workload(store, p, &id, "execution.read").map(|v| v.0))
                .transpose()?;
            encode(executions(store, p, id.as_deref(), cursor, limit, None)?)
        }
        AppCommand::RecentFailures {
            window_seconds,
            limit,
        } => {
            policy::require(p, "execution.read", None)?;
            if window_seconds == 0 || window_seconds > 31_536_000 {
                return Err(input("Failure window is outside limits"));
            }
            let since = crate::now_us().saturating_sub(
                i64::try_from(window_seconds)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(1_000_000),
            );
            encode(executions(store, p, None, None, limit, Some(since))?)
        }
        AppCommand::Execution { id } => {
            encode(authorized_execution(store, p, &id, "execution.read")?)
        }
        AppCommand::Logs {
            attempt_id,
            offset,
            max_bytes,
        } => {
            if max_bytes == 0 || max_bytes > 65_536 {
                return Err(input("Log byte limit must be between 1 and 65536"));
            }
            let (execution_id,truncated,terminal):(String,bool,bool)=store.connection.query_row("SELECT a.execution_id,COALESCE(l.truncated,0),a.state IN('SUCCEEDED','FAILED','TIMED_OUT','CANCELLED') FROM attempts a LEFT JOIN log_segments l ON l.attempt_id=a.id WHERE a.id=?1",[&attempt_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or_else(missing)?;
            authorized_execution(store, p, &execution_id, "logs.read")?;
            let path = store.log_path(&attempt_id)?;
            let mut file = File::open(path).map_err(|_| {
                Error::new(
                    "LOG_UNAVAILABLE",
                    "Log segment has expired or is unavailable",
                )
            })?;
            let (bytes, next_offset, response_truncated) =
                log_window(&mut file, offset, max_bytes, terminal)?;
            encode(LogDto {
                attempt_id,
                next_offset,
                text: String::from_utf8_lossy(&bytes).into_owned(),
                response_truncated,
                log_truncated: truncated,
                untrusted: true,
            })
        }
        AppCommand::Preview { id, count } => {
            let (_, _, m) = authorized_workload(store, p, &id, "workload.read")?;
            if m.kind != "ScheduledJob" {
                return Err(input("Only scheduled jobs have future due instants"));
            }
            if count == 0 || count > 100 {
                return Err(input("Preview count must be between 1 and 100"));
            }
            let timezone = m.spec.schedule.timezone.clone();
            let tz = Tz::from_str(&timezone).map_err(|_| input("Invalid timezone"))?;
            let mut at = crate::now_us();
            let mut items = Vec::new();
            for _ in 0..count {
                at = m.next_after(at)?;
                let local = DateTime::<Utc>::from_timestamp_micros(at)
                    .ok_or_else(|| input("Invalid schedule timestamp"))?
                    .with_timezone(&tz)
                    .to_rfc3339();
                items.push(PreviewInstantDto { utc_us: at, local });
            }
            encode(PreviewDto { items, timezone })
        }
        AppCommand::Validate { definition } => {
            policy::require(p, "workload.read", None)?;
            let m = Manifest::parse(&definition)?;
            policy::namespace(p, &m.metadata.namespace)?;
            let (_, revision_hash) = m.canonical()?;
            encode(ValidationDto {
                valid: true,
                revision_hash,
            })
        }
        AppCommand::Plan { definition } => {
            policy::require(p, "workload.edit_safe_config", None)?;
            let m = Manifest::parse(&definition)?;
            policy::namespace(p, &m.metadata.namespace)?;
            let (_, hash) = m.canonical()?;
            let current:Option<(String,String)>=store.connection.query_row("SELECT r.id,r.spec_hash FROM workloads w JOIN workload_revisions r ON r.id=w.current_revision_id WHERE w.namespace=?1 AND w.name=?2",params![m.metadata.namespace,m.metadata.name],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let warnings = if m.spec.execution.adapter == "process" {
                vec![
                    "Native execution is trusted same-UID code; it is not a security sandbox"
                        .into(),
                ]
            } else {
                vec!["External executor requires an enabled operator allowlist".into()]
            };
            encode(PlanDto {
                current_revision: current.as_ref().map(|v| v.0.clone()),
                proposed_hash: hash.clone(),
                changed: current.is_none_or(|v| v.1 != hash),
                source_kind: m.spec.owner.to_ascii_uppercase(),
                warnings,
            })
        }
        AppCommand::Apply {
            definition,
            expected_revision,
            source_kind,
            idempotency_key,
        } => {
            let m = Manifest::parse(&definition)?;
            policy::namespace(p, &m.metadata.namespace)?;
            policy::require(p, "workload.edit_safe_config", None)?;
            if source_kind == "FILE" && !local {
                return Err(policy::denied());
            }
            if !matches!(source_kind.as_str(), "FILE" | "API") {
                return Err(input("Source kind must be FILE or API"));
            }
            let existing =
                store.manifest_for(&format!("{}/{}", m.metadata.namespace, m.metadata.name));
            let target = match &existing {
                Ok((id, _, _)) => id.clone(),
                Err(e) if e.code == "NOT_FOUND" => {
                    format!("{}/{}", m.metadata.namespace, m.metadata.name)
                }
                Err(_) => return existing.map(|_| Value::Null),
            };
            require_log_policy(store, p, existing.as_ref().ok().map(|row| &row.2), &m)?;
            let command_changed = match existing {
                Ok((_, _, old)) => command_context_changed(&old, &m),
                Err(_) => true,
            };
            if command_changed {
                policy::require(p, "executor.create_command", Some(&target))?;
            }
            executor_permission(p, &m, Some(&target))?;
            operation(
                store,
                p,
                &idempotency_key,
                digest,
                "apply",
                &target,
                |s, _| {
                    let revision = s.apply_scoped(
                        &m,
                        &source_kind,
                        expected_revision.as_deref(),
                        &p.actor_id,
                        crate::now_us(),
                    )?;
                    Ok(Some(json!({"revision_id":revision})))
                },
            )
        }
        AppCommand::ApplyBatch {
            definitions,
            expected_revisions,
            source_kind,
            idempotency_key,
        } => {
            policy::require(p, "workload.edit_safe_config", None)?;
            if source_kind == "FILE" && !local {
                return Err(policy::denied());
            }
            if definitions.is_empty()
                || definitions.len() > 128
                || definitions.len() != expected_revisions.len()
                || !matches!(source_kind.as_str(), "FILE" | "API")
            {
                return Err(input(
                    "Apply batch requires matching bounded definitions and expected revisions",
                ));
            }
            let mut manifests = Vec::new();
            let mut names = std::collections::BTreeSet::new();
            for definition in &definitions {
                let m = Manifest::parse(definition)?;
                policy::namespace(p, &m.metadata.namespace)?;
                let identity = format!("{}/{}", m.metadata.namespace, m.metadata.name);
                let (target, changed) = match store.manifest_for(&identity) {
                    Ok((id, _, old)) => {
                        require_log_policy(store, p, Some(&old), &m)?;
                        (id, command_context_changed(&old, &m))
                    }
                    Err(error) if error.code == "NOT_FOUND" => {
                        require_log_policy(store, p, None, &m)?;
                        (identity, true)
                    }
                    Err(error) => return Err(error),
                };
                if changed {
                    policy::require(p, "executor.create_command", Some(&target))?;
                }
                executor_permission(p, &m, Some(&target))?;
                if !names.insert(format!("{}/{}", m.metadata.namespace, m.metadata.name)) {
                    return Err(input("Apply batch contains duplicate workload identities"));
                }
                manifests.push(m);
            }
            operation(
                store,
                p,
                &idempotency_key,
                digest,
                "apply_batch",
                "batch",
                |s, _| {
                    s.connection.execute_batch("SAVEPOINT config_batch")?;
                    let result = (|| {
                        let mut revisions = Vec::new();
                        for (manifest, expected) in manifests.iter().zip(&expected_revisions) {
                            revisions.push(s.apply_scoped(
                                manifest,
                                &source_kind,
                                expected.as_deref(),
                                &p.actor_id,
                                crate::now_us(),
                            )?);
                        }
                        Ok(json!({"revision_ids":revisions}))
                    })();
                    match result {
                        Ok(value) => {
                            s.connection.execute_batch("RELEASE config_batch")?;
                            Ok(Some(value))
                        }
                        Err(error) => {
                            s.connection
                                .execute_batch("ROLLBACK TO config_batch; RELEASE config_batch")?;
                            Err(error)
                        }
                    }
                },
            )
        }
        AppCommand::Run {
            id,
            idempotency_key,
        } => {
            let (id, revision, m) = authorized_workload(store, p, &id, "workload.run_preapproved")?;
            executor_permission(p, &m, Some(&id))?;
            if let Some(value) = replay(store, p, &idempotency_key, digest)? {
                return Ok(value);
            }
            if m.kind == "Service" {
                return Err(input("Use service desired state to start managed services"));
            }
            if store.paused()? {
                return Err(Error::new("RESTORE_REQUIRED", "Dispatch is paused"));
            }
            let admissible: bool = store.connection.query_row(
                "SELECT enabled=1 AND paused=0 FROM workloads WHERE id=?1",
                [&id],
                |r| r.get(0),
            )?;
            if !admissible {
                return Err(Error::new(
                    "STATE_CONFLICT",
                    "Workload is disabled or paused",
                ));
            }
            operation(store, p, &idempotency_key, digest, "run", &id, |s, op| {
                let now = crate::now_us();
                let queued: i64 =
                    s.connection
                        .query_row("SELECT count(*) FROM manual_queue", [], |r| r.get(0))?;
                if queued >= 1000 {
                    return Err(Error::new("RESOURCE_EXHAUSTED", "Manual queue is full"));
                }
                s.connection.execute("INSERT INTO manual_queue(operation_id,workload_id,revision_id,actor_id,created_at_us,expires_at_us) VALUES(?1,?2,?3,?4,?5,?6)",params![op,id,revision,p.actor_id,now,now.saturating_add(3_600_000_000)])?;
                Ok(None)
            })
        }
        AppCommand::Pause {
            id,
            paused,
            idempotency_key,
        } => {
            let (id, _, m) = authorized_workload(store, p, &id, "workload.read")?;
            policy::require(p, control_capability(&m), Some(&id))?;
            executor_permission(p, &m, Some(&id))?;
            operation(store, p, &idempotency_key, digest, "pause", &id, |s, _| {
                s.connection.execute(
                    "UPDATE workloads SET paused=?1 WHERE id=?2",
                    params![paused, id],
                )?;
                Ok(Some(json!({"paused":paused})))
            })
        }
        AppCommand::ServiceDesired {
            id,
            desired_state,
            idempotency_key,
            expected_revision,
        } => {
            let (id, revision, m) =
                authorized_workload(store, p, &id, "service.control_preapproved")?;
            if let Some(value) = replay(store, p, &idempotency_key, digest)? {
                return Ok(value);
            }
            if (!local && expected_revision.is_none())
                || expected_revision
                    .as_deref()
                    .is_some_and(|expected| expected != revision)
            {
                return Err(Error::new(
                    "REVISION_CONFLICT",
                    "Service control requires the current workload revision",
                ));
            }
            if m.kind != "Service" || !matches!(desired_state.as_str(), "RUNNING" | "STOPPED") {
                return Err(input(
                    "Expected a service and RUNNING or STOPPED desired state",
                ));
            }
            executor_permission(p, &m, Some(&id))?;
            if desired_state == "RUNNING" && store.paused()? {
                return Err(Error::new("RESTORE_REQUIRED", "Dispatch is paused"));
            }
            operation(
                store,
                p,
                &idempotency_key,
                digest,
                "service_desired",
                &id,
                |s, op| {
                    bind_service_operation(s, op, &id, &desired_state, &revision)?;
                    s.connection.execute("UPDATE service_instances SET desired_state=?1,restart_requested=CASE WHEN ?1='STOPPED' THEN 0 WHEN EXISTS(SELECT 1 FROM attempts a JOIN executions e ON e.id=a.execution_id WHERE e.workload_id=?2 AND a.cancel_requested=1 AND a.state IN('START_REQUESTED','RUNNING')) THEN 1 ELSE restart_requested END,observed_state=CASE WHEN observed_state='FAILED' AND ?1='RUNNING' THEN 'ABSENT' ELSE observed_state END WHERE workload_id=?2",params![desired_state,id])?;
                    if desired_state == "STOPPED" {
                        s.connection.execute("UPDATE attempts SET cancel_requested=1 WHERE execution_id IN(SELECT id FROM executions WHERE workload_id=?1) AND state IN('RUNNING','START_REQUESTED')",[&id])?;
                    }
                    let active:i64=s.connection.query_row("SELECT count(*) FROM executions WHERE workload_id=?1 AND state IN('RUNNING','CREATED','UNKNOWN')",[&id],|r|r.get(0))?;
                    let ready:bool=s.connection.query_row("SELECT observed_state='READY' AND restart_requested=0 AND NOT EXISTS(SELECT 1 FROM attempts a JOIN executions e ON e.id=a.execution_id WHERE e.workload_id=?1 AND a.cancel_requested=1 AND a.state IN('START_REQUESTED','RUNNING')) FROM service_instances WHERE workload_id=?1",[&id],|r|r.get(0))?;
                    if desired_state == "RUNNING" && ready {
                        return Ok(Some(json!({"desired_state":desired_state})));
                    }
                    if active == 0 && desired_state == "STOPPED" {
                        s.connection.execute("UPDATE service_instances SET observed_state='ABSENT' WHERE workload_id=?1",[&id])?;
                        Ok(Some(json!({"desired_state":desired_state})))
                    } else {
                        Ok(None)
                    }
                },
            )
        }
        AppCommand::ServiceRestart {
            id,
            idempotency_key,
            expected_revision,
        } => {
            let (id, revision, m) =
                authorized_workload(store, p, &id, "service.control_preapproved")?;
            if let Some(value) = replay(store, p, &idempotency_key, digest)? {
                return Ok(value);
            }
            if (!local && expected_revision.is_none())
                || expected_revision
                    .as_deref()
                    .is_some_and(|expected| expected != revision)
            {
                return Err(Error::new(
                    "REVISION_CONFLICT",
                    "Service control requires the current workload revision",
                ));
            }
            if m.kind != "Service" {
                return Err(input("Workload is not a managed service"));
            }
            executor_permission(p, &m, Some(&id))?;
            if store.paused()? {
                return Err(Error::new("RESTORE_REQUIRED", "Dispatch is paused"));
            }
            let unknown: i64 = store.connection.query_row(
                "SELECT count(*) FROM executions WHERE workload_id=?1 AND state='UNKNOWN'",
                [&id],
                |r| r.get(0),
            )?;
            if unknown > 0 {
                return Err(Error::new(
                    "STATE_CONFLICT",
                    "Unknown service attempt must be reconciled before restart",
                ));
            }
            operation(
                store,
                p,
                &idempotency_key,
                digest,
                "service_restart",
                &id,
                |s, op| {
                    bind_service_operation(s, op, &id, "RUNNING", &revision)?;
                    s.connection.execute("UPDATE service_instances SET desired_state='RUNNING',restart_requested=1,observed_state=CASE WHEN observed_state IN('ABSENT','FAILED') THEN 'BACKOFF' ELSE observed_state END,next_action_at_us=?1 WHERE workload_id=?2",params![crate::now_us(),id])?;
                    s.connection.execute("UPDATE attempts SET cancel_requested=1 WHERE execution_id IN(SELECT id FROM executions WHERE workload_id=?1) AND state IN('RUNNING','START_REQUESTED')",[&id])?;
                    Ok(None)
                },
            )
        }
        AppCommand::Cancel {
            execution_id,
            idempotency_key,
        } => {
            let row = authorized_execution(store, p, &execution_id, "execution.read")?;
            let (_, _, m) = store.manifest_for(&row.workload_id)?;
            policy::require(p, control_capability(&m), Some(&row.workload_id))?;
            executor_permission(p, &m, Some(&row.workload_id))?;
            if let Some(value) = replay(store, p, &idempotency_key, digest)? {
                return Ok(value);
            }
            if row.state == "UNKNOWN" {
                return Err(Error::new(
                    "STATE_CONFLICT",
                    "Unknown execution needs evidence-based reconciliation",
                ));
            }
            operation(
                store,
                p,
                &idempotency_key,
                digest,
                "cancel",
                &execution_id,
                |s, _| {
                    let changed=s.connection.execute("UPDATE attempts SET cancel_requested=1 WHERE execution_id=?1 AND state IN('START_REQUESTED','RUNNING')",[&execution_id])?;
                    if changed == 0 {
                        let now = crate::now_us();
                        let queued = s.connection.execute(
                            "DELETE FROM retry_queue WHERE execution_id=?1",
                            [&execution_id],
                        )?;
                        let cancelled=s.connection.execute("UPDATE attempts SET state='CANCELLED',error_code='CANCELLED_BEFORE_START',finished_at_us=?1 WHERE execution_id=?2 AND state IN('CREATED','LAUNCH_INTENT_RECORDED')",params![now,execution_id])?;
                        if cancelled > 0 || queued > 0 {
                            s.connection.execute("UPDATE executions SET state='CANCELLED',finished_at_us=?1 WHERE id=?2",params![now,execution_id])?;
                            s.connection.execute("UPDATE operations SET state='FAILED',error_code=?1,updated_at_us=?2 WHERE execution_id=?3 AND state IN('ACCEPTED','RUNNING')",params![if queued > 0 { "CANCELLED_BY_OPERATOR" } else { "CANCELLED_BEFORE_START" },now,execution_id])?;
                        }
                        Ok(Some(
                            json!({"state":if cancelled>0 || queued>0{"CANCELLED"}else{row.state.as_str()}}),
                        ))
                    } else {
                        Ok(None)
                    }
                },
            )
        }
        AppCommand::Reconcile {
            execution_id,
            state,
            evidence,
            idempotency_key,
        } => {
            policy::require(p, "operations.manage", None)?;
            let row = authorized_execution(store, p, &execution_id, "execution.read")?;
            if let Some(value) = replay(store, p, &idempotency_key, digest)? {
                return Ok(value);
            }
            if row.state != "UNKNOWN"
                || !matches!(
                    state.as_str(),
                    "SUCCEEDED" | "FAILED" | "CANCELLED" | "TIMED_OUT"
                )
                || evidence.trim().is_empty()
                || evidence.len() > 4096
            {
                return Err(input(
                    "Reconciliation requires an unknown execution, a terminal state and bounded observed evidence",
                ));
            }
            operation(
                store,
                p,
                &idempotency_key,
                digest,
                "reconcile",
                &execution_id,
                |s, _| {
                    let now = crate::now_us();
                    let tx = s.connection.savepoint()?;
                    tx.execute("UPDATE attempts SET state=?1,error_code='OPERATOR_RECONCILED',finished_at_us=?2 WHERE execution_id=?3 AND state='UNKNOWN'",params![state,now,execution_id])?;
                    tx.execute("UPDATE executions SET state=?1,finished_at_us=?2 WHERE id=?3 AND state='UNKNOWN'",params![state,now,execution_id])?;
                    tx.execute("UPDATE service_instances SET observed_state='ABSENT',last_observed_at_us=?1 WHERE workload_id=?2 AND observed_state='UNKNOWN'",params![now,row.workload_id])?;
                    event(
                        &tx,
                        "execution.reconciled",
                        Some(&row.workload_id),
                        Some(&execution_id),
                        json!({"state":state,"evidence":evidence}),
                        now,
                    )?;
                    audit_actor(
                        &tx,
                        &p.actor_id,
                        "execution.reconcile",
                        &execution_id,
                        "SUCCEEDED",
                        now,
                    )?;
                    tx.commit()?;
                    Ok(Some(json!({"state":state})))
                },
            )
        }
        AppCommand::Operation { id } => {
            let operation = operation_dto(store, &id)?;
            let actor: String = store.connection.query_row(
                "SELECT actor_id FROM operations WHERE id=?1",
                [&id],
                |r| r.get(0),
            )?;
            if actor != p.actor_id {
                policy::require(p, "operations.manage", None)?;
                if !wildcard(p) {
                    match operation.kind.as_str() {
                        "apply" if operation.target_id.contains('/') => {
                            let (namespace, _) = operation
                                .target_id
                                .split_once('/')
                                .ok_or_else(policy::denied)?;
                            policy::namespace(p, namespace)?;
                        }
                        "apply" | "run" | "pause" | "service_desired" | "service_restart" => {
                            authorized_workload(store, p, &operation.target_id, "workload.read")?;
                        }
                        "cancel" | "reconcile" => {
                            authorized_execution(store, p, &operation.target_id, "execution.read")?;
                        }
                        _ => return Err(policy::denied()),
                    }
                }
            }
            encode(operation)
        }
        AppCommand::Events { after, limit } => {
            policy::require(p, "execution.read", None)?;
            encode(events(store, p, after, limit)?)
        }
        AppCommand::Diagnostics { execution_id } => {
            let row = authorized_execution(store, p, &execution_id, "execution.read")?;
            let evidence = execution_events(store, &execution_id)?;
            let observed_causes = row.error_code.iter().cloned().collect();
            let hypotheses = if row.state == "UNKNOWN" {
                vec!["The daemon lost verified executor observation; external effects may have occurred".into()]
            } else {
                vec![]
            };
            let retry_scheduled: bool = store.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM retry_queue WHERE execution_id=?1)",
                [&execution_id],
                |r| r.get(0),
            )?;
            encode(DiagnosticDto {
                execution_id,
                state: row.state,
                observed_causes,
                hypotheses,
                evidence,
                safe_to_retry_automatically: retry_scheduled,
            })
        }
        AppCommand::CreateToken {
            role,
            namespaces,
            capabilities,
            ttl_seconds,
        } => {
            policy::require(p, "tokens.manage", None)?;
            if ttl_seconds == 0
                || ttl_seconds > 31_536_000
                || namespaces.is_empty()
                || namespaces.len() > 128
                || capabilities.len() > 128
                || namespaces.iter().any(|n| {
                    n.is_empty()
                        || n.len() > 128
                        || !n.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b"*.-".contains(&b)
                        })
                })
                || capabilities.iter().any(|c| {
                    c.is_empty()
                        || c.len() > 256
                        || !c
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"*._:-/".contains(&b))
                })
            {
                return Err(input("Token scope, grants or lifetime are outside limits"));
            }
            for namespace in &namespaces {
                policy::namespace(p, namespace)?;
            }
            for capability in &capabilities {
                if !policy::has(p, capability, None) {
                    return Err(policy::denied());
                }
            }
            let id = uuid::Uuid::new_v4().to_string();
            let token = format!(
                "ch_{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            let now = crate::now_us();
            let expiry = now
                .checked_add(
                    i64::try_from(ttl_seconds)
                        .map_err(|_| input("Invalid token lifetime"))?
                        .saturating_mul(1_000_000),
                )
                .ok_or_else(|| input("Invalid token lifetime"))?;
            let actor_id = format!("token:{id}");
            let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
            let tx = store.connection.transaction()?;
            tx.execute("INSERT INTO api_tokens(id,actor_id,hash,scopes_json,created_at_us,expires_at_us,role,namespaces_json,capabilities_json) VALUES(?1,?2,?3,'[]',?4,?5,?6,?7,?8)",params![id,actor_id,hash,now,expiry,serde_json::to_string(&role).map_err(|_|input("Invalid role"))?,serde_json::to_string(&namespaces).map_err(|_|input("Invalid namespaces"))?,serde_json::to_string(&capabilities).map_err(|_|input("Invalid capabilities"))?])?;
            audit_actor(&tx, &p.actor_id, "token.create", &id, "SUCCEEDED", now)?;
            tx.commit()?;
            encode(CreatedTokenDto {
                token,
                metadata: TokenDto {
                    id,
                    actor_id,
                    role,
                    namespaces,
                    capabilities,
                    expires_at_us: Some(expiry),
                    revoked: false,
                },
            })
        }
        AppCommand::Tokens { cursor, limit } => {
            policy::require(p, "tokens.manage", None)?;
            let limit = page_limit(limit)?;
            let cursor = cursor_id(cursor)?;
            let mut q=store.connection.prepare("SELECT id,actor_id,role,namespaces_json,capabilities_json,expires_at_us,revoked_at_us IS NOT NULL FROM api_tokens WHERE id>?1 ORDER BY id LIMIT 1001")?;
            let mut rows = q.query([cursor])?;
            let mut items = Vec::new();
            let mut consumed = None;
            let mut bytes = 0;
            let mut scanned = 0;
            let mut more = false;
            while let Some(row) = rows.next()? {
                scanned += 1;
                if scanned > 1000 {
                    more = true;
                    break;
                }
                let metadata = token_row(row)?;
                if token_authority(p, &metadata).is_err() {
                    consumed = Some(metadata.id);
                    continue;
                }
                let size = serde_json::to_vec(&metadata)
                    .map_err(|_| input("Cannot encode token metadata"))?
                    .len();
                if items.len() >= usize::try_from(limit).unwrap_or(100) || bytes + size > 256 * 1024
                {
                    more = true;
                    break;
                }
                bytes += size;
                consumed = Some(metadata.id.clone());
                items.push(metadata);
            }
            encode(Page {
                items,
                next_cursor: if more { consumed } else { None },
            })
        }
        AppCommand::RevokeToken { id } => {
            policy::require(p, "tokens.manage", None)?;
            let metadata=store.connection.query_row("SELECT id,actor_id,role,namespaces_json,capabilities_json,expires_at_us,revoked_at_us IS NOT NULL FROM api_tokens WHERE id=?1",[&id],token_row).optional()?.ok_or_else(missing)?;
            token_authority(p, &metadata)?;
            let tx = store.connection.transaction()?;
            let changed = tx.execute(
                "UPDATE api_tokens SET revoked_at_us=COALESCE(revoked_at_us,?1) WHERE id=?2",
                params![crate::now_us(), id],
            )?;
            if changed == 0 {
                return Err(missing());
            }
            audit_actor(
                &tx,
                &p.actor_id,
                "token.revoke",
                &id,
                "SUCCEEDED",
                crate::now_us(),
            )?;
            tx.commit()?;
            Ok(json!({"id":id,"revoked":true}))
        }
        AppCommand::Audit { cursor, limit } => {
            require_instance_operations(p)?;
            let limit = page_limit(limit)?;
            let after = cursor
                .unwrap_or_else(|| "0".into())
                .parse::<i64>()
                .map_err(|_| input("Invalid audit cursor"))?;
            let mut q=store.connection.prepare("SELECT seq,actor_id,action,target_id,result,request_id,created_at_us FROM audit_events WHERE seq>?1 ORDER BY seq LIMIT ?2")?;
            let mut items = q
                .query_map(params![after, limit + 1], |r| {
                    Ok(AuditDto {
                        seq: r.get(0)?,
                        actor_id: r.get(1)?,
                        action: r.get(2)?,
                        target_id: r.get(3)?,
                        result: r.get(4)?,
                        request_id: r.get(5)?,
                        created_at_us: r.get(6)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let next_cursor = if items.len() > usize::try_from(limit).unwrap_or(500) {
                items.truncate(usize::try_from(limit).unwrap_or(500));
                items.last().map(|row| row.seq.to_string())
            } else {
                None
            };
            encode(Page { items, next_cursor })
        }
        AppCommand::Backup { name } => {
            require_instance_operations(p)?;
            audit_command(store, p, "backup.request", &name, "ACCEPTED")?;
            let backup = store.backup(&name)?;
            audit_command(store, p, "backup.complete", &name, "SUCCEEDED")?;
            Ok(backup)
        }
        AppCommand::GlobalPause { paused, evidence } => {
            require_instance_operations(p)?;
            if evidence.trim().is_empty() || evidence.len() > 4096 {
                return Err(input("Pause changes require bounded operator evidence"));
            }
            if !paused && store.restore_required()? {
                return Err(Error::new(
                    "RESTORE_REQUIRED",
                    "Restore reconciliation must complete before dispatch resumes",
                ));
            }
            let tx = store.connection.transaction()?;
            tx.execute(
                "UPDATE runtime_settings SET value=?1 WHERE key='global_paused'",
                [if paused { "true" } else { "false" }],
            )?;
            audit_actor(
                &tx,
                &p.actor_id,
                "global.pause",
                "instance",
                "SUCCEEDED",
                crate::now_us(),
            )?;
            event(
                &tx,
                "global.pause",
                None,
                None,
                json!({"paused":paused,"evidence":evidence}),
                crate::now_us(),
            )?;
            tx.commit()?;
            Ok(json!({"paused":paused}))
        }
        AppCommand::RestoreResume {
            cutoff_us,
            evidence,
        } => {
            if !local {
                return Err(policy::denied());
            }
            require_instance_operations(p)?;
            store.restore_resume(cutoff_us, &evidence, &p.actor_id)?;
            Ok(json!({"paused":false,"cutoff_us":cutoff_us}))
        }
    }
}
fn workload(store: &Store, id: &str) -> Result<WorkloadDto> {
    let row=store.connection.query_row("SELECT w.id,w.namespace,w.name,w.kind,w.source_kind,w.current_revision_id,w.enabled,w.paused,r.spec_json,s.next_due_at_us,si.desired_state,si.observed_state,COALESCE(si.restart_count,0) FROM workloads w JOIN workload_revisions r ON r.id=w.current_revision_id LEFT JOIN schedules s ON s.workload_id=w.id AND s.active=1 LEFT JOIN service_instances si ON si.workload_id=w.id WHERE w.id=?1",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,bool>(6)?,r.get::<_,bool>(7)?,r.get::<_,String>(8)?,r.get::<_,Option<i64>>(9)?,r.get::<_,Option<String>>(10)?,r.get::<_,Option<String>>(11)?,r.get::<_,u32>(12)?))).optional()?.ok_or_else(missing)?;
    let m = Manifest::parse(&row.8)?;
    let description = if m.kind == "ScheduledJob" {
        Some(if m.spec.schedule.r#type == "interval" {
            format!(
                "every {}s anchored at {}",
                m.spec.schedule.interval_seconds, m.spec.schedule.anchor_at_us
            )
        } else {
            format!(
                "{} ({})",
                m.spec.schedule.expression, m.spec.schedule.timezone
            )
        })
    } else {
        None
    };
    let probe_history = probe_history(store, &row.0)?;
    Ok(WorkloadDto {
        id: row.0,
        namespace: row.1,
        name: row.2,
        kind: row.3,
        source_kind: row.4,
        revision_id: row.5,
        enabled: row.6,
        paused: row.7,
        executor: m.spec.execution.adapter,
        schedule_description: description,
        next_due_at_us: row.9,
        desired_state: row.10,
        observed_state: row.11,
        restart_count: row.12,
        definition: row.8,
        probe_history,
    })
}
fn workloads(
    store: &Store,
    p: &Principal,
    namespace: Option<&str>,
    cursor: Option<String>,
    limit: u32,
) -> Result<Page<WorkloadDto>> {
    let limit = page_limit(limit)?;
    let cursor = cursor_id(cursor)?;
    let mut q=store.connection.prepare("SELECT id FROM workloads WHERE id>?1 AND (?2 IS NULL OR namespace=?2) AND (?3 OR namespace IN(SELECT value FROM json_each(?4))) ORDER BY id LIMIT ?5")?;
    let ids = q
        .query_map(
            params![cursor, namespace, wildcard(p), scopes(p)?, limit + 1],
            |r| r.get::<_, String>(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let more = ids.len() > usize::try_from(limit).unwrap_or(500);
    let mut items = Vec::new();
    for id in ids.into_iter().take(usize::try_from(limit).unwrap_or(500)) {
        let mut row = workload(store, &id)?;
        row.definition.clear();
        row.probe_history.clear();
        items.push(row);
    }
    let next_cursor = if more {
        items.last().map(|v| v.id.clone())
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}
fn row_execution(r: &rusqlite::Row<'_>) -> rusqlite::Result<ExecutionDto> {
    Ok(ExecutionDto {
        execution_id: r.get(0)?,
        attempt_id: r.get(1)?,
        workload_id: r.get(2)?,
        namespace: r.get(3)?,
        name: r.get(4)?,
        state: r.get(5)?,
        due_at_us: r.get(6)?,
        started_at_us: r.get(7)?,
        finished_at_us: r.get(8)?,
        error_code: r.get(9)?,
        exit_code: r.get(10)?,
        log_truncated: r.get(11)?,
    })
}
const EXEC_SELECT: &str = "SELECT e.id,a.id,w.id,w.namespace,w.name,a.state,o.scheduled_at_us,a.started_at_us,a.finished_at_us,a.error_code,a.exit_code,COALESCE(l.truncated,0) FROM executions e JOIN attempts a ON a.execution_id=e.id JOIN workloads w ON w.id=e.workload_id LEFT JOIN occurrences o ON o.id=e.occurrence_id LEFT JOIN log_segments l ON l.attempt_id=a.id";
fn execution(store: &Store, id: &str) -> Result<ExecutionDto> {
    store
        .connection
        .query_row(
            &format!("{EXEC_SELECT} WHERE e.id=?1 ORDER BY a.attempt_index DESC LIMIT 1"),
            [id],
            row_execution,
        )
        .optional()?
        .ok_or_else(missing)
}
fn executions(
    store: &Store,
    p: &Principal,
    workload_id: Option<&str>,
    cursor: Option<String>,
    limit: u32,
    since: Option<i64>,
) -> Result<Page<ExecutionDto>> {
    let limit = page_limit(limit)?;
    let cursor = cursor_id(cursor)?;
    let (before, id) = if cursor.is_empty() {
        (i64::MAX, String::new())
    } else {
        let (time, id) = cursor
            .split_once(':')
            .ok_or_else(|| input("Invalid execution cursor"))?;
        (
            time.parse::<i64>()
                .map_err(|_| input("Invalid execution cursor"))?,
            id.to_owned(),
        )
    };
    let timestamp = if since.is_some() {
        "COALESCE(a.finished_at_us,a.started_at_us,a.created_at_us)"
    } else {
        "e.created_at_us"
    };
    let sql = format!(
        "{EXEC_SELECT} WHERE a.attempt_index=(SELECT MAX(x.attempt_index) FROM attempts x WHERE x.execution_id=e.id) AND (?1 IS NULL OR w.id=?1) AND (?2 OR w.namespace IN(SELECT value FROM json_each(?3))) AND ({timestamp}<?4 OR({timestamp}=?4 AND e.id<?5)) AND (?6 IS NULL OR a.state='UNKNOWN' OR(a.state IN('FAILED','TIMED_OUT') AND a.finished_at_us>=?6)) ORDER BY {timestamp} DESC,e.id DESC LIMIT ?7"
    );
    let mut q = store.connection.prepare(&sql)?;
    let mut items = q
        .query_map(
            params![
                workload_id,
                wildcard(p),
                scopes(p)?,
                before,
                id,
                since,
                limit + 1
            ],
            row_execution,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let next_cursor = if items.len() > usize::try_from(limit).unwrap_or(500) {
        items.truncate(usize::try_from(limit).unwrap_or(500));
        if let Some(last) = items.last() {
            let created: i64 = store.connection.query_row(
                &format!("SELECT {timestamp} FROM executions e JOIN attempts a ON a.execution_id=e.id WHERE e.id=?1 ORDER BY a.attempt_index DESC LIMIT 1"),
                [&last.execution_id],
                |r| r.get(0),
            )?;
            Some(format!("{created}:{}", last.execution_id))
        } else {
            None
        }
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}
fn operation_dto(store: &Store, id: &str) -> Result<OperationDto> {
    store.connection.query_row("SELECT id,state,target_id,operation_kind,created_at_us,error_code FROM operations WHERE id=?1",[id],|r|Ok(OperationDto{id:r.get(0)?,state:r.get(1)?,target_id:r.get(2)?,kind:r.get(3)?,created_at_us:r.get(4)?,error_code:r.get(5)?})).optional()?.ok_or_else(missing)
}
fn replay(store: &Store, p: &Principal, key: &str, digest: &str) -> Result<Option<Value>> {
    let hash = format!("{:x}", Sha256::digest(key.as_bytes()));
    let row:Option<(String,String,Option<String>,String)>=store.connection.query_row("SELECT id,request_digest,error_code,state FROM operations WHERE actor_id=?1 AND request_key_hash=?2",params![p.actor_id,hash],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((id, old, error, state)) = row else {
        return Ok(None);
    };
    if old != digest {
        return Err(Error::new(
            "IDEMPOTENCY_CONFLICT",
            "Idempotency key was already used with a different request",
        ));
    }
    if state == "FAILED"
        && let Some(code) = error
    {
        return Err(Error::new(
            code,
            "The original operation failed; inspect operation history",
        ));
    }
    Ok(Some(encode(operation_dto(store, &id)?)?))
}
fn operation(
    store: &mut Store,
    p: &Principal,
    key: &str,
    digest: &str,
    kind: &str,
    target: &str,
    apply: impl FnOnce(&mut Store, &str) -> Result<Option<Value>>,
) -> Result<Value> {
    if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(input("A bounded printable idempotency key is required"));
    }
    let key_hash = format!("{:x}", Sha256::digest(key.as_bytes()));
    if let Some(value) = replay(store, p, key, digest)? {
        return Ok(value);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::now_us();
    let tx = store.connection.transaction()?;
    tx.execute("INSERT INTO operations(id,actor_id,request_key_hash,request_digest,operation_kind,target_id,state,created_at_us,updated_at_us) VALUES(?1,?2,?3,?4,?5,?6,'ACCEPTED',?7,?7)",params![id,p.actor_id,key_hash,digest,kind,target,now])?;
    audit_actor(&tx, &p.actor_id, kind, target, "ACCEPTED", now)?;
    tx.commit()?;
    store
        .connection
        .execute_batch("SAVEPOINT operation_effect")?;
    let result: Result<Value> = (|| {
        if let Some(result) = apply(store, &id)? {
            store.connection.execute("UPDATE operations SET state='SUCCEEDED',result_json=?1,updated_at_us=?2 WHERE id=?3",params![result.to_string(),crate::now_us(),id])?;
            event(
                &store.connection,
                "operation.complete",
                None,
                None,
                json!({"operation_id":id,"state":"SUCCEEDED"}),
                crate::now_us(),
            )?;
        }
        let response = encode(operation_dto(store, &id)?)?;
        store.connection.execute_batch("RELEASE operation_effect")?;
        Ok(response)
    })();
    match result {
        Ok(response) => Ok(response),
        Err(error) => {
            if !store.connection.is_autocommit() {
                store
                    .connection
                    .execute_batch("ROLLBACK TO operation_effect; RELEASE operation_effect")?;
            }
            store.connection.execute(
                "UPDATE operations SET state='FAILED',error_code=?1,updated_at_us=?2 WHERE id=?3",
                params![error.code, crate::now_us(), id],
            )?;
            audit_command(store, p, kind, target, "FAILED")?;
            Err(error)
        }
    }
}
fn event_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<EventDto> {
    Ok(EventDto {
        seq: r.get(0)?,
        event_kind: r.get(1)?,
        workload_id: r.get(2)?,
        execution_id: r.get(3)?,
        payload_json: r.get(4)?,
        created_at_us: r.get(5)?,
    })
}
fn events(store: &Store, p: &Principal, after: i64, limit: u32) -> Result<Page<EventDto>> {
    let limit = page_limit(limit)?;
    if after < 0 {
        return Err(input("Event cursor cannot be negative"));
    }
    let earliest: Option<i64> =
        store
            .connection
            .query_row("SELECT MIN(seq) FROM events", [], |r| r.get(0))?;
    if after > 0 && earliest.is_some_and(|first| after < first - 1) {
        return Err(Error::new(
            "EVENT_CURSOR_EXPIRED",
            "Event cursor has expired; refresh current state",
        ));
    }
    let mut q=store.connection.prepare("SELECT ev.seq,ev.event_kind,ev.workload_id,ev.execution_id,ev.payload_json,ev.created_at_us FROM events ev LEFT JOIN executions e ON e.id=ev.execution_id LEFT JOIN workloads w ON w.id=COALESCE(ev.workload_id,e.workload_id) WHERE ev.seq>?1 AND (?2 OR w.namespace IN(SELECT value FROM json_each(?3))) ORDER BY ev.seq LIMIT ?4")?;
    let mut items = q
        .query_map(
            params![after, wildcard(p), scopes(p)?, limit + 1],
            event_row,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut allowed = 0;
    let mut bytes = 0;
    for item in &items {
        let size = serde_json::to_vec(item)
            .map_err(|_| input("Cannot encode event"))?
            .len();
        if allowed >= usize::try_from(limit).unwrap_or(100) || bytes + size > 256 * 1024 {
            break;
        }
        allowed += 1;
        bytes += size;
    }
    let more = allowed < items.len();
    items.truncate(allowed);
    let next_cursor = if more {
        items.last().map(|v| v.seq.to_string())
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}
fn execution_events(store: &Store, id: &str) -> Result<Vec<EventDto>> {
    let mut q=store.connection.prepare("SELECT seq,event_kind,workload_id,execution_id,payload_json,created_at_us FROM events WHERE execution_id=?1 ORDER BY seq DESC LIMIT 50")?;
    let mut items = Vec::new();
    let mut bytes = 0;
    for item in q.query_map([id], event_row)? {
        let item = item?;
        let size = serde_json::to_vec(&item)
            .map_err(|_| input("Cannot encode diagnostic evidence"))?
            .len();
        if bytes + size > 256 * 1024 {
            break;
        }
        bytes += size;
        items.push(item);
    }
    Ok(items)
}

fn token_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TokenDto> {
    fn parse<T: serde::de::DeserializeOwned>(
        row: &rusqlite::Row<'_>,
        index: usize,
    ) -> rusqlite::Result<T> {
        let value: String = row.get(index)?;
        serde_json::from_str(&value).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    }
    Ok(TokenDto {
        id: row.get(0)?,
        actor_id: row.get(1)?,
        role: parse(row, 2)?,
        namespaces: parse(row, 3)?,
        capabilities: parse(row, 4)?,
        expires_at_us: row.get(5)?,
        revoked: row.get(6)?,
    })
}
fn token_authority(principal: &Principal, metadata: &TokenDto) -> Result<()> {
    for namespace in &metadata.namespaces {
        policy::namespace(principal, namespace)?;
    }
    for capability in &metadata.capabilities {
        let granted = if let Some((base, target)) = capability.split_once(':') {
            policy::has(principal, base, Some(target))
        } else {
            policy::has(principal, capability, None)
        };
        if !granted {
            return Err(policy::denied());
        }
    }
    Ok(())
}

fn probe_history(store: &Store, id: &str) -> Result<Vec<ProbeEventDto>> {
    let mut q=store.connection.prepare("SELECT kind,healthy,observed_at_us FROM service_probe_events WHERE workload_id=?1 ORDER BY seq DESC LIMIT 20")?;
    Ok(q.query_map([id], |r| {
        Ok(ProbeEventDto {
            kind: r.get(0)?,
            healthy: r.get(1)?,
            observed_at_us: r.get(2)?,
        })
    })?
    .collect::<std::result::Result<Vec<_>, _>>()?)
}
fn adapter_health(store: &Store) -> Result<Vec<AdapterHealthDto>> {
    let policy = crate::external::load_policy(&store.directory);
    let mut result = Vec::new();
    for adapter in ["process", "docker", "systemd"] {
        let enabled = if adapter == "process" {
            true
        } else {
            policy.as_ref().is_ok_and(|p| {
                if adapter == "docker" {
                    p.docker_enabled
                } else {
                    p.systemd_enabled
                }
            })
        };
        let mut state = if enabled {
            "UNVERIFIED"
        } else if policy.is_err() {
            "INVALID_POLICY"
        } else {
            "DISABLED"
        }
        .to_owned();
        let mut detail = if enabled {
            "No executor observation has been recorded"
        } else {
            "Executor is disabled by operator policy"
        }
        .to_owned();
        let mut last_observed_at_us = None;
        if let Some(stored) = store.setting(&format!("adapter.{adapter}"))? {
            let value: Value = serde_json::from_str(&stored)
                .map_err(|_| Error::new("STATE_CORRUPT", "Invalid executor observation"))?;
            last_observed_at_us = value["last_observed_at_us"].as_i64();
            if enabled {
                state = value["state"].as_str().unwrap_or("UNVERIFIED").into();
                detail = value["detail"]
                    .as_str()
                    .unwrap_or("Executor observation has no detail")
                    .into();
            }
        }
        result.push(AdapterHealthDto {
            adapter: adapter.into(),
            enabled,
            state,
            last_observed_at_us,
            detail,
        });
    }
    Ok(result)
}

fn command_context_changed(old: &Manifest, new: &Manifest) -> bool {
    serde_json::to_value(&old.spec.execution).ok() != serde_json::to_value(&new.spec.execution).ok()
        || old
            .spec
            .service
            .readiness
            .as_ref()
            .map(|p| (&p.command, &p.args))
            != new
                .spec
                .service
                .readiness
                .as_ref()
                .map(|p| (&p.command, &p.args))
        || old
            .spec
            .service
            .liveness
            .as_ref()
            .map(|p| (&p.command, &p.args))
            != new
                .spec
                .service
                .liveness
                .as_ref()
                .map(|p| (&p.command, &p.args))
}

fn bind_service_operation(
    store: &Store,
    operation: &str,
    workload: &str,
    goal: &str,
    revision: &str,
) -> Result<()> {
    store.connection.execute("UPDATE operations SET state='FAILED',error_code='OPERATION_SUPERSEDED',updated_at_us=?1 WHERE target_id=?2 AND id<>?3 AND operation_kind IN('service_desired','service_restart') AND state IN('ACCEPTED','RUNNING')",params![crate::now_us(),workload,operation])?;
    store.connection.execute(
        "UPDATE operations SET result_json=?1 WHERE id=?2",
        params![
            json!({"desired_state":goal,"revision_id":revision}).to_string(),
            operation
        ],
    )?;
    Ok(())
}

fn trailing_incomplete_utf8(bytes: &[u8]) -> Option<usize> {
    let mut offset = 0;
    loop {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(_) => return None,
            Err(error) => match error.error_len() {
                Some(length) => offset += error.valid_up_to() + length,
                None => return Some(offset + error.valid_up_to()),
            },
        }
    }
}
fn log_window(
    file: &mut File,
    offset: u64,
    max_bytes: u32,
    terminal: bool,
) -> Result<(Vec<u8>, u64, bool)> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| input("Invalid log offset"))?;
    let mut bytes = Vec::new();
    file.take(u64::from(max_bytes) + 4)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::new("LOG_UNAVAILABLE", "Cannot read log segment"))?;
    let cap = usize::try_from(max_bytes).unwrap_or(65536);
    let available = bytes.len();
    let mut consumed = available.min(cap);
    if max_bytes >= 4
        && (!terminal || available > cap)
        && let Some(start) = trailing_incomplete_utf8(&bytes[..consumed])
    {
        consumed = start;
    }
    let waiting = !terminal && trailing_incomplete_utf8(&bytes) == Some(consumed);
    let response_truncated = available > consumed && !waiting;
    bytes.truncate(consumed);
    let next = offset
        .checked_add(u64::try_from(consumed).map_err(|_| input("Invalid log offset"))?)
        .ok_or_else(|| input("Invalid log offset"))?;
    Ok((bytes, next, response_truncated))
}
