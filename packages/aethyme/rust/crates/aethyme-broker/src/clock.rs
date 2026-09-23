//! Wall-clock helpers shared by modules that stamp durable records.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch, or 0 if the clock reads before it.
pub(crate) fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
