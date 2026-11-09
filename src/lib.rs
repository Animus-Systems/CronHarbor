pub mod admission;
pub mod alerts;
#[cfg(feature = "api")]
pub mod api;
pub mod application;
pub mod config;
pub mod contracts;
pub mod external;
pub mod http_readiness;
pub mod importer;
pub mod logs;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod ops;
pub mod policy;
pub mod process;
pub mod scheduler;
pub mod server;
pub mod store;
#[cfg(feature = "webhooks")]
pub mod webhooks;

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct Error {
    pub code: String,
    pub message: String,
}

impl Error {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::new(
            "PERSISTENCE_UNAVAILABLE",
            "State could not be read or durably updated",
        )
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn now_us() -> i64 {
    chrono::Utc::now().timestamp_micros()
}
