//! Pure schedule calculations. Cron is evaluated in wall time, then resolved explicitly.
use crate::{Error, Result, config::Schedule};
use chrono::{DateTime, Duration, LocalResult, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;
use std::str::FromStr;
fn invalid() -> Error {
    Error::new(
        "SCHEDULE_INVALID",
        "Schedule has no occurrence in the supported date range",
    )
}
pub fn next_after(schedule: &Schedule, timestamp: i64) -> Result<i64> {
    search(schedule, timestamp, true)
}
pub fn previous_at_or_before(schedule: &Schedule, timestamp: i64) -> Result<i64> {
    search(schedule, timestamp, false)
}
fn search(schedule: &Schedule, timestamp: i64, forward: bool) -> Result<i64> {
    if schedule.r#type == "interval" {
        let interval = i64::try_from(schedule.interval_seconds)
            .ok()
            .and_then(|v| v.checked_mul(1_000_000))
            .filter(|v| *v > 0)
            .ok_or_else(invalid)?;
        let delta = timestamp
            .checked_sub(schedule.anchor_at_us)
            .ok_or_else(invalid)?;
        let quotient = delta.div_euclid(interval);
        let index = if forward {
            quotient.checked_add(1).ok_or_else(invalid)?
        } else {
            quotient
        };
        return index
            .checked_mul(interval)
            .and_then(|v| v.checked_add(schedule.anchor_at_us))
            .ok_or_else(invalid);
    }
    let tz = Tz::from_str(&schedule.timezone).map_err(|_| invalid())?;
    let cron = Cron::from_str(&schedule.expression).map_err(|_| invalid())?;
    let time = DateTime::<Utc>::from_timestamp_micros(timestamp).ok_or_else(invalid)?;
    if tz == Tz::UTC {
        return (if forward {
            cron.find_next_occurrence(&time, false)
        } else {
            cron.find_previous_occurrence(&time, true)
        })
        .map(|t| t.timestamp_micros())
        .map_err(|_| invalid());
    }
    // IANA offsets and date-line transitions fit within 26h. Looking behind covers both fold halves.
    let local = time.with_timezone(&tz).naive_local().and_utc();
    let mut cursor = if forward {
        local.checked_sub_signed(Duration::hours(26))
    } else {
        local.checked_add_signed(Duration::hours(26))
    }
    .ok_or_else(invalid)?;
    let mut best: Option<i64> = None;
    for _ in 0..10_000 {
        let candidate = if forward {
            cron.find_next_occurrence(&cursor, false)
        } else {
            cron.find_previous_occurrence(&cursor, false)
        }
        .map_err(|_| invalid())?;
        let naive = candidate.naive_utc();
        let resolved = match tz.from_local_datetime(&naive) {
            LocalResult::Single(t) => vec![t],
            LocalResult::Ambiguous(a, b) => match schedule.fold.as_str() {
                "second" => vec![a.max(b)],
                "both" => vec![a, b],
                _ => vec![a.min(b)],
            },
            LocalResult::None if schedule.gap == "next_valid" => {
                let mut t = naive;
                let mut result = Vec::new();
                for _ in 0..1560 {
                    t = t
                        .checked_add_signed(Duration::minutes(1))
                        .ok_or_else(invalid)?;
                    if let Some(valid) = tz.from_local_datetime(&t).earliest() {
                        result.push(valid);
                        break;
                    }
                }
                result
            }
            LocalResult::None => Vec::new(),
        };
        for resolved in resolved {
            let value = resolved.timestamp_micros();
            if (forward && value > timestamp) || (!forward && value <= timestamp) {
                best = Some(match best {
                    Some(old) if forward => old.min(value),
                    Some(old) => old.max(value),
                    None => value,
                });
            }
        }
        cursor = candidate;
        if let Some(found) = best {
            let boundary = if forward {
                found.saturating_add(26 * 3600 * 1_000_000)
            } else {
                found.saturating_sub(26 * 3600 * 1_000_000)
            };
            if (forward && cursor.timestamp_micros() > boundary)
                || (!forward && cursor.timestamp_micros() < boundary)
            {
                return Ok(found);
            }
        }
    }
    best.ok_or_else(invalid)
}
