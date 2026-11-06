use anyhow::{Result, bail};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use serde_json::Value;
use std::time::Instant;

/// Accumulate wall time without rounding away short operations.
pub(crate) fn measure<T>(total_ms: &mut f64, operation: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = operation();
    *total_ms += start.elapsed().as_secs_f64() * 1000.0;
    result
}

/// Parallel reads overlap: count the union of their intervals, not worker sums.
pub(crate) fn union_ms(mut intervals: Vec<(Instant, Instant)>) -> f64 {
    intervals.sort_unstable_by_key(|span| span.0);
    let mut merged: Option<(Instant, Instant)> = None;
    let mut total = 0.0;
    for (start, end) in intervals {
        match &mut merged {
            Some((_, previous_end)) if start <= *previous_end => {
                *previous_end = (*previous_end).max(end);
            }
            _ => {
                if let Some((start, end)) = merged {
                    total += end.duration_since(start).as_secs_f64() * 1000.0;
                }
                merged = Some((start, end));
            }
        }
    }
    if let Some((start, end)) = merged {
        total += end.duration_since(start).as_secs_f64() * 1000.0;
    }
    total
}

pub fn millis_iso(millis: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_millis(millis)
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}

pub fn iso(value: &Value) -> Option<String> {
    if let Some(n) = value.as_i64() {
        return millis_iso(if n.unsigned_abs() < 1_000_000_000_000 {
            n.checked_mul(1000)?
        } else {
            n
        });
    }
    let text = value.as_str()?;
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return millis_iso(dt.timestamp_millis());
    }
    let dt = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
    millis_iso(
        Local
            .from_local_datetime(&dt)
            .earliest()?
            .timestamp_millis(),
    )
}

pub fn ms(value: &Option<String>) -> i64 {
    value
        .as_deref()
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.timestamp_millis())
        .unwrap_or(i64::MIN)
}

pub fn boundary(value: Option<&str>, end: bool, now: i64) -> Result<Option<i64>> {
    let Some(value) = value else { return Ok(None) };
    if value == "now" {
        return Ok(Some(now));
    }
    if value.len() >= 2 && value.as_bytes().last().is_some_and(u8::is_ascii_alphabetic) {
        let (digits, unit) = value.split_at(value.len() - 1);
        let multiplier = match unit.to_ascii_lowercase().as_str() {
            "d" => Some(86_400_000i64),
            "h" => Some(3_600_000),
            "m" => Some(60_000),
            _ => None,
        };
        if let Some(multiplier) = multiplier
            && !digits.is_empty()
            && digits.bytes().all(|c| c.is_ascii_digit())
        {
            return digits
                .parse::<i64>()
                .ok()
                .and_then(|n| n.checked_mul(multiplier))
                .and_then(|n| now.checked_sub(n))
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("Time range is too large: {value}"));
        }
    }
    if value.len() == 10 {
        let day = NaiveDate::parse_from_str(value, "%Y-%m-%d")?;
        let day = if end {
            day.succ_opt()
                .ok_or_else(|| anyhow::anyhow!("Date out of range"))?
        } else {
            day
        };
        let dt = Local
            .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .ok_or_else(|| {
                anyhow::anyhow!("Time does not exist in the local time zone: {value}")
            })?;
        return Ok(Some(dt.timestamp_millis()));
    }
    if value.contains('T')
        && let Some(value) = iso(&Value::String(value.to_owned()))
    {
        return Ok(Some(ms(&Some(value))));
    }
    bail!("Invalid time: {value}. Use YYYY-MM-DD, an ISO timestamp, or 7d / 24h");
}

pub fn local(value: &Option<String>) -> String {
    value
        .as_deref()
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

#[cfg(test)]
mod read_timing_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parallel_read_intervals_count_overlaps_once() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        assert_eq!(union_ms(vec![]), 0.0);
        assert_eq!(
            union_ms(vec![
                (at(20), at(25)),
                (at(3), at(8)),
                (at(0), at(5)),
                (at(1), at(2))
            ]),
            13.0
        );
    }
}
