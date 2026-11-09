//! Bounded loopback checks; exact owner policy binds the endpoint and assertion to a unit.
use crate::{
    Error, Result,
    external::{ExecutorPolicy, ExternalHandle, SystemdScope},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_RESPONSE_BYTES: usize = 65_536;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum HttpLiteral {
    Boolean(bool),
    Number(i64),
    String(String),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HttpResponseCheck {
    Status,
    Html,
    Json {
        #[serde(default)]
        equals: BTreeMap<String, HttpLiteral>,
        #[serde(default)]
        nonempty_arrays: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpReadiness {
    pub url: String,
    pub status: u16,
    pub check: HttpResponseCheck,
    pub interval_seconds: u64,
    pub timeout_seconds: u64,
    pub failure_threshold: u32,
    pub success_threshold: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SystemdHttpReadinessPolicy {
    pub unit: String,
    pub scope: SystemdScope,
    pub url: String,
    pub status: u16,
    pub check: HttpResponseCheck,
}

fn invalid() -> Error {
    Error::new(
        "CONFIG_INVALID",
        "HTTP readiness requires bounded literal-loopback HTTP and response checks",
    )
}

fn validate_target(url: &str, status: u16, check: &HttpResponseCheck) -> Result<()> {
    if url.len() > 2048
        || url.bytes().any(|b| b.is_ascii_control() || b == b'\\')
        || !(100..=599).contains(&status)
    {
        return Err(invalid());
    }
    let authority = url
        .strip_prefix("http://")
        .and_then(|s| s.split(['/', '?', '#']).next())
        .ok_or_else(invalid)?;
    let address: std::net::SocketAddr = authority.parse().map_err(|_| invalid())?;
    if !address.ip().is_loopback() || address.port() == 0 || url.contains('#') {
        return Err(invalid());
    }
    if let HttpResponseCheck::Json {
        equals,
        nonempty_arrays,
    } = check
    {
        let mut keys = BTreeSet::new();
        if equals.len() + nonempty_arrays.len() == 0 || equals.len() + nonempty_arrays.len() > 8 {
            return Err(invalid());
        }
        for key in equals.keys().chain(nonempty_arrays.iter()) {
            if key.is_empty()
                || key.len() > 128
                || key.chars().any(char::is_control)
                || !keys.insert(key)
            {
                return Err(invalid());
            }
        }
        if equals
            .values()
            .any(|v| matches!(v, HttpLiteral::String(s) if s.len() > 256))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

pub fn validate(probe: &HttpReadiness) -> Result<()> {
    if !cfg!(feature = "http-probes") {
        return Err(Error::new(
            "CONFIG_INVALID",
            "This build does not support HTTP readiness",
        ));
    }
    validate_target(&probe.url, probe.status, &probe.check)?;
    if !(1..=86400).contains(&probe.interval_seconds)
        || !(1..=10).contains(&probe.timeout_seconds)
        || !(1..=1000).contains(&probe.failure_threshold)
        || !(1..=1000).contains(&probe.success_threshold)
    {
        return Err(invalid());
    }
    Ok(())
}

pub fn validate_policy(policy: &ExecutorPolicy) -> Result<()> {
    if policy.systemd_http_readiness.len() > 256 {
        return Err(invalid());
    }
    let mut units = BTreeSet::new();
    for entry in &policy.systemd_http_readiness {
        validate_target(&entry.url, entry.status, &entry.check)?;
        if !units.insert(&entry.unit)
            || !policy.systemd_enabled
            || !policy
                .scoped_units
                .iter()
                .any(|u| u.unit == entry.unit && u.scope == entry.scope)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

pub fn authorize(
    policy: &ExecutorPolicy,
    handle: &ExternalHandle,
    probe: &HttpReadiness,
) -> Result<()> {
    validate(probe)?;
    validate_policy(policy)?;
    crate::external::systemd::authorize(policy, handle)?;
    if let ExternalHandle::SystemdV2 {
        unit,
        scope,
        service: true,
        ..
    } = handle
        && policy.systemd_http_readiness.iter().any(|p| {
            &p.unit == unit
                && p.scope == *scope
                && p.url == probe.url
                && p.status == probe.status
                && p.check == probe.check
        })
    {
        return Ok(());
    }
    Err(Error::new(
        "NOT_AUTHORIZED",
        "Executor policy does not permit this HTTP readiness check",
    ))
}

pub fn probe(
    policy: &ExecutorPolicy,
    handle: &ExternalHandle,
    spec: &HttpReadiness,
) -> Result<bool> {
    authorize(policy, handle, spec)?;
    #[cfg(feature = "http-probes")]
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| {
                Error::new(
                    "EXECUTOR_UNAVAILABLE",
                    "HTTP readiness transport is unavailable",
                )
            })?;
        Ok(runtime.block_on(check_http(spec)).unwrap_or(false))
    }
    #[cfg(not(feature = "http-probes"))]
    Err(Error::new(
        "CONFIG_INVALID",
        "This build does not support HTTP readiness",
    ))
}

#[cfg(feature = "http-probes")]
async fn check_http(spec: &HttpReadiness) -> std::result::Result<bool, reqwest::Error> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .http1_only()
        .http1_max_headers(32)
        .timeout(std::time::Duration::from_secs(spec.timeout_seconds))
        .build()?;
    let mut response = client.get(&spec.url).send().await?;
    if response.status().as_u16() != spec.status
        || response
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        || response
            .headers()
            .iter()
            .map(|(name, value)| name.as_str().len() + value.len())
            .sum::<usize>()
            > 16_384
        || response
            .headers()
            .get("content-encoding")
            .is_some_and(|v| v != "identity")
    {
        return Ok(false);
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let mut body = Vec::new();
    while let Some(bytes) = response.chunk().await? {
        if body.len().saturating_add(bytes.len()) > MAX_RESPONSE_BYTES {
            return Ok(false);
        }
        body.extend_from_slice(&bytes);
    }
    Ok(matches_body(&spec.check, &content_type, &body))
}

#[cfg(feature = "http-probes")]
fn matches_body(check: &HttpResponseCheck, content_type: &str, body: &[u8]) -> bool {
    let mime = content_type.split(';').next().unwrap_or_default().trim();
    match check {
        HttpResponseCheck::Status => true,
        HttpResponseCheck::Html => {
            if !mime.eq_ignore_ascii_case("text/html") {
                return false;
            }
            std::str::from_utf8(body).is_ok_and(|s| {
                let s = s.trim_start().to_ascii_lowercase();
                (s.starts_with("<!doctype html>") || s.starts_with("<html"))
                    && s.contains("<html")
                    && s.contains("</html>")
            })
        }
        HttpResponseCheck::Json {
            equals,
            nonempty_arrays,
        } => {
            if !mime.eq_ignore_ascii_case("application/json") {
                return false;
            }
            let Ok(serde_json::Value::Object(object)) =
                serde_json::from_slice::<serde_json::Value>(body)
            else {
                return false;
            };
            equals.iter().all(|(key, value)| {
                object.get(key).is_some_and(|actual| match value {
                    HttpLiteral::Boolean(v) => actual.as_bool() == Some(*v),
                    HttpLiteral::Number(v) => actual.as_i64() == Some(*v),
                    HttpLiteral::String(v) => actual.as_str() == Some(v.as_str()),
                })
            }) && nonempty_arrays.iter().all(|key| {
                object
                    .get(key)
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|rows| !rows.is_empty())
            })
        }
    }
}
