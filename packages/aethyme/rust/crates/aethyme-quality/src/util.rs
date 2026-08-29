//! Scan-id and timestamp helpers.

use std::fs::File;
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

/// A random UUIDv4 in the canonical lowercase-hyphenated form Python's
/// `str(uuid.uuid4())` produces. Entropy from `/dev/urandom` (the
/// workspace carries no rand crate; scan ids only need uniqueness).
pub fn uuid4() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut f) = File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    } else {
        // Last-resort fallback: clock-derived. Still unique enough for
        // a scan id; never used on platforms with /dev/urandom.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        bytes[..16].copy_from_slice(&now.as_nanos().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
    let h = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        h(&bytes[0..4]),
        h(&bytes[4..6]),
        h(&bytes[6..8]),
        h(&bytes[8..10]),
        h(&bytes[10..16])
    )
}

/// Current UTC instant in the two renderings the report needs:
/// `datetime.now(UTC).isoformat()` (`YYYY-MM-DDTHH:MM:SS.ffffff+00:00`)
/// and `strftime('%Y-%m-%d %H:%M:%S UTC')`.
pub fn now_timestamps() -> (String, String) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let micros = now.subsec_micros();
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    // Python isoformat omits `.ffffff` when microsecond == 0; matched
    // here for exactness (a once-in-a-million cosmetic case).
    let iso = if micros == 0 {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}+00:00")
    } else {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{micros:06}+00:00")
    };
    let display = format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC");
    (iso, display)
}

/// Days-since-epoch → (year, month, day). Howard Hinnant's
/// civil_from_days (same algorithm as `aethyme_enhance::timeutil`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Python `str.strip()` (no args): trim Unicode whitespace from both
/// ends. `char::is_whitespace` is the closest Rust equivalent (see the
/// dialect-audit note in `detectors/mod.rs`).
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

/// Python `s[:n]` character slicing.
pub fn py_slice(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid4_shape() {
        let u = uuid4();
        assert_eq!(u.len(), 36);
        let parts: Vec<&str> = u.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'));
        assert_ne!(uuid4(), uuid4());
    }

    #[test]
    fn timestamps_shape() {
        let (iso, display) = now_timestamps();
        assert!(iso.ends_with("+00:00"));
        assert!(iso.contains('T'));
        assert!(display.ends_with(" UTC"));
    }

    #[test]
    fn slicing_and_strip() {
        assert_eq!(py_slice("héllo wörld", 7), "héllo w");
        assert_eq!(py_strip("  a b \t"), "a b");
    }
}
