//! Wall-clock timestamps in the exact shape the Python pipeline emits:
//! `datetime.now(UTC).replace(microsecond=0).isoformat()` →
//! `YYYY-MM-DDTHH:MM:SS+00:00`.
//!
//! Parity-first decision (python-retirement Phase 2): the port keeps the
//! real wall-clock stamps; making enhance deploy fully deterministic is a
//! separate, deliberate later change.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current UTC time in Python `isoformat()` shape with `+00:00` offset.
pub fn now_iso_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso_utc_from_unix(secs)
}

/// Format a unix timestamp (seconds) as `YYYY-MM-DDTHH:MM:SS+00:00`.
pub fn iso_utc_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}+00:00")
}

/// Days-since-epoch → (year, month, day). Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(iso_utc_from_unix(0), "1970-01-01T00:00:00+00:00");
        // 2026-07-28T21:15:42Z
        assert_eq!(
            iso_utc_from_unix(1_785_273_342),
            "2026-07-28T21:15:42+00:00"
        );
        // Leap-year boundary: 2024-02-29T23:59:59Z
        assert_eq!(
            iso_utc_from_unix(1_709_251_199),
            "2024-02-29T23:59:59+00:00"
        );
    }
}
