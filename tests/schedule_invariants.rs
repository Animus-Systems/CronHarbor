use chrono::{NaiveDate, TimeZone};
use chrono_tz::Tz;
use cronharbor::{config::Manifest, scheduler};

fn manifest() -> Manifest {
    Manifest::parse(include_str!("../examples/hello.yaml")).unwrap()
}

#[test]
fn interval_neighbors_preserve_alignment_and_order_across_the_anchor() {
    let mut manifest = manifest();
    let schedule = &mut manifest.spec.schedule;
    schedule.r#type = "interval".into();
    for anchor in [-10_000_000_i64, 0, 10_000_000] {
        schedule.anchor_at_us = anchor;
        for seconds in [1_u64, 7, 60, 3600] {
            schedule.interval_seconds = seconds;
            let interval = i64::try_from(seconds).unwrap() * 1_000_000;
            for period in [-100, -1, 0, 1, 100] {
                for offset in [-1, 0, 1, interval - 1] {
                    let timestamp = anchor + period * interval + offset;
                    let next = scheduler::next_after(schedule, timestamp).unwrap();
                    let previous = scheduler::previous_at_or_before(schedule, timestamp).unwrap();
                    assert!(previous <= timestamp && timestamp < next);
                    assert_eq!(next - previous, interval);
                    assert_eq!((next - anchor).rem_euclid(interval), 0);
                    assert_eq!(
                        scheduler::previous_at_or_before(schedule, next).unwrap(),
                        next
                    );
                    assert_eq!(scheduler::next_after(schedule, previous).unwrap(), next);
                }
            }
        }
    }
    schedule.anchor_at_us = 0;
    schedule.interval_seconds = 1;
    assert!(scheduler::next_after(schedule, i64::MAX).is_err());
    assert!(scheduler::previous_at_or_before(schedule, i64::MIN).is_err());
    for seconds in [0, u64::MAX] {
        schedule.interval_seconds = seconds;
        assert!(scheduler::next_after(schedule, 0).is_err());
        assert!(scheduler::previous_at_or_before(schedule, 0).is_err());
    }
}

#[test]
fn half_hour_dst_fold_preserves_both_utc_instants_and_reverse_lookup() {
    let zone: Tz = "Australia/Lord_Howe".parse().unwrap();
    let local = NaiveDate::from_ymd_opt(2026, 4, 5)
        .unwrap()
        .and_hms_opt(1, 45, 0)
        .unwrap();
    let ambiguous = zone.from_local_datetime(&local);
    let first = ambiguous.earliest().unwrap().timestamp_micros();
    let second = ambiguous.latest().unwrap().timestamp_micros();
    assert_eq!(second - first, 30 * 60 * 1_000_000);
    let mut manifest = manifest();
    let schedule = &mut manifest.spec.schedule;
    schedule.timezone = "Australia/Lord_Howe".into();
    schedule.expression = "45 1 * * *".into();
    for (fold, expected) in [("first", first), ("second", second)] {
        schedule.fold = fold.into();
        assert_eq!(
            scheduler::next_after(schedule, first - 1).unwrap(),
            expected
        );
        assert_eq!(
            scheduler::previous_at_or_before(schedule, second).unwrap(),
            expected
        );
    }
    schedule.fold = "both".into();
    assert_eq!(scheduler::next_after(schedule, first).unwrap(), second);
    assert_eq!(
        scheduler::previous_at_or_before(schedule, second).unwrap(),
        second
    );
    assert_eq!(
        scheduler::previous_at_or_before(schedule, second - 1).unwrap(),
        first
    );
}

#[test]
fn date_line_gap_skips_a_missing_day_or_coalesces_to_the_first_valid_minute() {
    let zone: Tz = "Pacific/Apia".parse().unwrap();
    let local = |day, hour| {
        NaiveDate::from_ymd_opt(2011, 12, day)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap()
    };
    assert!(zone.from_local_datetime(&local(30, 12)).single().is_none());
    let from = zone
        .from_local_datetime(&local(29, 13))
        .single()
        .unwrap()
        .timestamp_micros();
    let midnight = zone
        .from_local_datetime(&local(31, 0))
        .single()
        .unwrap()
        .timestamp_micros();
    let noon = zone
        .from_local_datetime(&local(31, 12))
        .single()
        .unwrap()
        .timestamp_micros();
    let mut manifest = manifest();
    let schedule = &mut manifest.spec.schedule;
    schedule.timezone = "Pacific/Apia".into();
    schedule.expression = "0 12 * * *".into();
    schedule.gap = "skip".into();
    assert_eq!(scheduler::next_after(schedule, from).unwrap(), noon);
    schedule.gap = "next_valid".into();
    assert_eq!(scheduler::next_after(schedule, from).unwrap(), midnight);
    assert_eq!(scheduler::next_after(schedule, midnight).unwrap(), noon);
    assert_eq!(
        scheduler::previous_at_or_before(schedule, midnight).unwrap(),
        midnight
    );
}
