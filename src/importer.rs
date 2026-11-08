//! Passive Cronicle inventory/classification; migration never activates workloads.
use crate::{Error, Result, config::Manifest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Serialize, Deserialize)]
pub struct ImportReport {
    pub dry_run: bool,
    pub source: String,
    pub jobs: Vec<ImportJob>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct ImportJob {
    pub source_id: String,
    pub name: String,
    pub status: String,
    pub issues: Vec<String>,
    pub expression: Option<String>,
    pub timezone: String,
    pub next_due_utc_microseconds: Vec<i64>,
    pub manifest: Option<Manifest>,
}
fn invalid() -> Error {
    Error::new(
        "IMPORT_INVALID",
        "Expected a bounded Cronicle event list or export with events/items",
    )
}

pub fn inspect(text: &str, namespace: &str, now_us: i64) -> Result<ImportReport> {
    if text.len() > 4_194_304 || namespace.len() > 128 {
        return Err(invalid());
    }
    let value: Value = serde_json::from_str(text).map_err(|_| invalid())?;
    let events = value
        .as_array()
        .or_else(|| value.get("events").and_then(Value::as_array))
        .or_else(|| value.get("items").and_then(Value::as_array))
        .ok_or_else(invalid)?;
    if events.len() > 1000 {
        return Err(invalid());
    }
    let mut jobs = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let source_id = event
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unidentified")
            .chars()
            .take(128)
            .collect::<String>();
        let title = event
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(&source_id);
        let mut name = title
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>();
        name = name.trim_matches('-').chars().take(100).collect();
        if name.is_empty() {
            name = format!("imported-{}", index + 1);
        }
        let timezone = event
            .get("timezone")
            .and_then(Value::as_str)
            .unwrap_or("UTC")
            .to_owned();
        let mut job = ImportJob {
            source_id,
            name,
            status: "REQUIRES_REVIEW".into(),
            issues: Vec::new(),
            expression: None,
            timezone,
            next_due_utc_microseconds: Vec::new(),
            manifest: None,
        };
        if event.get("systemd_unit").is_some()
            || event.get("sandbox").is_some()
            || event.get("privileged") == Some(&Value::Bool(true))
        {
            job.status = "SECURITY_GAP".into();
            job.issues.push("Preserve the existing lifecycle owner and hardened OS security profile; raw-process conversion is prohibited".into());
            jobs.push(job);
            continue;
        }
        let timing = event.get("timing").and_then(Value::as_object);
        if timing.is_some_and(|map| map.contains_key("years")) {
            job.status = "UNSUPPORTED".into();
            job.issues
                .push("Cronicle years constraints are not expressible by five-field cron".into());
            jobs.push(job);
            continue;
        }
        let timing = match timing {
            Some(timing) => timing,
            None => {
                job.status = "UNSUPPORTED".into();
                job.issues.push(
                    "Demand/plugin-triggered event has no reconstructable periodic timing".into(),
                );
                jobs.push(job);
                continue;
            }
        };
        if timing.keys().any(|key| {
            !matches!(
                key.as_str(),
                "minutes" | "hours" | "days" | "months" | "weekdays"
            )
        }) {
            job.status = "UNSUPPORTED".into();
            job.issues
                .push("Unknown Cronicle timing fields require explicit parity review".into());
            jobs.push(job);
            continue;
        }
        let fields = [
            ("minutes", 0, 59),
            ("hours", 0, 23),
            ("days", 1, 31),
            ("months", 1, 12),
            ("weekdays", 0, 6),
        ];
        let mut expression = Vec::new();
        let mut valid = true;
        for (field, min, max) in fields {
            match timing.get(field) {
                None => expression.push("*".to_owned()),
                Some(Value::Array(values)) if values.is_empty() => expression.push("*".to_owned()),
                Some(Value::Array(values)) if values.len() <= 60 => {
                    let mut numbers = Vec::new();
                    for value in values {
                        match value.as_i64() {
                            Some(number) if (min..=max).contains(&number) => numbers.push(number),
                            _ => valid = false,
                        }
                    }
                    numbers.sort_unstable();
                    numbers.dedup();
                    expression.push(
                        numbers
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(","),
                    );
                }
                _ => valid = false,
            }
        }
        if !valid {
            job.status = "UNSUPPORTED".into();
            job.issues
                .push("Timing fields contain invalid ranges or noninteger values".into());
            jobs.push(job);
            continue;
        }
        let expression = expression.join(" ");
        job.expression = Some(expression.clone());
        if timing.contains_key("days") && timing.contains_key("weekdays") {
            job.issues.push("DOM/DOW combination must be compared against the source scheduler; CronHarbor uses Vixie OR".into());
        }
        let script = event.pointer("/params/script").and_then(Value::as_str);
        let argv = script.and_then(simple_argv);
        let (command, args) = match argv {
            Some((command, args)) => (command, args),
            None => {
                job.status = "UNSUPPORTED".into();
                job.issues.push("Script requires an explicit executable file and argv review; shell text is neither exposed nor automatically converted".into());
                ("/bin/true".to_owned(), Vec::new())
            }
        };
        let mut proposal = json!({"apiVersion":"cronharbor.dev/v1","kind":"ScheduledJob","metadata":{"name":job.name,"namespace":namespace,"labels":{"migration.source":"cronicle"}},"spec":{"owner":"file","enabled":false,"schedule":{"type":"cron","expression":expression,"timezone":job.timezone,"fold":"first","gap":"skip","misfire":{"policy":"skip","latenessGraceSeconds":60,"maxAgeSeconds":3600,"maxOccurrences":1}},"execution":{"adapter":"process","command":command,"args":args,"workingDirectory":"/tmp","sandbox":"default"},"concurrency":{"mode":"forbid","maxActive":1},"timeoutSeconds":event.get("timeout").and_then(Value::as_u64).filter(|value|*value>0).unwrap_or(300),"retry":{"maxAttempts":1},"logs":{"maxRunBytes":10485760}}});
        if event
            .get("max_children")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            > 1
        {
            proposal["spec"]["concurrency"] =
                json!({"mode":"allow","maxActive":event["max_children"]});
        }
        if event.get("catch_up").and_then(Value::as_i64) == Some(1)
            || event.get("catch_up") == Some(&Value::Bool(true))
        {
            job.issues.push("Source catch-up is enabled: choose a bounded recovery window explicitly before cutover".into());
        }
        match Manifest::parse(&proposal.to_string()) {
            Ok(manifest) => {
                let mut cursor = now_us;
                for _ in 0..100 {
                    match manifest.next_after(cursor) {
                        Ok(next) => {
                            job.next_due_utc_microseconds.push(next);
                            cursor = next
                        }
                        Err(_) => break,
                    }
                }
                if job.status != "UNSUPPORTED" {
                    job.manifest = Some(manifest);
                }
            }
            Err(_) => {
                job.status = "UNSUPPORTED".into();
                job.issues.push(
                    "Source schedule/limits cannot be validated by the current workload schema"
                        .into(),
                );
            }
        }
        job.issues.push("Source templates do not prove live ownership, credentials, cwd, sandbox or side-effect safety; verify before enabling".into());
        jobs.push(job);
    }
    Ok(ImportReport{dry_run:true,source:"cronicle-export".into(),jobs,warnings:vec!["No event was executed, enabled, applied or disabled in either scheduler".into(),"Schedule previews are CronHarbor projections, not proof of source parity; compare the next 100 source UTC instants before cutover".into()]})
}

// Only literal argv without shell syntax is portable; complex scripts require a reviewed executable file.
fn simple_argv(script: &str) -> Option<(String, Vec<String>)> {
    if script.len() > 16_384
        || script
            .chars()
            .any(|c| c.is_control() || "'\"`$;&|<>\\(){}*?[]~#".contains(c))
    {
        return None;
    }
    let mut parts = script.split_whitespace();
    let command = parts.next()?;
    if !command.starts_with('/') || command.contains('=') {
        return None;
    }
    Some((command.to_owned(), parts.map(ToOwned::to_owned).collect()))
}

pub fn markdown(report: &ImportReport) -> String {
    let mut output = String::from(
        "# Cronicle dry-run classification\n\nNo workloads were activated. Source parity and exclusive ownership still require verification.\n\n| Source ID | Workload | Status | Issues |\n|---|---|---|---|\n",
    );
    for job in &report.jobs {
        let escape = |s: &str| s.replace('|', "\\|").replace(['\n', '\r'], " ");
        output.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            escape(&job.source_id),
            escape(&job.name),
            job.status,
            escape(&job.issues.join("; "))
        ));
    }
    output
}
