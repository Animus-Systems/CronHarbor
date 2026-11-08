//! Optional metadata budget; history and idempotency records are never purged for capacity.
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{Error, Result, contracts::AdmissionStatusDto, store::Store};

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmissionPolicy {
    #[ts(type = "number")]
    pub max_database_bytes: u64,
    #[ts(type = "number")]
    pub warning_bytes: u64,
    #[ts(type = "number")]
    pub admission_pause_bytes: u64,
    #[ts(type = "number")]
    pub max_wal_bytes: u64,
}

fn invalid() -> Error {
    Error::new(
        "ADMISSION_POLICY_INVALID",
        "Admission policy must be private, valid and bounded",
    )
}

pub(crate) fn load(directory: &Path) -> Result<Option<AdmissionPolicy>> {
    let path = directory.join("admission-policy.json");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(invalid()),
        Ok(_) => {}
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| invalid())?;
    let meta = file.metadata().map_err(|_| invalid())?;
    if !meta.is_file()
        || meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.nlink() != 1
        || meta.mode() & 0o077 != 0
        || meta.len() > 4096
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > 4096 {
        return Err(invalid());
    }
    let policy: AdmissionPolicy = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if policy.max_database_bytes < 1_048_576
        || policy.max_database_bytes > i64::MAX as u64
        || policy.warning_bytes == 0
        || policy.warning_bytes >= policy.admission_pause_bytes
        || policy.admission_pause_bytes >= policy.max_database_bytes
        || policy.max_wal_bytes < 4096
        || policy.max_wal_bytes > policy.max_database_bytes
    {
        return Err(invalid());
    }
    Ok(Some(policy))
}

pub(crate) fn configure(connection: &Connection, policy: &AdmissionPolicy) -> Result<()> {
    let page_size: u32 = connection.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let pages: u32 = connection.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let limit =
        u32::try_from((policy.max_database_bytes / u64::from(page_size)).min(4_294_967_294))
            .map_err(|_| invalid())?;
    if pages > limit {
        return Err(pressure());
    }
    connection.pragma_update(None, "max_page_count", limit)?;
    connection.pragma_update(
        None,
        "journal_size_limit",
        i64::try_from(policy.max_wal_bytes).map_err(|_| invalid())?,
    )?;
    Ok(())
}

fn pressure() -> Error {
    Error::new(
        "STORAGE_PRESSURE",
        "New work is paused by the metadata storage budget; preserve history and inspect admission status",
    )
}

impl Store {
    pub fn admission_status(&self) -> Result<Option<AdmissionStatusDto>> {
        let Some(policy) = &self.admission_policy else {
            return Ok(None);
        };
        let page_size: u32 = self
            .connection
            .query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let pages: u32 = self
            .connection
            .query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let database_bytes = u64::from(pages)
            .checked_mul(u64::from(page_size))
            .ok_or_else(|| Error::new("STORAGE_UNAVAILABLE", "Cannot measure metadata storage"))?;
        let wal_bytes = match fs::symlink_metadata(self.directory.join("state.db-wal")) {
            Ok(meta)
                if meta.is_file()
                    && meta.nlink() == 1
                    && meta.uid() == nix::unistd::geteuid().as_raw()
                    && meta.mode() & 0o077 == 0 =>
            {
                meta.len()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            _ => {
                return Err(Error::new(
                    "STORAGE_UNAVAILABLE",
                    "Cannot inspect private metadata journal",
                ));
            }
        };
        Ok(Some(AdmissionStatusDto {
            database_bytes,
            wal_bytes,
            policy: policy.clone(),
            warning: database_bytes >= policy.warning_bytes || wal_bytes > policy.max_wal_bytes,
            paused: database_bytes >= policy.admission_pause_bytes
                || wal_bytes > policy.max_wal_bytes,
        }))
    }

    pub fn ensure_admission(&self) -> Result<()> {
        let Some(mut status) = self.admission_status()? else {
            return Ok(());
        };
        if status.wal_bytes > status.policy.max_wal_bytes && self.connection.is_autocommit() {
            // A pinned reader may prevent truncation; never treat a busy checkpoint as reclaimed space.
            let timeout: i64 = self
                .connection
                .query_row("PRAGMA busy_timeout", [], |r| r.get(0))?;
            self.connection.pragma_update(None, "busy_timeout", 0)?;
            let checkpoint =
                self.connection
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                        r.get::<_, i64>(0)
                    });
            self.connection
                .pragma_update(None, "busy_timeout", timeout)?;
            let busy = checkpoint?;
            if busy != 0 {
                return Err(pressure());
            }
            status = self.admission_status()?.ok_or_else(pressure)?;
        }
        if status.paused {
            Err(pressure())
        } else {
            Ok(())
        }
    }
}
