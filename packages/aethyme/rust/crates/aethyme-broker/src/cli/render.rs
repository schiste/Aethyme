//! Formatting and process helpers shared by several command groups.

use super::*;

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(super) fn print_checks(checks: &[crate::init::Check]) {
    for check in checks {
        let tag = match check.status {
            crate::init::CheckStatus::Pass => "pass",
            crate::init::CheckStatus::Created => "created",
            crate::init::CheckStatus::Warn => "warn",
            crate::init::CheckStatus::Fail => "FAIL",
            crate::init::CheckStatus::Skipped => "skip",
        };
        out!("{tag:<8} {:<28} {}", check.id, check.detail);
    }
}

pub(super) fn duration_label(duration_ms: Option<i64>) -> String {
    duration_ms
        .map(|ms| format!("{ms}ms"))
        .unwrap_or_else(|| "-".into())
}

pub(super) fn print_overlap_warnings(overlaps: &[crate::Overlap]) {
    for overlap in overlaps {
        eprintln!(
            "⚠ overlap: sessions {} and {} are both touching {}",
            overlap.session_a, overlap.session_b, overlap.path
        );
    }
}

pub(super) fn print_promoted_conflict_warnings(conflicts: &[crate::PromotedConflict]) {
    for conflict in conflicts {
        eprintln!(
            "⚠ promoted conflict: session {} is touching {}; integration already changed {}",
            conflict.session_id, conflict.session_path, conflict.promoted_path
        );
    }
}

pub(super) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0_usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub(super) fn render_capped<T>(items: &[T], cap: usize, detail: bool, mut render: impl FnMut(&T)) {
    let shown = if detail {
        items.len()
    } else {
        cap.min(items.len())
    };
    for item in &items[..shown] {
        render(item);
    }
    if shown < items.len() {
        out!(
            "    ... and {} more; rerun with --detail to list them",
            items.len() - shown
        );
    }
}

pub(super) fn short_commit(commit: &str) -> &str {
    &commit[..12.min(commit.len())]
}

pub(super) fn capped_join(values: &[String], limit: usize) -> String {
    let mut shown: Vec<String> = values.iter().take(limit).cloned().collect();
    if values.len() > shown.len() {
        shown.push(format!("and {} more", values.len() - shown.len()));
    }
    shown.join(", ")
}

pub(super) fn plural<'a>(count: usize, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 { singular } else { plural }
}

pub(super) fn short(commit: &str) -> &str {
    &commit[..12.min(commit.len())]
}

/// Print one JSON value, pretty.
pub(super) fn print_json(value: &serde_json::Value) -> Result<(), UsageError> {
    out!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| UsageError::Message(e.to_string()))?
    );
    Ok(())
}

pub(super) fn to_usage<E: std::fmt::Display>(error: E) -> UsageError {
    UsageError::Message(error.to_string())
}

pub(super) fn git_output(root: &Path, args: &[&str]) -> Result<String, UsageError> {
    let output = crate::git::git_command()
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|error| UsageError::Message(format!("git {}: {error}", args.join(" "))))?;
    if !output.status.success() {
        return Err(UsageError::Message(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(super) fn git_lines(root: &Path, args: &[&str]) -> Result<Vec<String>, UsageError> {
    Ok(git_output(root, args)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect())
}

pub(super) fn short_sha(value: &str) -> &str {
    &value[..12.min(value.len())]
}

pub(super) fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}
