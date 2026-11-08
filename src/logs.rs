//! Daemon-owned logical log streams. Pipe readers submit redacted bytes; only this owner mutates files.
use crate::{
    Error, Result,
    config::Manifest,
    contracts::*,
    process::Outcome,
    store::{Launch, Store, private_directory},
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub const MAX_OFFSET: u64 = 9_007_199_254_740_991;
fn unavailable() -> Error {
    Error::new("LOG_UNAVAILABLE", "Retained log evidence is unavailable")
}
fn invalid(message: &'static str) -> Error {
    Error::new("INPUT_INVALID", message)
}
fn durable(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| invalid("Log value exceeds durable range"))
}
fn nonnegative(value: i64) -> Result<u64> {
    u64::try_from(value)
        .ok()
        .filter(|v| *v <= MAX_OFFSET)
        .ok_or_else(|| Error::new("STATE_CORRUPT", "Invalid log counter"))
}
fn decode<T: serde::de::DeserializeOwned>(value: String) -> Result<T> {
    serde_json::from_value(serde_json::Value::String(value))
        .map_err(|_| Error::new("STATE_CORRUPT", "Invalid log state"))
}

pub fn validate_legacy_policy(old: Option<&Manifest>, new: &Manifest) -> Result<()> {
    let changed = |value: Option<u64>, previous: Option<u64>| value.is_some() && value != previous;
    if changed(
        new.spec.logs.max_disk_bytes,
        old.and_then(|m| m.spec.logs.max_disk_bytes),
    ) || changed(
        new.spec.logs.retention_seconds,
        old.and_then(|m| m.spec.logs.retention_seconds),
    ) || new.spec.logs.max_segments.is_some()
        && new.spec.logs.max_segments != old.and_then(|m| m.spec.logs.max_segments)
    {
        return Err(Error::new(
            "LOG_POLICY_MOVED",
            "Installation log limits are managed through log storage policy; remove new or changed legacy fields",
        ));
    }
    Ok(())
}
pub(crate) fn ensure_stream(connection: &Connection, launch: &Launch, now: i64) -> Result<()> {
    let mode = if launch.manifest.spec.execution.adapter != "process" {
        "EXTERNAL_SNAPSHOT"
    } else if launch.manifest.spec.logs.rotation.is_some() {
        "ROLLING"
    } else {
        "PREFIX"
    };
    connection.execute("INSERT INTO log_streams(id,attempt_id,capture_mode,capture_state,created_at_us) VALUES(?1,?1,?2,'PREPARED',?3) ON CONFLICT(attempt_id) DO NOTHING",params![launch.attempt_id,mode,now])?;
    Ok(())
}
pub(crate) fn finish_stream(
    connection: &Connection,
    attempt: &str,
    outcome: &Outcome,
    now: i64,
) -> Result<()> {
    let state = if outcome.state == crate::process::EndState::Unknown {
        "FROZEN"
    } else {
        "SEALED"
    };
    connection.execute("UPDATE log_streams SET capture_state=CASE WHEN capture_state='STOPPED' AND ?1!='FROZEN' THEN capture_state ELSE ?1 END,truncated=MAX(truncated,?2),closed_at_us=?3 WHERE attempt_id=?4",params![state,outcome.log_truncated,now,attempt])?;
    connection.execute("UPDATE log_segments SET state='SEALED',closed_at_us=?1 WHERE stream_id IN(SELECT id FROM log_streams WHERE attempt_id=?2) AND state='OPEN'",params![now,attempt])?;
    Ok(())
}
#[derive(Debug)]
struct Segment {
    id: String,
    stream: String,
    path: String,
    start: u64,
    end: u64,
    state: String,
    sequence: u64,
}
fn counter(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value)
        .ok()
        .filter(|v| *v <= MAX_OFFSET)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn segment_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Segment> {
    Ok(Segment {
        id: row.get(0)?,
        stream: row.get(1)?,
        path: row.get(2)?,
        start: counter(row, 3)?,
        end: counter(row, 4)?,
        state: row.get(5)?,
        sequence: counter(row, 6)?,
    })
}
const SEGMENTS: &str =
    "SELECT id,stream_id,relative_path,start_offset,end_offset,state,sequence_no FROM log_segments";
fn path(directory: &Path, relative: &str) -> Result<PathBuf> {
    let Some(name) = relative.strip_prefix("logs/") else {
        return Err(Error::new("STATE_CORRUPT", "Invalid log path"));
    };
    let Some(stem) = name.strip_suffix(".log") else {
        return Err(Error::new("STATE_CORRUPT", "Invalid log path"));
    };
    let mut parts = stem.split('.');
    let identity = parts.next().ok_or_else(unavailable)?;
    if uuid::Uuid::parse_str(identity).is_err()
        || parts.next().is_some_and(|v| v.parse::<u64>().is_err())
        || parts.next().is_some()
    {
        return Err(Error::new("STATE_CORRUPT", "Invalid log path"));
    }
    Ok(directory.join(relative))
}
fn checked_file(directory: &Path, relative: &str, writable: bool) -> Result<File> {
    private_directory(&directory.join("logs"))?;
    let full = path(directory, relative)?;
    let meta = fs::symlink_metadata(&full).map_err(|_| unavailable())?;
    if !meta.is_file()
        || meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err(Error::new(
            "UNSAFE_STATE_PATH",
            "Logs require private owned regular files without hard links",
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(writable)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(full)
        .map_err(|_| unavailable())?;
    let opened = file.metadata().map_err(|_| unavailable())?;
    if opened.ino() != meta.ino() || opened.dev() != meta.dev() {
        return Err(unavailable());
    }
    Ok(file)
}
fn sync_directory(directory: &Path) -> Result<()> {
    File::open(directory)
        .and_then(|f| f.sync_all())
        .map_err(|_| Error::new("STORAGE_UNAVAILABLE", "Cannot synchronize log directory"))
}

impl Store {
    fn log_segment_path(&self, segment: &Segment) -> Result<PathBuf> {
        let owner: String = self.connection.query_row(
            "SELECT COALESCE(attempt_id,service_instance_id) FROM log_streams WHERE id=?1",
            [&segment.stream],
            |r| r.get(0),
        )?;
        let legacy = format!("logs/{owner}.log");
        let rotated = format!("logs/{owner}.{}.log", segment.sequence);
        if (segment.sequence != 0 || segment.path != legacy) && segment.path != rotated {
            return Err(Error::new(
                "STATE_CORRUPT",
                "Log path does not match its stream identity",
            ));
        }
        path(&self.directory, &segment.path)
    }
    fn open_log_file(&self, segment: &Segment, writable: bool) -> Result<File> {
        self.log_segment_path(segment)?;
        checked_file(&self.directory, &segment.path, writable)
    }
    pub fn log_storage_policy(&self) -> Result<LogStoragePolicyDto> {
        let imported_conservative =
            self.setting("log_policy_import_discrepancy")?.as_deref() == Some("true");
        self.connection.query_row("SELECT revision,max_disk_bytes,max_segments,retention_seconds FROM log_storage_policy WHERE singleton=1",[],|r|Ok(LogStoragePolicyDto { imported_conservative,revision:counter(r,0)?,max_disk_bytes:counter(r,1)?,max_segments:r.get(2)?,retention_seconds:counter(r,3)? })).map_err(Into::into)
    }
    pub fn log_storage(&self) -> Result<LogStorageDto> {
        let policy = self.log_storage_policy()?;
        let mut usage=self.connection.query_row("SELECT COALESCE(SUM(l.bytes_written),0),count(*),COALESCE(SUM(CASE WHEN l.state IN('OPEN','PREPARED') THEN l.bytes_written ELSE 0 END),0),COALESCE(SUM(l.state IN('OPEN','PREPARED')),0),COALESCE(SUM(CASE WHEN s.capture_state='FROZEN' OR a.state='UNKNOWN' OR COALESCE(s.last_error_code,'')='LOG_UNAVAILABLE' THEN l.bytes_written ELSE 0 END),0),COALESCE(SUM(s.capture_state='FROZEN' OR a.state='UNKNOWN' OR COALESCE(s.last_error_code,'')='LOG_UNAVAILABLE'),0) FROM log_segments l JOIN log_streams s ON s.id=l.stream_id LEFT JOIN attempts a ON a.id=s.attempt_id",[],|r|Ok(LogStorageUsageDto { retained_bytes:counter(r,0)?,retained_segments:r.get(1)?,reserved_bytes:counter(r,2)?,reserved_segments:r.get(3)?,protected_bytes:counter(r,4)?,protected_segments:r.get(5)?,..LogStorageUsageDto::default() }))?;
        usage.capture_error_count = self.connection.query_row(
            "SELECT count(*) FROM log_streams WHERE last_error_code IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        let mut known = BTreeMap::new();
        let mut q = self
            .connection
            .prepare("SELECT relative_path,bytes_written FROM log_segments")?;
        for row in q.query_map([], |r| Ok((r.get::<_, String>(0)?, counter(r, 1)?)))? {
            let (path, bytes) = row?;
            known.insert(path, bytes);
        }
        let directory = self.directory.join("logs");
        match fs::read_dir(&directory) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|_| unavailable())?;
                    let relative = format!("logs/{}", entry.file_name().to_string_lossy());
                    let meta = fs::symlink_metadata(entry.path()).map_err(|_| unavailable())?;
                    let extra = if let Some(known_bytes) = known.get(&relative) {
                        meta.len().saturating_sub(*known_bytes)
                    } else {
                        usage.unexplained_segments = usage
                            .unexplained_segments
                            .checked_add(1)
                            .ok_or_else(unavailable)?;
                        meta.len()
                    };
                    usage.unexplained_bytes = usage
                        .unexplained_bytes
                        .checked_add(extra)
                        .filter(|v| *v <= MAX_OFFSET)
                        .ok_or_else(unavailable)?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(unavailable()),
        }
        usage.under_pressure = usage.retained_bytes.saturating_add(usage.unexplained_bytes)
            >= policy.max_disk_bytes
            || usage
                .retained_segments
                .saturating_add(usage.unexplained_segments)
                >= policy.max_segments;
        Ok(LogStorageDto { policy, usage })
    }
    pub fn set_log_storage_policy(
        &self,
        input: &LogStoragePolicyInput,
        expected: u64,
    ) -> Result<LogStoragePolicyDto> {
        if input.max_disk_bytes == 0
            || input.max_disk_bytes > 10_737_418_240
            || !(1..=100000).contains(&input.max_segments)
            || input.retention_seconds == 0
            || input.retention_seconds > 9_223_372_036_854
        {
            return Err(invalid(
                "Installation log policy is outside supported limits",
            ));
        }
        let old = self.log_storage_policy()?;
        if expected != old.revision {
            return Err(Error::new(
                "REVISION_CONFLICT",
                "Log storage policy revision changed",
            ));
        }
        if input.max_disk_bytes < old.max_disk_bytes
            || input.max_segments < old.max_segments
            || input.retention_seconds < old.retention_seconds
        {
            let active:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM attempts WHERE state IN('LAUNCH_INTENT_RECORDED','START_REQUESTED','RUNNING','UNKNOWN')) OR EXISTS(SELECT 1 FROM log_segments WHERE state IN('PREPARED','OPEN'))",[],|r|r.get(0))?;
            if active {
                return Err(Error::new(
                    "STATE_CONFLICT",
                    "Drain active attempts and reconcile UNKNOWN before lowering log storage policy",
                ));
            }
            let usage = self.log_storage()?.usage;
            if usage
                .protected_bytes
                .saturating_add(usage.unexplained_bytes)
                > input.max_disk_bytes
                || usage
                    .protected_segments
                    .saturating_add(usage.unexplained_segments)
                    > input.max_segments
            {
                return Err(Error::new(
                    "STATE_CONFLICT",
                    "Protected or unexplained evidence exceeds proposed policy",
                ));
            }
        }
        let revision = expected
            .checked_add(1)
            .filter(|v| *v <= MAX_OFFSET)
            .ok_or_else(|| invalid("Policy revision exhausted"))?;
        let changed=self.connection.execute("UPDATE log_storage_policy SET revision=?1,max_disk_bytes=?2,max_segments=?3,retention_seconds=?4 WHERE singleton=1 AND revision=?5",params![durable(revision)?,durable(input.max_disk_bytes)?,input.max_segments,durable(input.retention_seconds)?,durable(expected)?])?;
        if changed != 1 {
            return Err(Error::new(
                "REVISION_CONFLICT",
                "Log storage policy revision changed",
            ));
        }
        self.log_storage_policy()
    }
    pub fn log_info(&self, attempt: &str) -> Result<LogStreamDto> {
        let row=self.connection.query_row("SELECT capture_mode,capture_state,retained_from_offset,written_end_offset,known_discarded_bytes,history_evicted,truncated,last_error_code FROM log_streams WHERE attempt_id=?1",[attempt],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,bool>(5)?,r.get::<_,bool>(6)?,r.get::<_,Option<String>>(7)?))).optional()?.ok_or_else(||Error::new("LOG_UNAVAILABLE","Log capture is not available for this attempt"))?;
        let retained=self.connection.query_row("SELECT COALESCE(SUM(l.bytes_written),0) FROM log_segments l JOIN log_streams s ON s.id=l.stream_id WHERE s.attempt_id=?1",[attempt],|r|r.get::<_,i64>(0))?;
        Ok(LogStreamDto {
            attempt_id: attempt.into(),
            capture_mode: decode(row.0)?,
            capture_state: decode(row.1)?,
            retained_from_offset: nonnegative(row.2)?,
            written_end_offset: nonnegative(row.3)?,
            retained_bytes: nonnegative(retained)?,
            known_discarded_bytes: nonnegative(row.4)?,
            history_evicted: row.5,
            log_truncated: row.6,
            last_error_code: row.7,
        })
    }
    pub fn read_latest_log(&self, attempt: &str, max_bytes: u32) -> Result<LogDto> {
        // Range selection and the bounded read share one daemon command turn.
        let stream = self.log_info(attempt)?;
        self.read_log(attempt, stream.retained_from_offset, max_bytes)
    }
    pub fn read_log(&self, attempt: &str, offset: u64, max_bytes: u32) -> Result<LogDto> {
        if !(1..=65536).contains(&max_bytes) {
            return Err(invalid("Log byte limit must be between 1 and 65536"));
        }
        let stream = self.log_info(attempt)?;
        if offset < stream.retained_from_offset {
            return Err(Error::new(
                "LOG_CURSOR_EXPIRED",
                "Earlier log history expired; query log info and explicitly resume from retained start",
            ));
        }
        if offset > stream.written_end_offset {
            return Err(invalid("Log offset is beyond the saved stream end"));
        }
        if stream.last_error_code.as_deref() == Some("LOG_UNAVAILABLE") {
            return Err(unavailable());
        }
        let mut q=self.connection.prepare(&format!("{SEGMENTS} WHERE stream_id=(SELECT id FROM log_streams WHERE attempt_id=?1) AND end_offset>?2 AND start_offset<?3 ORDER BY sequence_no"))?;
        let end = offset
            .saturating_add(u64::from(max_bytes) + 4)
            .min(stream.written_end_offset);
        let segments = q
            .query_map(
                params![attempt, durable(offset)?, durable(end)?],
                segment_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut bytes = Vec::new();
        let mut at = offset;
        for (index, segment) in segments.into_iter().enumerate() {
            if segment.start > at
                || (index > 0 && segment.start != at)
                || segment.end <= at
                || segment.state == "PREPARED"
            {
                return Err(unavailable());
            }
            let mut file = self.open_log_file(&segment, false)?;
            if file.metadata().map_err(|_| unavailable())?.len() < segment.end - segment.start {
                return Err(unavailable());
            }
            file.seek(SeekFrom::Start(at - segment.start))
                .map_err(|_| unavailable())?;
            let take = (end - at).min(segment.end - at);
            let before = bytes.len();
            file.take(take)
                .read_to_end(&mut bytes)
                .map_err(|_| unavailable())?;
            if u64::try_from(bytes.len() - before).map_err(|_| unavailable())? != take {
                return Err(unavailable());
            }
            at += take;
        }
        if at != end {
            return Err(unavailable());
        }
        let terminal = matches!(
            stream.capture_state,
            LogCaptureState::Sealed | LogCaptureState::Frozen | LogCaptureState::Stopped
        );
        let cap = usize::try_from(max_bytes).map_err(|_| invalid("Invalid log byte limit"))?;
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
        Ok(LogDto {
            attempt_id: attempt.into(),
            text: String::from_utf8_lossy(&bytes).into_owned(),
            response_truncated,
            log_truncated: stream.log_truncated,
            next_offset: offset + u64::try_from(consumed).map_err(|_| unavailable())?,
            untrusted: true,
            stream,
        })
    }
    pub(crate) fn legacy_log_path(&self, attempt: &str) -> Result<PathBuf> {
        let paths = {
            let mut q=self.connection.prepare("SELECT l.relative_path FROM log_segments l JOIN log_streams s ON s.id=l.stream_id WHERE s.attempt_id=?1")?;
            q.query_map([attempt], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        if paths.len() != 1 {
            return Err(Error::new(
                "LOG_UNAVAILABLE",
                "Legacy log path requires exactly one non-rotated physical file",
            ));
        }
        let relative = paths.first().ok_or_else(unavailable)?;
        if relative != &format!("logs/{attempt}.log") {
            return Err(Error::new(
                "LOG_UNAVAILABLE",
                "Use the logical reader for rotated logs",
            ));
        }
        let full = path(&self.directory, relative)?;
        let _ = checked_file(&self.directory, relative, false)?;
        Ok(full)
    }
    /// Call only with a freshly verified, redacted external terminal snapshot.
    pub fn prepare_external_log_capture(&self, launch: &Launch) -> Result<()> {
        ensure_stream(&self.connection, launch, crate::now_us())?;
        // Recovery may freeze an empty stream before ownership and original secrets are rebuilt.
        self.connection.execute("UPDATE log_streams SET capture_state='PREPARED',closed_at_us=NULL WHERE attempt_id=?1 AND capture_mode='EXTERNAL_SNAPSHOT' AND capture_state='FROZEN' AND written_end_offset=0 AND truncated=0 AND last_error_code IS NULL AND NOT EXISTS(SELECT 1 FROM log_segments WHERE stream_id=log_streams.id) AND EXISTS(SELECT 1 FROM attempts a WHERE a.id=?1 AND a.state IN('START_REQUESTED','RUNNING','UNKNOWN'))",[&launch.attempt_id])?;
        self.prepare_log_capture(launch)
    }
    pub fn prepare_log_capture(&self, launch: &Launch) -> Result<()> {
        ensure_stream(&self.connection, launch, crate::now_us())?;
        if self.log_info(&launch.attempt_id)?.capture_state == LogCaptureState::Prepared {
            self.open_log_segment(&launch.attempt_id, 0)?;
        }
        Ok(())
    }
    fn open_log_segment(&self, attempt: &str, offset: u64) -> Result<Segment> {
        self.retain_log_segments(crate::now_us(), 0, 1)?;
        let storage = self.log_storage()?;
        if storage
            .usage
            .retained_segments
            .saturating_add(storage.usage.unexplained_segments)
            >= storage.policy.max_segments
        {
            return Err(Error::new(
                "LOG_STORAGE_PRESSURE",
                "Installation log file budget exhausted",
            ));
        }
        let (stream,sequence):(String,u64)=self.connection.query_row("SELECT s.id,COALESCE(MAX(l.sequence_no)+1,0) FROM log_streams s LEFT JOIN log_segments l ON l.stream_id=s.id WHERE s.attempt_id=?1 GROUP BY s.id",[attempt],|r|Ok((r.get(0)?,counter(r,1)?)))?;
        // Sequence never resets after complete retention: lifetime offset also makes each filename unique.
        let sequence = sequence.max(offset);
        let relative = if sequence == 0 {
            format!("logs/{attempt}.log")
        } else {
            format!("logs/{attempt}.{sequence}.log")
        };
        let id = uuid::Uuid::new_v4().to_string();
        let now = crate::now_us();
        self.connection.execute("INSERT INTO log_segments(id,stream_id,sequence_no,relative_path,start_offset,end_offset,bytes_written,state,created_at_us) VALUES(?1,?2,?3,?4,?5,?5,0,'PREPARED',?6)",params![id,stream,durable(sequence)?,relative,durable(offset)?,now])?;
        let directory = private_directory(&self.directory.join("logs"))?;
        let file = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .open(path(&self.directory, &relative)?)
            .map_err(|_| Error::new("LOG_STORAGE_FAILURE", "Cannot create log segment"))?;
        file.sync_all().map_err(|_| unavailable())?;
        sync_directory(&directory)?;
        self.connection
            .execute("UPDATE log_segments SET state='OPEN' WHERE id=?1", [&id])?;
        self.connection.execute("UPDATE log_streams SET capture_state='CAPTURING' WHERE id=?1 AND capture_state='PREPARED'",[&stream])?;
        Ok(Segment {
            id,
            stream,
            path: relative,
            start: offset,
            end: offset,
            state: "OPEN".into(),
            sequence,
        })
    }
    pub fn append_log(&self, launch: &Launch, bytes: &[u8]) -> Result<usize> {
        let mut saved = 0;
        while saved < bytes.len() {
            let info = self.log_info(&launch.attempt_id)?;
            if !matches!(
                info.capture_state,
                LogCaptureState::Capturing | LogCaptureState::Prepared
            ) {
                return Ok(saved);
            }
            let rotation = launch.manifest.spec.logs.rotation.as_ref();
            let mut current=self.connection.query_row(&format!("{SEGMENTS} WHERE stream_id=(SELECT id FROM log_streams WHERE attempt_id=?1) AND state='OPEN'"),[&launch.attempt_id],segment_row).optional()?;
            if current.as_ref().is_some_and(|segment| {
                rotation.is_some_and(|r| segment.end - segment.start >= r.segment_bytes)
            }) && let Some(segment) = current.take()
            {
                self.open_log_file(&segment, false)?
                    .sync_all()
                    .map_err(|_| unavailable())?;
                self.connection.execute(
                    "UPDATE log_segments SET state='SEALED',closed_at_us=?1 WHERE id=?2",
                    params![crate::now_us(), segment.id],
                )?;
            }
            if current.is_none() {
                if let Some(rotation) = rotation {
                    self.trim_stream(
                        &launch.attempt_id,
                        launch
                            .manifest
                            .spec
                            .logs
                            .max_run_bytes
                            .saturating_sub(rotation.segment_bytes),
                        rotation.max_segments - 1,
                    )?;
                }
                current = Some(self.open_log_segment(&launch.attempt_id, info.written_end_offset)?);
            }
            let segment = current.ok_or_else(unavailable)?;
            let limit =
                rotation.map_or(launch.manifest.spec.logs.max_run_bytes, |r| r.segment_bytes);
            let room = limit.saturating_sub(if rotation.is_some() {
                segment.end - segment.start
            } else {
                info.written_end_offset
            });
            let mut count = u64::try_from(bytes.len() - saved)
                .map_err(|_| unavailable())?
                .min(room);
            if count == 0 {
                self.stop_log_capture(
                    &launch.attempt_id,
                    "LOG_PREFIX_LIMIT",
                    u64::try_from(bytes.len() - saved).map_err(|_| unavailable())?,
                )?;
                return Ok(saved);
            }
            if info
                .written_end_offset
                .checked_add(count)
                .is_none_or(|v| v > MAX_OFFSET)
            {
                return Err(Error::new(
                    "LOG_OFFSET_EXHAUSTED",
                    "Log cursor range exhausted",
                ));
            }
            self.retain_log_segments(crate::now_us(), count, 0)?;
            let storage = self.log_storage()?;
            count = count.min(
                storage.policy.max_disk_bytes.saturating_sub(
                    storage
                        .usage
                        .retained_bytes
                        .saturating_add(storage.usage.unexplained_bytes),
                ),
            );
            if count == 0 {
                return Err(Error::new(
                    "LOG_STORAGE_PRESSURE",
                    "Installation log byte budget exhausted",
                ));
            }
            let count = usize::try_from(count).map_err(|_| unavailable())?;
            let mut file = self.open_log_file(&segment, true)?;
            if file.metadata().map_err(|_| unavailable())?.len() != segment.end - segment.start {
                return Err(unavailable());
            }
            file.seek(SeekFrom::End(0)).map_err(|_| unavailable())?;
            let mut written = 0;
            let mut failed = false;
            while written < count {
                match file.write(&bytes[saved + written..saved + count]) {
                    Ok(0) => {
                        failed = true;
                        break;
                    }
                    Ok(n) => written += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            if file.sync_data().is_err() {
                failed = true;
            }
            let end = segment.end + u64::try_from(written).map_err(|_| unavailable())?;
            self.connection.execute_batch("SAVEPOINT log_append")?;
            let result = (|| -> Result<()> {
                self.connection.execute("UPDATE log_segments SET end_offset=?1,bytes_written=?1-start_offset WHERE id=?2",params![durable(end)?,segment.id])?;
                self.connection.execute(
                    "UPDATE log_streams SET written_end_offset=?1 WHERE id=?2",
                    params![durable(end)?, segment.stream],
                )?;
                Ok(())
            })();
            if let Err(error) = result {
                self.connection
                    .execute_batch("ROLLBACK TO log_append; RELEASE log_append")?;
                return Err(error);
            }
            self.connection.execute_batch("RELEASE log_append")?;
            saved += written;
            if failed {
                return Err(Error::new(
                    "LOG_STORAGE_FAILURE",
                    "Log append failed; capture stopped",
                ));
            }
        }
        Ok(saved)
    }
    pub fn stop_log_capture(&self, attempt: &str, reason: &str, discarded: u64) -> Result<()> {
        let info = self.log_info(attempt)?;
        let discarded = info
            .known_discarded_bytes
            .checked_add(discarded)
            .filter(|v| *v <= MAX_OFFSET)
            .ok_or_else(|| Error::new("LOG_OFFSET_EXHAUSTED", "Log discard counter exhausted"))?;
        self.connection.execute("UPDATE log_streams SET capture_state=CASE WHEN capture_state='FROZEN' THEN capture_state ELSE 'STOPPED' END,truncated=1,last_error_code=COALESCE(last_error_code,?1),known_discarded_bytes=?2 WHERE attempt_id=?3",params![reason,durable(discarded)?,attempt])?;
        Ok(())
    }
    pub fn freeze_log_capture(&self, attempt: &str) -> Result<()> {
        let segments = {
            let mut q = self.connection.prepare(&format!(
                "{SEGMENTS} WHERE stream_id=(SELECT id FROM log_streams WHERE attempt_id=?1)"
            ))?;
            q.query_map([attempt], segment_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for segment in segments {
            self.open_log_file(&segment, false)?
                .sync_all()
                .map_err(|_| unavailable())?;
        }
        self.connection.execute(
            "UPDATE log_streams SET capture_state='FROZEN',closed_at_us=?1 WHERE attempt_id=?2",
            params![crate::now_us(), attempt],
        )?;
        Ok(())
    }
    fn trim_stream(&self, attempt: &str, bytes: u64, files: u32) -> Result<()> {
        loop {
            let info = self.log_info(attempt)?;
            let count:u32=self.connection.query_row("SELECT count(*) FROM log_segments WHERE stream_id=(SELECT id FROM log_streams WHERE attempt_id=?1)",[attempt],|r|r.get(0))?;
            if info.retained_bytes <= bytes && count <= files {
                return Ok(());
            }
            let candidate=self.connection.query_row("SELECT l.id FROM log_segments l JOIN log_streams s ON s.id=l.stream_id WHERE s.attempt_id=?1 AND l.state='SEALED' AND s.capture_state NOT IN('FROZEN','STOPPED') AND COALESCE(s.last_error_code,'')!='LOG_UNAVAILABLE' ORDER BY l.sequence_no LIMIT 1",[attempt],|r|r.get::<_,String>(0)).optional()?;
            let Some(candidate) = candidate else {
                return Err(Error::new(
                    "LOG_STORAGE_PRESSURE",
                    "Protected stream cannot rotate",
                ));
            };
            if !self.delete_log_segment(&candidate)? {
                return Err(unavailable());
            }
        }
    }
    fn mark_log_unavailable(&self, segment: &Segment) -> Result<bool> {
        self.connection.execute(
            "UPDATE log_streams SET last_error_code='LOG_UNAVAILABLE',truncated=1 WHERE id=?1",
            [&segment.stream],
        )?;
        self.connection.execute(
            "UPDATE log_segments SET state='SEALED' WHERE id=?1 AND state='DELETE_PENDING'",
            [&segment.id],
        )?;
        Ok(false)
    }
    fn delete_log_segment(&self, id: &str) -> Result<bool> {
        let segment =
            self.connection
                .query_row(&format!("{SEGMENTS} WHERE id=?1"), [id], segment_row)?;
        let (protected,evidence_unavailable):(bool,bool)=self.connection.query_row("SELECT s.capture_state='FROZEN' OR COALESCE(a.state='UNKNOWN',0),COALESCE(s.last_error_code,'')='LOG_UNAVAILABLE' FROM log_streams s LEFT JOIN attempts a ON a.id=s.attempt_id WHERE s.id=?1",[&segment.stream],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if evidence_unavailable {
            return self.mark_log_unavailable(&segment);
        }
        if protected {
            return Err(Error::new(
                "STATE_CONFLICT",
                "UNKNOWN log evidence is protected",
            ));
        }
        let file = match self.open_log_file(&segment, false) {
            Ok(file) => file,
            Err(error) if error.code == "LOG_UNAVAILABLE" => {
                return self.mark_log_unavailable(&segment);
            }
            Err(error) => return Err(error),
        };
        let metadata = file.metadata().map_err(|_| unavailable())?;
        if metadata.len() != segment.end - segment.start {
            return self.mark_log_unavailable(&segment);
        }
        self.connection.execute(
            "UPDATE log_segments SET state='DELETE_PENDING' WHERE id=?1",
            [id],
        )?;
        let full = self.log_segment_path(&segment)?;
        let current = match fs::symlink_metadata(&full) {
            Ok(metadata) => metadata,
            Err(_) => return self.mark_log_unavailable(&segment),
        };
        if !current.is_file()
            || current.uid() != nix::unistd::geteuid().as_raw()
            || current.nlink() != 1
            || current.mode() & 0o077 != 0
            || current.ino() != metadata.ino()
            || current.dev() != metadata.dev()
            || current.len() != segment.end - segment.start
        {
            return self.mark_log_unavailable(&segment);
        }
        if fs::remove_file(full).is_err() {
            return self.mark_log_unavailable(&segment);
        }
        sync_directory(&self.directory.join("logs"))?;
        drop(file);
        self.finalize_log_delete(&segment)?;
        Ok(true)
    }
    fn finalize_log_delete(&self, segment: &Segment) -> Result<()> {
        self.connection.execute_batch("SAVEPOINT log_delete")?;
        let result = (|| -> Result<()> {
            self.connection
                .execute("DELETE FROM log_segments WHERE id=?1", [&segment.id])?;
            self.connection.execute("UPDATE log_streams SET retained_from_offset=MAX(retained_from_offset,?1),history_evicted=1 WHERE id=?2",params![durable(segment.end)?,segment.stream])?;
            crate::store::audit_actor(
                &self.connection,
                "local-owner",
                "logs.retention",
                &segment.id,
                "SUCCEEDED",
                crate::now_us(),
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            self.connection
                .execute_batch("ROLLBACK TO log_delete; RELEASE log_delete")?;
            return Err(error);
        }
        self.connection.execute_batch("RELEASE log_delete")?;
        Ok(())
    }
    pub(crate) fn retain_log_segments(
        &self,
        now: i64,
        reserve_bytes: u64,
        reserve_files: u32,
    ) -> Result<()> {
        let policy = self.log_storage_policy()?;
        let retention = durable(policy.retention_seconds)?
            .checked_mul(1_000_000)
            .ok_or_else(|| invalid("Log retention duration overflow"))?;
        for _ in 0..16 {
            let usage = self.log_storage()?.usage;
            let pressure = usage
                .retained_bytes
                .saturating_add(usage.unexplained_bytes)
                .saturating_add(reserve_bytes)
                > policy.max_disk_bytes
                || usage
                    .retained_segments
                    .saturating_add(usage.unexplained_segments)
                    .saturating_add(reserve_files)
                    > policy.max_segments;
            let candidate=self.connection.query_row("SELECT l.id FROM log_segments l JOIN log_streams s ON s.id=l.stream_id LEFT JOIN attempts a ON a.id=s.attempt_id WHERE l.state='SEALED' AND s.capture_state!='FROZEN' AND COALESCE(s.last_error_code,'')!='LOG_UNAVAILABLE' AND (a.state IN('SUCCEEDED','FAILED','TIMED_OUT','CANCELLED') OR (a.state='RUNNING' AND s.capture_mode='ROLLING' AND s.capture_state='CAPTURING')) AND NOT EXISTS(SELECT 1 FROM log_segments earlier WHERE earlier.stream_id=l.stream_id AND earlier.sequence_no<l.sequence_no) AND (?1 OR l.created_at_us<?2) ORDER BY l.created_at_us,l.id LIMIT 1",params![pressure,now.saturating_sub(retention)],|r|r.get::<_,String>(0)).optional()?;
            let Some(candidate) = candidate else {
                return Ok(());
            };
            self.delete_log_segment(&candidate)?;
        }
        Ok(())
    }
    pub(crate) fn recover_log_streams(&self) -> Result<()> {
        self.connection.execute("UPDATE log_streams SET capture_state='FROZEN' WHERE attempt_id IN(SELECT id FROM attempts WHERE state='UNKNOWN')",[])?;
        let segments = {
            let mut q = self
                .connection
                .prepare(&format!("{SEGMENTS} ORDER BY stream_id,sequence_no"))?;
            q.query_map([], segment_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for segment in segments {
            let full = self.log_segment_path(&segment)?;
            let exists = match fs::symlink_metadata(&full) {
                Ok(_) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(_) => return Err(unavailable()),
            };
            if segment.state == "DELETE_PENDING" {
                let frozen:bool=self.connection.query_row("SELECT capture_state='FROZEN' OR EXISTS(SELECT 1 FROM attempts a WHERE a.id=log_streams.attempt_id AND a.state='UNKNOWN') FROM log_streams WHERE id=?1",[&segment.stream],|r|r.get(0))?;
                if !exists {
                    self.finalize_log_delete(&segment)?;
                    continue;
                }
                if frozen {
                    self.connection.execute(
                        "UPDATE log_segments SET state='SEALED' WHERE id=?1",
                        [&segment.id],
                    )?;
                } else {
                    self.delete_log_segment(&segment.id)?;
                    continue;
                }
            }
            if !exists && segment.state == "PREPARED" && segment.start == segment.end {
                self.connection
                    .execute("DELETE FROM log_segments WHERE id=?1", [&segment.id])?;
                continue;
            }
            if !exists {
                self.connection.execute("UPDATE log_streams SET last_error_code='LOG_UNAVAILABLE',truncated=1 WHERE id=?1",[&segment.stream])?;
                continue;
            }
            let file = self.open_log_file(&segment, false)?;
            let actual = file.metadata().map_err(|_| unavailable())?.len();
            if actual < segment.end - segment.start
                || (segment.state == "SEALED" && actual != segment.end - segment.start)
            {
                self.connection.execute("UPDATE log_streams SET last_error_code='LOG_UNAVAILABLE',truncated=1 WHERE id=?1",[&segment.stream])?;
                continue;
            }
            let end = segment
                .start
                .checked_add(actual)
                .filter(|v| *v <= MAX_OFFSET)
                .ok_or_else(unavailable)?;
            self.connection.execute("UPDATE log_segments SET end_offset=?1,bytes_written=?2,state='SEALED',closed_at_us=COALESCE(closed_at_us,?3) WHERE id=?4",params![durable(end)?,durable(actual)?,crate::now_us(),segment.id])?;
            self.connection.execute("UPDATE log_streams SET written_end_offset=MAX(written_end_offset,?1),capture_state=CASE WHEN EXISTS(SELECT 1 FROM attempts a WHERE a.id=log_streams.attempt_id AND a.state='UNKNOWN') THEN 'FROZEN' WHEN capture_state IN('PREPARED','CAPTURING') THEN 'SEALED' ELSE capture_state END WHERE id=?2",params![durable(end)?,segment.stream])?;
        }
        let mut streams = self
            .connection
            .prepare("SELECT id,retained_from_offset,written_end_offset FROM log_streams")?;
        for stream in streams.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, counter(r, 1)?, counter(r, 2)?))
        })? {
            let (id, start, end) = stream?;
            let mut expected = start;
            let mut ranges=self.connection.prepare("SELECT start_offset,end_offset FROM log_segments WHERE stream_id=?1 ORDER BY sequence_no")?;
            for range in ranges.query_map([&id], |r| Ok((counter(r, 0)?, counter(r, 1)?)))? {
                let (start, end) = range?;
                if start != expected {
                    return Err(Error::new(
                        "STATE_CORRUPT",
                        "Log segments do not form a contiguous retained range",
                    ));
                }
                expected = end;
            }
            if expected != end {
                return Err(Error::new(
                    "STATE_CORRUPT",
                    "Log stream range disagrees with retained segments",
                ));
            }
        }
        Ok(())
    }
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
