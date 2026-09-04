//! Civil dates and the two spellings the panels use.
//!
//! No timezone: the store's timestamps are unix seconds and the demo world
//! is naïve. Everything here is pure arithmetic, so it holds under a virtual
//! clock as well as a real one.

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Days from civil date (Howard Hinnant's algorithm), epoch 1970-01-01.
#[must_use]
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Civil date from days since the epoch (the inverse of the above).
#[must_use]
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A timestamp from a civil date-time.
#[must_use]
pub fn ts(y: i64, mo: u32, d: u32, h: u32, min: u32) -> f64 {
    (days_from_civil(y, mo, d) * 86_400 + i64::from(h) * 3_600 + i64::from(min) * 60) as f64
}

/// The list style: `aug 31 09:14`.
#[must_use]
pub fn fmt_date(ts: f64) -> String {
    let secs = ts as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (_, m, d) = civil_from_days(days);
    let (h, min) = (rem / 3_600, (rem % 3_600) / 60);
    format!("{} {d:02} {h:02}:{min:02}", MONTHS[(m - 1) as usize])
}

/// The date written out, with the year the list style leaves off:
/// `31 Aug 2026 at 09:14`.
#[must_use]
pub fn fmt_date_long(ts: f64) -> String {
    let secs = ts as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    let (h, min) = (rem / 3_600, (rem % 3_600) / 60);
    let mut mon = MONTHS[(m - 1) as usize].to_string();
    mon[..1].make_ascii_uppercase();
    format!("{d} {mon} {y} at {h:02}:{min:02}")
}

/// Where a virtual clock starts: the instant a headless run and every
/// library mount believe it is. Fixed, so a run is reproducible down to the
/// dates it draws — and public because a fixture that plants a *deadline*
/// (the effect queue's `not_before`) has to place it against this, not
/// against the wall.
///
/// It names no app: it is a date, and the seeds are written around it.
#[must_use]
pub fn virtual_epoch() -> f64 {
    ts(2026, 9, 1, 12, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip() {
        for (y, m, d) in [
            (1970, 1, 1),
            (2000, 2, 29),
            (2026, 9, 1),
            (2026, 12, 31),
            (1969, 12, 31),
        ] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d), "{y}-{m}-{d}");
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn the_two_spellings() {
        let t = ts(2026, 8, 31, 9, 14);
        assert_eq!(fmt_date(t), "aug 31 09:14");
        assert_eq!(fmt_date_long(t), "31 Aug 2026 at 09:14");
        assert_eq!(fmt_date(0.0), "jan 01 00:00");
    }

    #[test]
    fn the_virtual_epoch_is_fixed() {
        assert_eq!(virtual_epoch(), ts(2026, 9, 1, 12, 0));
        assert_eq!(fmt_date(virtual_epoch()), "sep 01 12:00");
    }
}
