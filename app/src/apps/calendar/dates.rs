//! Civil dates and IANA zones; all-day end dates are exclusive on the wire.
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};

pub fn zone(name: &str) -> Result<Tz, String> {
    name.parse()
        .map_err(|_| format!("unknown time zone: {name}"))
}
pub fn date(s: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| "use a valid date, YYYY-MM-DD".into())
}
pub fn instant(s: &str, tz: &str) -> Result<f64, String> {
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Ok(d.timestamp() as f64);
    }
    let d = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").map_err(|_| {
        "use YYYY-MM-DDTHH:MM or a date and time with an explicit UTC offset".to_string()
    })?;
    zone(tz)?
        .from_local_datetime(&d)
        .single()
        .map(|d| d.timestamp() as f64)
        .ok_or(
            "this local time is skipped or repeated by daylight saving; use an explicit UTC offset"
                .into(),
        )
}
pub fn utc(t: f64) -> DateTime<Utc> {
    DateTime::from_timestamp(t as i64, 0).unwrap_or(DateTime::UNIX_EPOCH)
}
pub fn rfc(t: f64) -> String {
    utc(t).to_rfc3339()
}
pub fn day(t: f64, tz: &str) -> String {
    utc(t)
        .with_timezone(&zone(tz).unwrap_or(chrono_tz::UTC))
        .format("%Y-%m-%d")
        .to_string()
}
pub fn local(t: f64, tz: &str) -> String {
    utc(t)
        .with_timezone(&zone(tz).unwrap_or(chrono_tz::UTC))
        .format("%Y-%m-%dT%H:%M")
        .to_string()
}
/// Preserve the offset of an existing event during a repeated DST hour.
pub fn editor_time(t: f64, tz: &str) -> String {
    let local = local(t, tz);
    if instant(&local, tz).is_ok_and(|parsed| (parsed - t).abs() < 60.0) {
        local
    } else {
        utc(t)
            .with_timezone(&zone(tz).unwrap_or(chrono_tz::UTC))
            .to_rfc3339()
    }
}
pub fn label(t: f64, tz: &str) -> String {
    utc(t)
        .with_timezone(&zone(tz).unwrap_or(chrono_tz::UTC))
        .format("%a %d %b · %H:%M %Z")
        .to_string()
}
pub fn midnight(d: NaiveDate, tz: &str) -> Result<f64, String> {
    zone(tz)?
        .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|t| t.timestamp() as f64)
        .ok_or("this date has no midnight in its time zone".into())
}
pub fn read(v: &Value, fallback: &str) -> Result<(f64, bool), String> {
    if let Some(d) = v["date"].as_str() {
        return midnight(date(d)?, fallback).map(|t| (t, true));
    }
    instant(
        v["dateTime"].as_str().ok_or("event has no start/end")?,
        v["timeZone"].as_str().unwrap_or(fallback),
    )
    .map(|t| (t, false))
}
pub fn wire(s: &str, tz: &str, all_day: bool) -> Result<Value, String> {
    if all_day {
        date(s)?;
        Ok(json!({"date":s}))
    } else {
        Ok(json!({"dateTime":rfc(instant(s,tz)?),"timeZone":tz}))
    }
}
pub fn month(s: &str, delta: i32) -> NaiveDate {
    let d = date(s).unwrap_or_else(|_| utc(0.0).date_naive());
    let months = d.year() * 12 + d.month0() as i32 + delta;
    NaiveDate::from_ymd_opt(months.div_euclid(12), months.rem_euclid(12) as u32 + 1, 1).unwrap_or(d)
}
pub fn grid(s: &str) -> Vec<NaiveDate> {
    let first = month(s, 0);
    let start = first - Duration::days(first.weekday().num_days_from_monday() as i64);
    (0..42).map(|i| start + Duration::days(i)).collect()
}
