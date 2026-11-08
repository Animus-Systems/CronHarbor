use std::{
    fs::{self, File, OpenOptions, Permissions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    Error, Result,
    config::{GRACE_US, Manifest},
};

pub(crate) const MIGRATION2: &str = include_str!("../migrations/0002_runtime.sql");
pub(crate) const MIGRATION: &str = include_str!("../migrations/0001_initial.sql");
pub(crate) const SCHEMA_VERSION: u32 = 5;
pub(crate) const MIGRATIONS: &[(u32, &str)] = &[
    (1, MIGRATION),
    (2, MIGRATION2),
    (3, include_str!("../migrations/0003_auth_rejections.sql")),
    (4, include_str!("../migrations/0004_log_streams.sql")),
    (5, include_str!("../migrations/0005_alert_deliveries.sql")),
];
pub const MAX_ACTIVE: i64 = 4;

pub struct Store {
    pub(crate) connection: Connection,
    _owner: File,
    pub directory: PathBuf,
    last_auth_rejection: Option<Instant>,
    pub(crate) alert_policy: crate::alerts::AlertCapturePolicy,
    pub(crate) alert_worker_unavailable: bool,
}

#[derive(Debug, Clone)]
pub struct Launch {
    pub attempt_id: String,
    pub execution_id: String,
    pub launch_nonce: String,
    pub due_at_us: i64,
    pub manifest: Manifest,
}

#[derive(Debug, Serialize)]
pub struct History {
    pub execution_id: String,
    pub attempt_id: String,
    pub namespace: String,
    pub name: String,
    pub state: String,
    pub due_at_us: i64,
    pub error_code: Option<String>,
    pub exit_code: Option<i32>,
    pub log_truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub sqlite_version: String,
    pub workloads: i64,
    pub active: i64,
    pub unknown: i64,
    pub paused: bool,
}

fn id() -> String {
    Uuid::new_v4().to_string()
}

pub fn secure_file(path: &Path) -> Result<File> {
    check_existing(path)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|_| Error::new("STORAGE_UNAVAILABLE", "Cannot open private state file"))
}

fn check_existing(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta)
            if meta.is_file()
                && meta.uid() == nix::unistd::geteuid().as_raw()
                && meta.nlink() == 1
                && meta.mode() & 0o077 == 0 =>
        {
            Ok(())
        }
        Ok(_) => Err(Error::new(
            "UNSAFE_STATE_PATH",
            "State files must be private, owned regular files without hard links",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Error::new(
            "STORAGE_UNAVAILABLE",
            "Cannot inspect state file",
        )),
    }
}

pub fn private_directory(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|_| {
                    Error::new(
                        "STORAGE_UNAVAILABLE",
                        "Cannot create private state directory",
                    )
                })?;
        }
        Err(_) => {
            return Err(Error::new(
                "STORAGE_UNAVAILABLE",
                "Cannot inspect state directory",
            ));
        }
        Ok(_) => {}
    }
    let meta = fs::symlink_metadata(path)
        .map_err(|_| Error::new("STORAGE_UNAVAILABLE", "Cannot inspect state directory"))?;
    if !meta.is_dir() || meta.uid() != nix::unistd::geteuid().as_raw() || meta.mode() & 0o077 != 0 {
        return Err(Error::new(
            "UNSAFE_STATE_PATH",
            "State directory must be owned by the current user with mode 0700",
        ));
    }
    fs::canonicalize(path)
        .map_err(|_| Error::new("STORAGE_UNAVAILABLE", "Cannot resolve state directory"))
}

impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        Self::open_with_alerts(directory, None)
    }
    pub fn open_with_alerts(
        directory: &Path,
        alerts: Option<&crate::config::AlertConfig>,
    ) -> Result<Self> {
        let alert_policy = crate::alerts::AlertCapturePolicy::from_config(alerts)?;
        let directory = private_directory(directory)?;
        let owner = secure_file(&directory.join("owner.lock"))?;
        owner.try_lock().map_err(|_| {
            Error::new(
                "OWNER_UNAVAILABLE",
                "Another daemon owns this state directory, or locking is unavailable",
            )
        })?;
        match fs::symlink_metadata(directory.join("restore-installing")) {
            Ok(_) => {
                return Err(Error::new(
                    "RESTORE_REQUIRED",
                    "Restore installation is incomplete; preserve quarantine and finish the supported restore",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(Error::new(
                    "RESTORE_REQUIRED",
                    "Cannot verify restore installation state",
                ));
            }
        }
        let db_path = directory.join("state.db");
        let restore_marker = match fs::symlink_metadata(directory.join("restore-required.json")) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => {
                return Err(Error::new(
                    "RESTORE_REQUIRED",
                    "Cannot inspect restore quarantine marker",
                ));
            }
        };
        if restore_marker
            && !fs::metadata(&db_path)
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        {
            return Err(Error::new(
                "RESTORE_REQUIRED",
                "Restore quarantine requires an existing verified database; finish restore before startup",
            ));
        }

        for suffix in ["state.db", "state.db-wal", "state.db-shm"] {
            check_existing(&directory.join(suffix))?;
        }
        drop(secure_file(&db_path)?);
        let mut connection = Connection::open(&db_path)?;
        let version = rusqlite::version_number();
        if version < 3_051_003 && version != 3_044_006 && version != 3_050_007 {
            return Err(Error::new(
                "SQLITE_UNPATCHED",
                "SQLite runtime lacks a known WAL-reset fix",
            ));
        }
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;")?;
        let current: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if !(0..=i64::from(SCHEMA_VERSION)).contains(&current) {
            return Err(Error::new(
                "SCHEMA_UNSUPPORTED",
                "Database schema version is unsupported",
            ));
        }
        if current == 0 {
            let count:i64=connection.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",[],|r|r.get(0))?;
            if count != 0 {
                return Err(Error::new(
                    "SCHEMA_UNSUPPORTED",
                    "Refusing to adopt an unversioned database",
                ));
            }
        }
        for &(number, sql) in MIGRATIONS {
            let hash = format!("{:x}", Sha256::digest(sql.as_bytes()));
            if current < i64::from(number) {
                if current > 0 && i64::from(number) == current + 1 {
                    let backup =
                        directory.join(format!("pre-migration-{number}-{}.db", crate::now_us()));
                    connection.backup(rusqlite::MAIN_DB, &backup, rusqlite::backup::NO_PROGRESS)?;
                    fs::set_permissions(backup, Permissions::from_mode(0o600)).map_err(|_| {
                        Error::new("STORAGE_UNAVAILABLE", "Cannot secure migration backup")
                    })?;
                }
                let tx = connection.transaction()?;
                tx.execute_batch(sql)?;
                tx.execute("INSERT INTO schema_migrations(version,checksum,applied_at_us) VALUES(?1,?2,?3)",params![number,hash,crate::now_us()])?;
                tx.pragma_update(None, "user_version", number)?;
                tx.commit()?;
            } else {
                let stored: String = connection.query_row(
                    "SELECT checksum FROM schema_migrations WHERE version=?1",
                    [number],
                    |r| r.get(0),
                )?;
                if stored != hash {
                    return Err(Error::new(
                        "SCHEMA_UNSUPPORTED",
                        "Migration checksum differs; use the matching binary",
                    ));
                }
            }
        }
        let integrity: String = connection.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if integrity != "ok" || connection.prepare("PRAGMA foreign_key_check")?.exists([])? {
            return Err(Error::new(
                "STATE_CORRUPT",
                "Database integrity checks failed; dispatch is disabled",
            ));
        }
        let mut store = Self {
            connection,
            _owner: owner,
            directory,
            last_auth_rejection: None,
            alert_policy,
            alert_worker_unavailable: false,
        };
        if store.setting("instance_id")?.is_none() {
            store.set_setting("instance_id", &id())?;
        }
        if restore_marker {
            let marker = store.restore_marker()?;
            store.set_setting("restore_required", "true")?;
            store.set_setting("global_paused", "true")?;
            store.set_setting("restore_backup_at_us", &marker.backup_at_us.to_string())?;
            let bytes = fs::read(store.directory.join("restore-required.json")).map_err(|_| {
                Error::new(
                    "RESTORE_REQUIRED",
                    "Cannot inspect restore quarantine identity",
                )
            })?;
            let fingerprint = format!("{:x}", Sha256::digest(bytes));
            if store.setting("restore_token_invalidation")?.as_deref() != Some(&fingerprint) {
                let tx = store.connection.transaction()?;
                tx.execute(
                    "UPDATE api_tokens SET revoked_at_us=COALESCE(revoked_at_us,?1)",
                    [crate::now_us()],
                )?;
                tx.execute("INSERT INTO runtime_settings(key,value) VALUES('restore_token_invalidation',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[&fingerprint])?;
                audit_actor(
                    &tx,
                    "local-owner",
                    "restore.invalidate_tokens",
                    "instance",
                    "SUCCEEDED",
                    crate::now_us(),
                )?;
                tx.commit()?;
            }
        }
        store.reconcile()?;
        store.recover_alert_deliveries(crate::now_us())?;
        store.recover_log_streams()?;
        Ok(store)
    }
    pub(crate) fn record_auth_rejection(&mut self) -> Result<()> {
        let now = Instant::now();
        if self
            .last_auth_rejection
            .is_some_and(|at| now.duration_since(at) < Duration::from_secs(1))
        {
            return Ok(());
        }
        let tx = self.connection.transaction()?;
        audit_actor(
            &tx,
            "anonymous",
            "auth.reject",
            "token",
            "DENIED",
            crate::now_us(),
        )?;
        tx.execute(
            "DELETE FROM audit_events WHERE action='auth.reject' AND seq < COALESCE((SELECT seq FROM audit_events WHERE action='auth.reject' ORDER BY seq DESC LIMIT 1 OFFSET 999),0)",
            [],
        )?;
        tx.commit()?;
        self.last_auth_rejection = Some(now);
        Ok(())
    }
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM runtime_settings WHERE key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.connection.execute("INSERT INTO runtime_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
        Ok(())
    }
    pub fn paused(&self) -> Result<bool> {
        Ok(self.setting("global_paused")?.as_deref() == Some("true") || self.restore_required()?)
    }
    pub fn restore_required(&self) -> Result<bool> {
        Ok(self.setting("restore_required")?.as_deref() == Some("true")
            || self.directory.join("restore-required.json").exists())
    }
    pub fn instance_id(&self) -> Result<String> {
        self.setting("instance_id")?
            .ok_or_else(|| Error::new("STATE_CORRUPT", "Instance identity is missing"))
    }
    fn restore_marker(&self) -> Result<RestoreMarker> {
        let path = self.directory.join("restore-required.json");
        check_existing(&path)?;
        let value = fs::read(path)
            .map_err(|_| Error::new("RESTORE_REQUIRED", "Restore boundary marker is unreadable"))?;
        if value.len() > 8192 {
            return Err(Error::new(
                "RESTORE_REQUIRED",
                "Restore boundary marker is invalid",
            ));
        }
        serde_json::from_slice(&value)
            .map_err(|_| Error::new("RESTORE_REQUIRED", "Restore boundary marker is invalid"))
    }
    fn reconcile(&mut self) -> Result<()> {
        let now = crate::now_us();
        let restored = self.restore_required()?;
        let tx = self.connection.transaction()?;
        crate::alerts::recover_manual_deliveries(&tx, now)?;
        tx.execute("UPDATE attempts SET state='UNKNOWN',error_code='RECONCILIATION_REQUIRED',finished_at_us=NULL WHERE state IN ('START_REQUESTED','RUNNING')",[])?;
        if restored {
            tx.execute("UPDATE attempts SET state='UNKNOWN',error_code='RESTORE_RECONCILIATION_REQUIRED',finished_at_us=NULL WHERE state IN ('CREATED','LAUNCH_INTENT_RECORDED')",[])?;
        } else {
            tx.execute("UPDATE attempts SET state='CANCELLED',error_code='START_INTERRUPTED',finished_at_us=?1 WHERE state IN ('CREATED','LAUNCH_INTENT_RECORDED')",[now])?;
        }
        tx.execute("UPDATE executions SET state='UNKNOWN',finished_at_us=NULL WHERE id IN(SELECT execution_id FROM attempts WHERE state='UNKNOWN')",[])?;
        tx.execute("UPDATE executions SET state='CANCELLED',finished_at_us=?1 WHERE state IN('CREATED','RUNNING') AND id IN(SELECT execution_id FROM attempts WHERE state='CANCELLED' AND error_code='START_INTERRUPTED')",[now])?;
        tx.execute("UPDATE operations SET state='FAILED',error_code='START_INTERRUPTED',updated_at_us=?1 WHERE state IN('ACCEPTED','RUNNING') AND execution_id IN(SELECT e.id FROM executions e JOIN attempts a ON a.execution_id=e.id WHERE e.state='CANCELLED' AND a.state='CANCELLED' AND a.error_code='START_INTERRUPTED')",[now])?;
        tx.execute("UPDATE service_instances SET observed_state='ABSENT',next_action_at_us=NULL,last_observed_at_us=?1 WHERE observed_state='STARTING' AND workload_id IN(SELECT e.workload_id FROM executions e JOIN attempts a ON a.execution_id=e.id WHERE a.error_code='START_INTERRUPTED' AND a.state='CANCELLED') AND NOT EXISTS(SELECT 1 FROM executions e JOIN attempts a ON a.execution_id=e.id WHERE e.workload_id=service_instances.workload_id AND a.state IN('LAUNCH_INTENT_RECORDED','START_REQUESTED','RUNNING','UNKNOWN'))",[now])?;
        tx.execute("UPDATE service_instances SET observed_state='UNKNOWN',last_observed_at_us=?1 WHERE workload_id IN(SELECT workload_id FROM executions WHERE state='UNKNOWN')",[now])?;
        tx.execute("UPDATE operations SET state='UNKNOWN',error_code='RECONCILIATION_REQUIRED',updated_at_us=?1 WHERE state='RUNNING' AND execution_id IN(SELECT id FROM executions WHERE state='UNKNOWN')",[now])?;
        tx.execute("UPDATE operations SET state='FAILED',error_code='START_INTERRUPTED',updated_at_us=?1 WHERE state='ACCEPTED' AND id NOT IN(SELECT operation_id FROM manual_queue) AND operation_kind NOT IN('service_desired','service_restart','alert_retry')",[now])?;
        tx.execute("UPDATE operations SET state='UNKNOWN',error_code='RECONCILIATION_REQUIRED',updated_at_us=?1 WHERE state IN('ACCEPTED','RUNNING') AND operation_kind IN('service_desired','service_restart') AND target_id IN(SELECT workload_id FROM service_instances WHERE observed_state='UNKNOWN')",[now])?;
        audit(&tx, "recovery.inspect", "instance", now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn apply(&mut self, manifest: &Manifest, now: i64) -> Result<String> {
        self.apply_scoped(manifest, "FILE", None, "local-owner", now)
    }
    pub fn log_disk_limit(&self) -> Result<u64> {
        Ok(self.log_storage_policy()?.max_disk_bytes)
    }
    pub fn apply_scoped(
        &mut self,
        manifest: &Manifest,
        source: &str,
        expected: Option<&str>,
        actor: &str,
        now: i64,
    ) -> Result<String> {
        let (json, hash) = manifest.canonical()?;
        if !matches!(source, "FILE" | "API") || manifest.spec.owner.to_ascii_uppercase() != source {
            return Err(Error::new(
                "SOURCE_CONFLICT",
                "Manifest ownership must match its source",
            ));
        }
        let next_due = if manifest.kind == "ScheduledJob" {
            Some(manifest.next_after(now)?)
        } else {
            None
        };
        let old = self
            .manifest_for(&format!(
                "{}/{}",
                manifest.metadata.namespace, manifest.metadata.name
            ))
            .ok()
            .map(|row| row.2);
        crate::logs::validate_legacy_policy(old.as_ref(), manifest)?;
        let tx = self.connection.savepoint()?;
        let existing:Option<(String,String,Option<String>)>=tx.query_row("SELECT id,source_kind,current_revision_id FROM workloads WHERE namespace=?1 AND name=?2",params![manifest.metadata.namespace,manifest.metadata.name],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let workload = match existing {
            Some((workload, owned, current)) => {
                if source != owned {
                    return Err(Error::new(
                        "SOURCE_CONFLICT",
                        "Workload is owned by another definition source",
                    ));
                }
                let kind: String =
                    tx.query_row("SELECT kind FROM workloads WHERE id=?1", [&workload], |r| {
                        r.get(0)
                    })?;
                if kind != manifest.storage_kind() {
                    return Err(Error::new(
                        "STATE_CONFLICT",
                        "Workload kind is immutable; create a separate workload",
                    ));
                }
                if source == "API" && expected != current.as_deref() {
                    return Err(Error::new(
                        "REVISION_CONFLICT",
                        "Expected revision does not match current workload revision",
                    ));
                }
                if source == "FILE" && expected.is_some() && expected != current.as_deref() {
                    return Err(Error::new(
                        "REVISION_CONFLICT",
                        "Expected revision does not match current workload revision",
                    ));
                }
                if let Some(ref revision) = current {
                    let current_hash: String = tx.query_row(
                        "SELECT spec_hash FROM workload_revisions WHERE id=?1",
                        [revision],
                        |r| r.get(0),
                    )?;
                    if hash == current_hash {
                        return Ok(revision.clone());
                    }
                }
                workload
            }
            None => {
                if expected.is_some() {
                    return Err(Error::new(
                        "REVISION_CONFLICT",
                        "New workload cannot have an expected revision",
                    ));
                }
                let workload = id();
                tx.execute("INSERT INTO workloads(id,namespace,name,kind,source_kind,enabled,created_at_us) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![workload,manifest.metadata.namespace,manifest.metadata.name,manifest.storage_kind(),source,manifest.spec.enabled,now])?;
                workload
            }
        };
        let revision = id();
        let number: i64 = tx.query_row(
            "SELECT COALESCE(MAX(revision_no),0)+1 FROM workload_revisions WHERE workload_id=?1",
            [&workload],
            |r| r.get(0),
        )?;
        tx.execute("INSERT INTO workload_revisions(id,workload_id,revision_no,schema_version,spec_json,spec_hash,created_by,created_at_us) VALUES(?1,?2,?3,'cronharbor.dev/v1',?4,?5,?6,?7)",params![revision,workload,number,json,hash,actor,now])?;
        tx.execute(
            "UPDATE schedules SET active=0 WHERE workload_id=?1",
            [&workload],
        )?;
        tx.execute(
            "UPDATE workloads SET current_revision_id=?1,enabled=?2,kind=?3 WHERE id=?4",
            params![
                revision,
                manifest.spec.enabled,
                manifest.storage_kind(),
                workload
            ],
        )?;
        if let Some(next) = next_due {
            let s = &manifest.spec.schedule;
            tx.execute("INSERT INTO schedules(id,workload_id,revision_id,schedule_kind,expression,timezone,fold_policy,gap_policy,misfire_policy,lateness_grace_seconds,next_due_at_us,active) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,60,?10,?11)",params![id(),workload,revision,s.r#type,s.expression,s.timezone,s.fold,s.gap,s.misfire.policy,next,manifest.spec.enabled])?;
        }
        tx.execute("UPDATE operations SET state='FAILED',error_code='REVISION_SUPERSEDED',updated_at_us=?1 WHERE target_id=?2 AND operation_kind IN('service_desired','service_restart') AND state IN('ACCEPTED','RUNNING')",params![now,workload])?;
        if manifest.kind == "Service" {
            tx.execute("INSERT INTO service_instances(id,workload_id,revision_id,desired_state,observed_state,budget_started_at_us) VALUES(?1,?2,?3,?4,'ABSENT',?5) ON CONFLICT(workload_id) DO UPDATE SET revision_id=excluded.revision_id,restart_requested=1,observed_state=CASE WHEN service_instances.observed_state='FAILED' AND service_instances.desired_state='RUNNING' THEN 'ABSENT' ELSE service_instances.observed_state END",params![id(),workload,revision,manifest.spec.service.desired_state,now])?;
        }
        audit_actor(&tx, actor, "config.apply", &workload, "SUCCEEDED", now)?;
        event(
            &tx,
            "workload.applied",
            Some(&workload),
            None,
            serde_json::json!({"revision_id":revision}),
            now,
        )?;
        tx.commit()?;
        Ok(revision)
    }
    pub fn manifest_for(&self, workload: &str) -> Result<(String, String, Manifest)> {
        let row:Option<(String,String,String)>=self.connection.query_row("SELECT w.id,w.current_revision_id,r.spec_json FROM workloads w JOIN workload_revisions r ON r.id=w.current_revision_id WHERE w.id=?1 OR (w.namespace||'/'||w.name)=?1",[workload],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let (id, rev, json) =
            row.ok_or_else(|| Error::new("NOT_FOUND", "Workload does not exist"))?;
        Ok((id, rev, Manifest::parse(&json)?))
    }
    pub fn claim_due(&mut self, now: i64) -> Result<Option<Launch>> {
        if self.paused()? {
            return Ok(None);
        }
        let tx = self.connection.transaction()?;
        let active:i64=tx.query_row("SELECT count(*) FROM attempts WHERE state IN('LAUNCH_INTENT_RECORDED','START_REQUESTED','RUNNING','UNKNOWN')",[],|r|r.get(0))?;
        if active >= MAX_ACTIVE {
            return Ok(None);
        }
        if let Some(launch) = claim_retry(&tx, now)? {
            tx.commit()?;
            return Ok(Some(launch));
        }
        if let Some(launch) = claim_manual(&tx, now)? {
            tx.commit()?;
            return Ok(Some(launch));
        }
        if let Some(launch) = claim_service(&tx, now)? {
            tx.commit()?;
            return Ok(Some(launch));
        }
        let due:Option<(String,String,String,i64,String)>=tx.query_row("SELECT s.id,s.workload_id,s.revision_id,s.next_due_at_us,r.spec_json FROM schedules s JOIN workloads w ON w.id=s.workload_id JOIN workload_revisions r ON r.id=s.revision_id WHERE s.active=1 AND w.enabled=1 AND w.paused=0 AND s.next_due_at_us<=?1 ORDER BY s.next_due_at_us,s.id LIMIT 1",[now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        if let Some((schedule, workload, revision, due, json)) = due {
            let manifest = Manifest::parse(&json)?;
            let policy = &manifest.spec.schedule.misfire;
            let missed = now.saturating_sub(due) > GRACE_US;
            let next = if missed {
                match policy.policy.as_str() {
                    "skip" => {
                        manifest.next_after(now.saturating_sub(GRACE_US).saturating_sub(1))?
                    }
                    _ => manifest.next_after(now)?,
                }
            } else {
                manifest.next_after(due)?
            };
            let mut instants = Vec::new();
            if missed && policy.policy != "skip" {
                let mut cursor = now;
                for _ in 0..if policy.policy == "latest" {
                    1
                } else {
                    policy.max_occurrences
                } {
                    let value =
                        crate::scheduler::previous_at_or_before(&manifest.spec.schedule, cursor)?;
                    if value < due
                        || now.saturating_sub(value)
                            > i64::try_from(policy.max_age_seconds)
                                .unwrap_or(i64::MAX)
                                .saturating_mul(1_000_000)
                    {
                        break;
                    }
                    instants.push(value);
                    cursor = value.saturating_sub(1);
                }
                instants.reverse();
            } else if !missed {
                instants.push(due);
            }
            if missed {
                let payload = serde_json::json!({"fromDueUtcMicroseconds":due,"throughUtcMicroseconds":next.saturating_sub(1),"reason":"MISFIRE_RANGE_SKIPPED","admitted":instants.len()});
                event(
                    &tx,
                    "schedule.misfire_skipped",
                    Some(&workload),
                    None,
                    payload,
                    now,
                )?;
            }
            if instants.is_empty() {
                insert_occurrence(
                    &tx,
                    &schedule,
                    &workload,
                    &revision,
                    due,
                    "SKIPPED",
                    Some("MISFIRE_RANGE_SKIPPED"),
                    now,
                )?;
            }
            for instant in instants {
                insert_occurrence(
                    &tx, &schedule, &workload, &revision, instant, "QUEUED", None, now,
                )?;
            }
            tx.execute(
                "UPDATE schedules SET next_due_at_us=?1,last_materialized_at_us=?2 WHERE id=?3",
                params![next, now, schedule],
            )?;
        }
        let queued = {
            let mut q=tx.prepare("SELECT o.id,o.workload_id,o.revision_id,o.scheduled_at_us,r.spec_json FROM occurrences o JOIN workloads w ON w.id=o.workload_id JOIN workload_revisions r ON r.id=o.revision_id WHERE o.state='QUEUED' AND w.enabled=1 AND w.paused=0 ORDER BY o.scheduled_at_us,o.id LIMIT 256")?;
            q.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (occurrence, workload, revision, due, json) in queued {
            let manifest = Manifest::parse(&json)?;
            let policy = &manifest.spec.schedule.misfire;
            let age = now.saturating_sub(due);
            let expired = if policy.policy == "skip" {
                age > GRACE_US
            } else {
                age > i64::try_from(policy.max_age_seconds)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(1_000_000)
            };
            let count = workload_active(&tx, &workload)?;
            let reason = if expired {
                Some("MISFIRE_EXPIRED_AT_ADMISSION")
            } else if count >= i64::from(manifest.spec.concurrency.max_active)
                && manifest.spec.concurrency.mode == "forbid"
            {
                Some("OVERLAP_OR_UNKNOWN")
            } else {
                None
            };
            if let Some(reason) = reason {
                tx.execute(
                    "UPDATE occurrences SET state='SKIPPED',reason=?1 WHERE id=?2",
                    params![reason, occurrence],
                )?;
                continue;
            }
            if count >= i64::from(manifest.spec.concurrency.max_active) {
                continue;
            }
            tx.execute(
                "UPDATE occurrences SET state='DISPATCHED' WHERE id=?1",
                [&occurrence],
            )?;
            let launch = insert_launch(
                &tx,
                &workload,
                &revision,
                Some(&occurrence),
                "SCHEDULE",
                None,
                manifest,
                due,
                now,
            )?;
            tx.commit()?;
            return Ok(Some(launch));
        }
        tx.commit()?;
        Ok(None)
    }
    pub fn start_requested(&mut self, attempt: &str, now: i64) -> Result<()> {
        let tx = self.connection.transaction()?;
        let changed=tx.execute("UPDATE attempts SET state='START_REQUESTED' WHERE id=?1 AND state='LAUNCH_INTENT_RECORDED'",[attempt])?;
        if changed != 1 {
            return Err(Error::new(
                "STATE_CONFLICT",
                "Attempt cannot request a start",
            ));
        }
        audit(&tx, "execution.start_requested", attempt, now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn running(&mut self, launch: &Launch, pid: u32, now: i64) -> Result<()> {
        let handle = serde_json::json!({"pid":pid,"launchNonce":launch.launch_nonce});
        self.running_handle(launch, &handle.to_string(), now)
    }
    pub fn running_handle(&mut self, launch: &Launch, handle: &str, now: i64) -> Result<()> {
        let tx = self.connection.transaction()?;
        let changed=tx.execute("UPDATE attempts SET state='RUNNING',external_handle_json=?1,error_code=NULL,finished_at_us=NULL,started_at_us=COALESCE(started_at_us,?2) WHERE id=?3 AND state IN('START_REQUESTED','UNKNOWN')",params![handle,now,launch.attempt_id])?;
        if changed != 1 {
            return Err(Error::new(
                "STATE_CONFLICT",
                "Attempt cannot accept a launch receipt",
            ));
        }
        tx.execute(
            "UPDATE executions SET state='RUNNING' WHERE id=?1",
            [&launch.execution_id],
        )?;
        crate::logs::ensure_stream(&tx, launch, now)?;
        if launch.manifest.kind == "Service" {
            tx.execute("UPDATE service_instances SET observed_state='STARTING',external_handle_json=?1,last_observed_at_us=?2 WHERE workload_id=(SELECT workload_id FROM executions WHERE id=?3)",params![handle,now,launch.execution_id])?;
        }
        event(
            &tx,
            "execution.running",
            None,
            Some(&launch.execution_id),
            serde_json::json!({"attempt_id":launch.attempt_id}),
            now,
        )?;
        audit(&tx, "execution.running", &launch.attempt_id, now)?;
        tx.commit()?;
        self.record_adapter_observation(
            &launch.manifest.spec.execution.adapter,
            "AVAILABLE",
            "Executor returned a verified launch identity",
            now,
        )?;
        Ok(())
    }
    pub fn persist_external_intent(
        &self,
        attempt: &str,
        handle: &crate::external::ExternalHandle,
    ) -> Result<()> {
        let value = serde_json::to_string(handle)
            .map_err(|_| Error::new("INTERNAL_ERROR", "Cannot encode executor identity"))?;
        self.connection.execute("UPDATE attempts SET external_handle_json=?1 WHERE id=?2 AND state='LAUNCH_INTENT_RECORDED'",params![value,attempt])?;
        Ok(())
    }
    pub fn external_identity(&self, launch: &Launch) -> Result<crate::external::Identity> {
        let (workload, revision): (String, String) = self.connection.query_row(
            "SELECT workload_id,revision_id FROM executions WHERE id=?1",
            [&launch.execution_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(crate::external::Identity {
            instance_id: self.instance_id()?,
            workload_id: workload,
            revision_id: revision,
            execution_id: launch.execution_id.clone(),
            launch_nonce: launch.launch_nonce.clone(),
        })
    }
    pub fn finish(
        &mut self,
        launch: &Launch,
        outcome: &crate::process::Outcome,
        now: i64,
    ) -> Result<()> {
        let tx = self.connection.savepoint()?;
        let state = outcome.state.as_str();
        let finished = if state == "UNKNOWN" { None } else { Some(now) };
        let changed=tx.execute("UPDATE attempts SET state=?1,error_code=?2,exit_code=?3,finished_at_us=?4 WHERE id=?5 AND state IN('START_REQUESTED','RUNNING','UNKNOWN')",params![state,outcome.error_code,outcome.exit_code,finished,launch.attempt_id])?;
        if changed != 1 {
            return Err(Error::new(
                "STATE_CONFLICT",
                "Attempt cannot accept a result",
            ));
        }
        tx.execute(
            "UPDATE executions SET state=?1,finished_at_us=?2 WHERE id=?3",
            params![state, finished, launch.execution_id],
        )?;
        crate::logs::finish_stream(&tx, &launch.attempt_id, outcome, now)?;
        if launch.manifest.kind == "Service" {
            service_finished(&tx, launch, state, outcome.error_code, now)?;
        } else {
            let index: i64 = tx.query_row(
                "SELECT attempt_index FROM attempts WHERE id=?1",
                [&launch.attempt_id],
                |r| r.get(0),
            )?;
            let retry = &launch.manifest.spec.retry;
            let safe = state == "FAILED"
                && (retry.policy == "idempotent"
                    || (retry.policy == "start_rejected"
                        && outcome.error_code == Some("START_REJECTED")));
            if safe && index < i64::from(retry.max_attempts) {
                let next = now.saturating_add(
                    i64::try_from(retry.backoff_seconds)
                        .unwrap_or(i64::MAX)
                        .saturating_mul(1_000_000),
                );
                tx.execute("INSERT INTO retry_queue(execution_id,attempt_index,due_at_us) VALUES(?1,?2,?3)",params![launch.execution_id,index+1,next])?;
                tx.execute(
                    "UPDATE executions SET state='CREATED',finished_at_us=NULL WHERE id=?1",
                    [&launch.execution_id],
                )?;
            } else {
                tx.execute("UPDATE operations SET state=?1,error_code=?2,updated_at_us=?3 WHERE execution_id=?4",params![if state=="SUCCEEDED"{"SUCCEEDED"}else if state=="UNKNOWN"{"UNKNOWN"}else{"FAILED"},outcome.error_code,now,launch.execution_id])?;
            }
        }
        tx.execute("UPDATE operations SET state=?1,error_code=?2,updated_at_us=?3 WHERE target_id=?4 AND operation_kind='cancel' AND state IN('ACCEPTED','RUNNING')",params![if state=="UNKNOWN"{"UNKNOWN"}else{"SUCCEEDED"},outcome.error_code,now,launch.execution_id])?;
        crate::alerts::capture_result(&tx, &self.alert_policy, &launch.attempt_id, now)?;
        event(
            &tx,
            "execution.result",
            None,
            Some(&launch.execution_id),
            serde_json::json!({"state":state,"error_code":outcome.error_code}),
            now,
        )?;
        audit(&tx, "execution.result", &launch.attempt_id, now)?;
        tx.commit()?;
        let adapter_state = if state == "UNKNOWN" {
            "UNAVAILABLE"
        } else if outcome.error_code == Some("START_REJECTED") {
            "REJECTED"
        } else {
            "AVAILABLE"
        };
        self.record_adapter_observation(
            &launch.manifest.spec.execution.adapter,
            adapter_state,
            if state == "UNKNOWN" {
                "Executor observation is uncertain"
            } else {
                "Latest attempt produced a verified result or rejection"
            },
            now,
        )?;
        if self.connection.is_autocommit() {
            self.retain_logs_at(now)?;
        }
        Ok(())
    }
    pub fn attempt_is_unknown(&self, attempt: &str) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT state='UNKNOWN' FROM attempts WHERE id=?1",
            [attempt],
            |r| r.get(0),
        )?)
    }
    pub(crate) fn attempt_is_observable(&self, attempt: &str) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT state IN('START_REQUESTED','RUNNING','UNKNOWN') FROM attempts WHERE id=?1",
            [attempt],
            |r| r.get(0),
        )?)
    }
    pub fn attempt_started_at(&self, attempt: &str) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT COALESCE(started_at_us,created_at_us) FROM attempts WHERE id=?1",
            [attempt],
            |r| r.get(0),
        )?)
    }
    pub fn record_probe_events(
        &mut self,
        launch: &Launch,
        events: &[crate::contracts::ProbeEventDto],
    ) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let tx = self.connection.transaction()?;
        let workload: String = tx.query_row(
            "SELECT workload_id FROM executions WHERE id=?1",
            [&launch.execution_id],
            |r| r.get(0),
        )?;
        for observed in events {
            tx.execute("INSERT INTO service_probe_events(workload_id,execution_id,kind,healthy,observed_at_us) VALUES(?1,?2,?3,?4,?5)",params![workload,launch.execution_id,observed.kind,observed.healthy,observed.observed_at_us])?;
            event(
                &tx,
                "service.probe",
                Some(&workload),
                Some(&launch.execution_id),
                serde_json::json!({"kind":observed.kind,"healthy":observed.healthy}),
                observed.observed_at_us,
            )?;
        }
        tx.execute("DELETE FROM service_probe_events WHERE workload_id=?1 AND seq NOT IN(SELECT seq FROM service_probe_events WHERE workload_id=?1 ORDER BY seq DESC LIMIT 20)",[&workload])?;
        tx.commit()?;
        Ok(())
    }
    pub fn record_adapter_observation(
        &self,
        adapter: &str,
        state: &str,
        detail: &str,
        now: i64,
    ) -> Result<()> {
        let value = serde_json::json!({"state":state,"detail":detail,"last_observed_at_us":now});
        self.set_setting(&format!("adapter.{adapter}"), &value.to_string())
    }
    pub fn service_observation(&mut self, launch: &Launch, state: &str, now: i64) -> Result<()> {
        if launch.manifest.kind != "Service" {
            return Ok(());
        }
        let tx = self.connection.transaction()?;
        let previous:String=tx.query_row("SELECT observed_state FROM service_instances WHERE workload_id=(SELECT workload_id FROM executions WHERE id=?1)",[&launch.execution_id],|r|r.get(0))?;
        if previous != state {
            let workload: String = tx.query_row(
                "SELECT workload_id FROM executions WHERE id=?1",
                [&launch.execution_id],
                |r| r.get(0),
            )?;
            tx.execute("UPDATE service_instances SET observed_state=?1,last_observed_at_us=?2,ready_at_us=CASE WHEN ?1='READY' THEN COALESCE(ready_at_us,?2) ELSE NULL END WHERE workload_id=?3",params![state,now,workload])?;
            event(
                &tx,
                "service.observed",
                Some(&workload),
                Some(&launch.execution_id),
                serde_json::json!({"observed_state":state,"readiness_probe_configured":launch.manifest.spec.service.readiness.is_some()}),
                now,
            )?;
            if state == "READY" {
                tx.execute("UPDATE operations SET state='SUCCEEDED',updated_at_us=?1 WHERE target_id=?2 AND operation_kind IN('service_desired','service_restart') AND state IN('ACCEPTED','RUNNING') AND json_extract(result_json,'$.desired_state')='RUNNING' AND json_extract(result_json,'$.revision_id')=(SELECT revision_id FROM executions WHERE id=?3) AND (operation_kind!='service_restart' OR created_at_us<=(SELECT created_at_us FROM executions WHERE id=?3))",params![now,workload,launch.execution_id])?;
            }
        }
        let workload: String = tx.query_row(
            "SELECT workload_id FROM executions WHERE id=?1",
            [&launch.execution_id],
            |r| r.get(0),
        )?;
        crate::alerts::capture_service(&tx, &self.alert_policy, &workload, now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn service_should_stop(&self, launch: &Launch) -> Result<bool> {
        if launch.manifest.kind != "Service" {
            return Ok(false);
        }
        Ok(self.connection.query_row("SELECT si.desired_state='STOPPED' OR si.restart_requested=1 OR si.revision_id!=e.revision_id FROM service_instances si JOIN executions e ON e.workload_id=si.workload_id WHERE e.id=?1",[&launch.execution_id],|r|r.get(0))?)
    }
    pub fn finish_external(
        &mut self,
        launch: &Launch,
        outcome: &crate::process::Outcome,
        logs_persisted: bool,
        handle: &crate::external::ExternalHandle,
        now: i64,
    ) -> Result<()> {
        self.connection.execute_batch("SAVEPOINT external_result")?;
        let result = (|| {
            crate::logs::ensure_stream(&self.connection, launch, now)?;
            self.finish(launch, outcome, now)?;
            if logs_persisted && outcome.state != crate::process::EndState::Unknown {
                self.queue_external_cleanup(&launch.attempt_id, handle)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.connection.execute_batch("RELEASE external_result")?;
                self.retain_logs_at(now)
            }
            Err(error) => {
                self.connection
                    .execute_batch("ROLLBACK TO external_result; RELEASE external_result")?;
                Err(error)
            }
        }
    }
    pub fn queue_external_cleanup(
        &self,
        attempt: &str,
        handle: &crate::external::ExternalHandle,
    ) -> Result<()> {
        if !matches!(handle, crate::external::ExternalHandle::Docker { .. }) {
            return Ok(());
        }
        let value = serde_json::to_string(handle)
            .map_err(|_| Error::new("STATE_CORRUPT", "Invalid cleanup identity"))?;
        self.connection.execute("INSERT OR IGNORE INTO external_cleanup(attempt_id,handle_json,due_at_us) VALUES(?1,?2,?3)",params![attempt,value,crate::now_us()])?;
        Ok(())
    }
    pub fn next_external_cleanup(
        &self,
    ) -> Result<Option<(String, crate::external::ExternalHandle)>> {
        let row:Option<(String,String)>=self.connection.query_row("SELECT attempt_id,handle_json FROM external_cleanup WHERE due_at_us<=?1 ORDER BY due_at_us LIMIT 1",[crate::now_us()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        row.map(|(attempt, value)| {
            serde_json::from_str(&value)
                .map(|handle| (attempt, handle))
                .map_err(|_| Error::new("STATE_CORRUPT", "Invalid cleanup identity"))
        })
        .transpose()
    }
    pub fn finish_external_cleanup(&self, attempt: &str, success: bool) -> Result<()> {
        if success {
            self.connection.execute(
                "DELETE FROM external_cleanup WHERE attempt_id=?1",
                [attempt],
            )?;
        } else {
            self.connection.execute(
                "UPDATE external_cleanup SET failures=failures+1,due_at_us=?1 WHERE attempt_id=?2",
                params![crate::now_us().saturating_add(60_000_000), attempt],
            )?;
        }
        Ok(())
    }
    pub fn recoverable_external(&self) -> Result<Vec<(Launch, crate::external::ExternalHandle)>> {
        let mut q=self.connection.prepare("SELECT a.id,e.id,a.launch_nonce,COALESCE(o.scheduled_at_us,e.created_at_us),r.spec_json,a.external_handle_json FROM attempts a JOIN executions e ON e.id=a.execution_id JOIN workload_revisions r ON r.id=e.revision_id LEFT JOIN occurrences o ON o.id=e.occurrence_id WHERE a.state='UNKNOWN' AND a.executor_kind IN('docker','systemd') AND a.external_handle_json IS NOT NULL ORDER BY a.created_at_us LIMIT 64")?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut result = Vec::new();
        for (attempt_id, execution_id, launch_nonce, due_at_us, json, handle) in rows {
            let launch = Launch {
                attempt_id,
                execution_id,
                launch_nonce,
                due_at_us,
                manifest: Manifest::parse(&json)?,
            };
            let handle: crate::external::ExternalHandle = serde_json::from_str(&handle)
                .map_err(|_| Error::new("STATE_CORRUPT", "Invalid external identity"))?;
            if matches!(handle, crate::external::ExternalHandle::Docker { .. }) {
                let expected = crate::external::docker_handle(&self.external_identity(&launch)?);
                if serde_json::to_value(&expected).ok() != serde_json::to_value(&handle).ok() {
                    continue;
                }
            }
            result.push((launch, handle));
        }
        Ok(result)
    }
    pub fn cancel_requested(&self, attempt: &str) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT cancel_requested FROM attempts WHERE id=?1",
            [attempt],
            |r| r.get(0),
        )?)
    }
    pub fn history(&self) -> Result<Vec<History>> {
        let mut q=self.connection.prepare("SELECT e.id,a.id,w.namespace,w.name,a.state,COALESCE(o.scheduled_at_us,e.created_at_us),a.error_code,a.exit_code,COALESCE(l.truncated,0) FROM executions e JOIN attempts a ON a.execution_id=e.id JOIN workloads w ON w.id=e.workload_id LEFT JOIN occurrences o ON o.id=e.occurrence_id LEFT JOIN log_streams l ON l.attempt_id=a.id ORDER BY e.created_at_us DESC,e.id DESC,a.attempt_index DESC LIMIT 100")?;
        Ok(q.query_map([], |r| {
            Ok(History {
                execution_id: r.get(0)?,
                attempt_id: r.get(1)?,
                namespace: r.get(2)?,
                name: r.get(3)?,
                state: r.get(4)?,
                due_at_us: r.get(5)?,
                error_code: r.get(6)?,
                exit_code: r.get(7)?,
                log_truncated: r.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn status(&self, paused: bool) -> Result<Status> {
        Ok(Status{sqlite_version:rusqlite::version().into(),workloads:self.connection.query_row("SELECT count(*) FROM workloads",[],|r|r.get(0))?,active:self.connection.query_row("SELECT count(*) FROM attempts WHERE state IN('LAUNCH_INTENT_RECORDED','START_REQUESTED','RUNNING','UNKNOWN')",[],|r|r.get(0))?,unknown:self.connection.query_row("SELECT count(*) FROM attempts WHERE state='UNKNOWN'",[],|r|r.get(0))?,paused:paused||self.paused()?})
    }
    pub fn log_path(&self, attempt: &str) -> Result<PathBuf> {
        self.legacy_log_path(attempt)
    }
    pub fn retain_logs(&self) -> Result<()> {
        self.retain_logs_at(crate::now_us())
    }
    fn retain_logs_at(&self, now: i64) -> Result<()> {
        self.retain_log_segments(now, 0, 0)
    }
    pub fn backup(&mut self, name: &str) -> Result<serde_json::Value> {
        let result = crate::ops::backup(
            &self.connection,
            &self.directory,
            name,
            &self.instance_id()?,
        )?;
        serde_json::to_value(result)
            .map_err(|_| Error::new("INTERNAL_ERROR", "Cannot encode backup result"))
    }
    pub fn restore_resume(&mut self, cutoff: i64, evidence: &str, actor: &str) -> Result<()> {
        if !self.restore_required()? {
            return Err(Error::new(
                "STATE_CONFLICT",
                "Restore reconciliation is not pending",
            ));
        }
        if evidence.trim().is_empty() || evidence.len() > 4096 {
            return Err(Error::new(
                "INPUT_INVALID",
                "Restore reconciliation requires bounded operator evidence",
            ));
        }
        let marker = self.restore_marker()?;
        if cutoff < marker.backup_at_us
            || cutoff > crate::now_us()
            || marker.cutoff_us.is_some_and(|expected| cutoff != expected)
        {
            return Err(Error::new(
                "RESTORE_REQUIRED",
                "Known exclusive-owner cutoff must match the restore boundary",
            ));
        }
        let schedules = {
            let mut q=self.connection.prepare("SELECT s.id,s.next_due_at_us,r.spec_json FROM schedules s JOIN workload_revisions r ON r.id=s.revision_id")?;
            q.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let tx = self.connection.transaction()?;
        for (id, current, json) in schedules {
            let manifest = Manifest::parse(&json)?;
            let next = manifest.next_after(cutoff)?;
            tx.execute(
                "UPDATE schedules SET next_due_at_us=?1,last_materialized_at_us=?2 WHERE id=?3",
                params![current.map_or(next, |old| old.max(next)), cutoff, id],
            )?;
        }
        tx.execute("UPDATE occurrences SET state='SKIPPED',reason='RESTORE_QUARANTINE' WHERE state IN('DUE','QUEUED','BLOCKED') AND scheduled_at_us<=?1",[cutoff])?;
        tx.execute("UPDATE operations SET state='FAILED',error_code='RESTORE_QUARANTINE',updated_at_us=?1 WHERE id IN(SELECT operation_id FROM manual_queue)",[crate::now_us()])?;
        tx.execute("DELETE FROM manual_queue", [])?;
        tx.execute("UPDATE operations SET state='FAILED',error_code='RESTORE_QUARANTINE',updated_at_us=?1 WHERE state IN('ACCEPTED','RUNNING') AND execution_id IN(SELECT execution_id FROM retry_queue)",[crate::now_us()])?;
        tx.execute("UPDATE executions SET state='CANCELLED',finished_at_us=?1 WHERE state='CREATED' AND id IN(SELECT execution_id FROM retry_queue)",[crate::now_us()])?;
        tx.execute("DELETE FROM retry_queue", [])?;
        let unknown: i64 = tx.query_row(
            "SELECT count(*) FROM attempts WHERE state='UNKNOWN'",
            [],
            |r| r.get(0),
        )?;
        tx.execute("INSERT INTO restore_quarantine(id,backup_at_us,cutoff_us,evidence,unresolved_attempts,created_at_us) VALUES(?1,?2,?3,?4,?5,?6)",params![id(),marker.backup_at_us,cutoff,evidence,unknown,crate::now_us()])?;
        tx.execute("UPDATE runtime_settings SET value='false' WHERE key IN('restore_required','global_paused')",[])?;
        Self::suppress_restored_alerts(&tx, crate::now_us())?;
        audit_actor(
            &tx,
            actor,
            "restore.resume",
            "instance",
            "SUCCEEDED",
            crate::now_us(),
        )?;
        event(
            &tx,
            "restore.resumed",
            None,
            None,
            serde_json::json!({"backup_at_us":marker.backup_at_us,"cutoff_us":cutoff,"unresolved_attempts":unknown}),
            crate::now_us(),
        )?;
        tx.commit()?;
        fs::remove_file(self.directory.join("restore-required.json"))
            .map_err(|_| Error::new("RESTORE_REQUIRED", "Cannot clear restore boundary marker"))?;
        File::open(&self.directory)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::new("STORAGE_UNAVAILABLE", "Cannot sync restore reconciliation"))?;
        Ok(())
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreMarker {
    #[serde(default)]
    #[allow(dead_code)]
    restore_id: Option<String>,
    backup_at_us: i64,
    cutoff_us: Option<i64>,
    #[allow(dead_code)]
    evidence: String,
}
fn workload_active(tx: &Transaction<'_>, workload: &str) -> Result<i64> {
    Ok(tx.query_row("SELECT count(*) FROM attempts a JOIN executions e ON e.id=a.execution_id WHERE e.workload_id=?1 AND a.state IN('LAUNCH_INTENT_RECORDED','START_REQUESTED','RUNNING','UNKNOWN')",[workload],|r|r.get(0))?)
}
#[allow(clippy::too_many_arguments)]
fn insert_occurrence(
    tx: &Transaction<'_>,
    schedule: &str,
    workload: &str,
    revision: &str,
    due: i64,
    state: &str,
    reason: Option<&str>,
    now: i64,
) -> Result<()> {
    tx.execute("INSERT OR IGNORE INTO occurrences(id,schedule_id,workload_id,revision_id,scheduled_at_us,state,reason,created_at_us) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![id(),schedule,workload,revision,due,state,reason,now])?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn insert_launch(
    tx: &Transaction<'_>,
    workload: &str,
    revision: &str,
    occurrence: Option<&str>,
    trigger: &str,
    actor: Option<&str>,
    manifest: Manifest,
    due: i64,
    now: i64,
) -> Result<Launch> {
    let execution = id();
    let attempt = id();
    let nonce = id();
    tx.execute("INSERT INTO executions(id,workload_id,revision_id,occurrence_id,trigger_kind,actor_id,created_at_us,state) VALUES(?1,?2,?3,?4,?5,?6,?7,'CREATED')",params![execution,workload,revision,occurrence,trigger,actor,now])?;
    tx.execute("INSERT INTO attempts(id,execution_id,attempt_index,launch_nonce,state,executor_kind,created_at_us) VALUES(?1,?2,1,?3,'LAUNCH_INTENT_RECORDED',?4,?5)",params![attempt,execution,nonce,manifest.spec.execution.adapter,now])?;
    audit(tx, "execution.launch_intent", &attempt, now)?;
    event(
        tx,
        "execution.intent",
        Some(workload),
        Some(&execution),
        serde_json::json!({"attempt_id":attempt}),
        now,
    )?;
    Ok(Launch {
        attempt_id: attempt,
        execution_id: execution,
        launch_nonce: nonce,
        due_at_us: due,
        manifest,
    })
}
fn claim_retry(tx: &Transaction<'_>, now: i64) -> Result<Option<Launch>> {
    let row:Option<(String,i64,String,String,String)>=tx.query_row("SELECT q.execution_id,q.attempt_index,e.workload_id,e.revision_id,r.spec_json FROM retry_queue q JOIN executions e ON e.id=q.execution_id JOIN workloads w ON w.id=e.workload_id JOIN workload_revisions r ON r.id=e.revision_id WHERE q.due_at_us<=?1 AND w.enabled=1 AND w.paused=0 AND (SELECT count(*) FROM attempts a JOIN executions active ON active.id=a.execution_id WHERE active.workload_id=e.workload_id AND a.state IN('LAUNCH_INTENT_RECORDED','START_REQUESTED','RUNNING','UNKNOWN')) < json_extract(r.spec_json,'$.spec.concurrency.maxActive') ORDER BY q.due_at_us,q.execution_id LIMIT 1",[now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((execution, index, workload, _revision, json)) = row else {
        return Ok(None);
    };
    let manifest = Manifest::parse(&json)?;
    if workload_active(tx, &workload)? >= i64::from(manifest.spec.concurrency.max_active) {
        return Ok(None);
    }
    let attempt = id();
    let nonce = id();
    tx.execute(
        "DELETE FROM retry_queue WHERE execution_id=?1",
        [&execution],
    )?;
    tx.execute("INSERT INTO attempts(id,execution_id,attempt_index,launch_nonce,state,executor_kind,created_at_us) VALUES(?1,?2,?3,?4,'LAUNCH_INTENT_RECORDED',?5,?6)",params![attempt,execution,index,nonce,manifest.spec.execution.adapter,now])?;
    Ok(Some(Launch {
        attempt_id: attempt,
        execution_id: execution,
        launch_nonce: nonce,
        due_at_us: now,
        manifest,
    }))
}
fn claim_manual(tx: &Transaction<'_>, now: i64) -> Result<Option<Launch>> {
    tx.execute("UPDATE operations SET state='FAILED',error_code='QUEUE_EXPIRED',updated_at_us=?1 WHERE id IN(SELECT operation_id FROM manual_queue WHERE expires_at_us<?1)",[now])?;
    tx.execute("DELETE FROM manual_queue WHERE expires_at_us<?1", [now])?;
    let rows = {
        let mut q=tx.prepare("SELECT q.operation_id,q.workload_id,q.revision_id,q.actor_id,r.spec_json FROM manual_queue q JOIN workloads w ON w.id=q.workload_id JOIN workload_revisions r ON r.id=q.revision_id WHERE w.enabled=1 AND w.paused=0 ORDER BY q.created_at_us LIMIT 256")?;
        q.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (operation, workload, revision, actor, json) in rows {
        let manifest = Manifest::parse(&json)?;
        if workload_active(tx, &workload)? >= i64::from(manifest.spec.concurrency.max_active) {
            continue;
        }
        let launch = insert_launch(
            tx,
            &workload,
            &revision,
            None,
            "MANUAL",
            Some(&actor),
            manifest,
            now,
            now,
        )?;
        tx.execute(
            "DELETE FROM manual_queue WHERE operation_id=?1",
            [&operation],
        )?;
        tx.execute(
            "UPDATE operations SET state='RUNNING',execution_id=?1,updated_at_us=?2 WHERE id=?3",
            params![launch.execution_id, now, operation],
        )?;
        return Ok(Some(launch));
    }
    Ok(None)
}
fn claim_service(tx: &Transaction<'_>, now: i64) -> Result<Option<Launch>> {
    let rows = {
        let mut q=tx.prepare("SELECT si.workload_id,si.revision_id,r.spec_json FROM service_instances si JOIN workloads w ON w.id=si.workload_id JOIN workload_revisions r ON r.id=si.revision_id WHERE si.desired_state='RUNNING' AND si.observed_state IN('ABSENT','BACKOFF') AND (si.next_action_at_us IS NULL OR si.next_action_at_us<=?1) AND w.enabled=1 AND w.paused=0 ORDER BY si.next_action_at_us LIMIT 64")?;
        q.query_map([now], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (workload, revision, json) in rows {
        if workload_active(tx, &workload)? > 0 {
            continue;
        }
        let manifest = Manifest::parse(&json)?;
        let launch = insert_launch(
            tx, &workload, &revision, None, "INTERNAL", None, manifest, now, now,
        )?;
        tx.execute("UPDATE service_instances SET observed_state='STARTING',restart_requested=0 WHERE workload_id=?1",[workload])?;
        return Ok(Some(launch));
    }
    Ok(None)
}
fn service_finished(
    tx: &Connection,
    launch: &Launch,
    state: &str,
    error_code: Option<&str>,
    now: i64,
) -> Result<()> {
    let (workload,desired,count,window,ready,restart):(String,String,u32,Option<i64>,Option<i64>,bool)=tx.query_row("SELECT si.workload_id,si.desired_state,si.restart_count,si.budget_started_at_us,si.ready_at_us,si.restart_requested FROM service_instances si JOIN executions e ON e.workload_id=si.workload_id WHERE e.id=?1",[&launch.execution_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    let svc = &launch.manifest.spec.service;
    let reset = window.is_none_or(|at| {
        now.saturating_sub(at)
            >= i64::try_from(svc.window_seconds)
                .unwrap_or(i64::MAX)
                .saturating_mul(1_000_000)
    }) || ready.is_some_and(|at| {
        now.saturating_sub(at)
            >= i64::try_from(svc.reset_after_seconds)
                .unwrap_or(i64::MAX)
                .saturating_mul(1_000_000)
    });
    let count = if reset { 0 } else { count };
    let owner_shutdown = state == "CANCELLED" && error_code == Some("DAEMON_SHUTDOWN");
    let should = !owner_shutdown
        && state != "UNKNOWN"
        && desired == "RUNNING"
        && (restart
            || svc.restart == "always"
            || (svc.restart == "on-failure" && state != "SUCCEEDED" && state != "CANCELLED"));
    let next_count = if should && !restart && count < svc.max_restarts {
        count.saturating_add(1)
    } else {
        count
    };
    let observed = if state == "UNKNOWN" {
        "UNKNOWN"
    } else if owner_shutdown {
        "ABSENT"
    } else if should && (restart || count < svc.max_restarts) {
        "BACKOFF"
    } else if desired == "RUNNING" {
        "FAILED"
    } else {
        "ABSENT"
    };
    let delay = svc
        .backoff_seconds
        .saturating_mul(1_u64.checked_shl(count.min(20)).unwrap_or(u64::MAX))
        .min(svc.max_backoff_seconds);
    let jitter = u64::from(Sha256::digest(launch.attempt_id.as_bytes())[0])
        * delay.saturating_mul(1_000_000)
        / 1024;
    let next = now.saturating_add(
        i64::try_from(delay.saturating_mul(1_000_000).saturating_add(jitter)).unwrap_or(i64::MAX),
    );
    tx.execute("UPDATE service_instances SET observed_state=?1,restart_count=?2,budget_started_at_us=?3,next_action_at_us=?4,ready_at_us=NULL,consecutive_failures=0,consecutive_successes=0,restart_reason=?5,last_observed_at_us=?6 WHERE workload_id=?7",params![observed,next_count,if reset{Some(now)}else{window},if observed=="BACKOFF"{Some(next)}else{None},state,now,workload])?;
    if observed == "FAILED" {
        tx.execute("UPDATE operations SET state='FAILED',error_code=?1,updated_at_us=?2 WHERE target_id=?3 AND operation_kind IN('service_desired','service_restart') AND state IN('ACCEPTED','RUNNING') AND json_extract(result_json,'$.desired_state')='RUNNING' AND json_extract(result_json,'$.revision_id')=(SELECT revision_id FROM service_instances WHERE workload_id=?3)",params![error_code.unwrap_or("SERVICE_NOT_READY"),now,workload])?;
    }
    tx.execute("UPDATE operations SET state=?1,error_code=?2,updated_at_us=?3 WHERE target_id=?4 AND operation_kind IN('service_desired','service_restart') AND state IN('ACCEPTED','RUNNING') AND (?1='UNKNOWN' OR (?5='STOPPED' AND json_extract(result_json,'$.desired_state')='STOPPED'))",params![if state=="UNKNOWN"{"UNKNOWN"}else{"SUCCEEDED"},if state=="UNKNOWN"{Some("RECONCILIATION_REQUIRED")}else{None},now,workload,desired])?;
    Ok(())
}
pub(crate) fn event(
    tx: &Connection,
    kind: &str,
    workload: Option<&str>,
    execution: Option<&str>,
    payload: serde_json::Value,
    now: i64,
) -> Result<()> {
    tx.execute("INSERT INTO events(event_kind,workload_id,execution_id,payload_json,correlation_id,created_at_us) VALUES(?1,?2,?3,?4,?5,?6)",params![kind,workload,execution,payload.to_string(),id(),now])?;
    tx.execute(
        "DELETE FROM events WHERE seq <= (SELECT COALESCE(MAX(seq),0)-10000 FROM events)",
        [],
    )?;
    Ok(())
}
pub(crate) fn audit_actor(
    tx: &Connection,
    actor: &str,
    action: &str,
    target: &str,
    result: &str,
    now: i64,
) -> Result<()> {
    tx.execute("INSERT INTO audit_events(actor_id,action,target_id,result,request_id,created_at_us) VALUES(?1,?2,?3,?4,?5,?6)",params![actor,action,target,result,id(),now])?;
    Ok(())
}
fn audit(tx: &Connection, action: &str, target: &str, now: i64) -> Result<()> {
    audit_actor(tx, "local-owner", action, target, "SUCCEEDED", now)
}
pub fn protect_socket(path: &Path) -> Result<()> {
    fs::set_permissions(path, Permissions::from_mode(0o600)).map_err(|_| {
        Error::new(
            "LOCAL_TRANSPORT_UNAVAILABLE",
            "Cannot secure local control socket",
        )
    })
}
