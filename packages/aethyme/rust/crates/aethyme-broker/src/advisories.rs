//! Deterministic Markdown projection of durable non-blocking advisories.
//!
//! The SQLite rows are authoritative. This file only serializes concurrent
//! projectors and replaces the generated Markdown in one rename.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Advisory;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

struct ProjectionLock {
    file: File,
}

impl ProjectionLock {
    fn acquire(main_root: &Path) -> io::Result<Self> {
        let run_dir = main_root.join(".aethyme/run");
        std::fs::create_dir_all(&run_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(run_dir.join("broker-advisory.lock"))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { file })
    }
}

impl Drop for ProjectionLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

pub(crate) fn render(advisories: &[Advisory]) -> String {
    let mut output = String::from(
        "# Aethyme Broker Advisories\n\n\
         This file is generated from `.aethyme/broker.db`; the database is authoritative.\n\
         Advisories are informational and never block gates, submission, promotion, or shipping.\n\n",
    );
    if advisories.is_empty() {
        output.push_str("No outstanding advisories.\n");
        return output;
    }

    output.push_str(&format!("Outstanding advisories: {}\n\n", advisories.len()));
    for advisory in advisories {
        output.push_str(&format!(
            "## {} — {} (advisory {})\n\n",
            advisory.severity.as_str().to_uppercase(),
            quoted(&advisory.identity),
            advisory.id,
        ));
        output.push_str(&format!("- Created: `{}`\n", advisory.created_at));
        output.push_str(&format!(
            "- Session: {}\n",
            advisory
                .session_id
                .map(|id| format!("`{id}`"))
                .unwrap_or_else(|| "none".to_string())
        ));
        output.push_str(&format!(
            "- Queue entry: {}\n",
            advisory
                .queue_entry_id
                .map(|id| format!("`{id}`"))
                .unwrap_or_else(|| "none".to_string())
        ));
        output.push_str(&format!(
            "- Integration SHA: {}\n",
            advisory
                .integration_sha
                .as_deref()
                .map(|sha| format!("`{sha}`"))
                .unwrap_or_else(|| "none".to_string())
        ));
        if !advisory.paths.is_empty() {
            output.push_str("- Paths:\n");
            for path in &advisory.paths {
                output.push_str(&format!("  - {}\n", quoted(path)));
            }
        }
        if !advisory.evidence.is_empty() {
            output.push_str("- Evidence:\n");
            for evidence in &advisory.evidence {
                output.push_str(&format!(
                    "  - {}: {}\n",
                    quoted(&evidence.kind),
                    quoted(&evidence.summary),
                ));
            }
        }
        output.push_str(&format!(
            "- Acknowledge: `aethyme broker advisories ack {}`\n\n",
            advisory.id
        ));
    }
    output
}

/// Serialize projectors across processes, re-read authoritative rows while
/// holding the lock, and atomically replace the generated Markdown file.
pub(crate) fn project(
    main_root: &Path,
    load: impl FnOnce() -> Result<Vec<Advisory>, crate::BrokerError>,
) -> Result<PathBuf, crate::BrokerOpError> {
    let target = main_root.join(crate::BROKER_ADVISORY_RELPATH);
    let _lock = ProjectionLock::acquire(main_root).map_err(|source| {
        crate::BrokerOpError::AdvisoryProjectionIo {
            path: target.clone(),
            source,
        }
    })?;
    let advisories = load()?;
    let bytes = render(&advisories);
    let parent = target.parent().expect("advisory path has a parent");
    std::fs::create_dir_all(parent).map_err(|source| {
        crate::BrokerOpError::AdvisoryProjectionIo {
            path: target.clone(),
            source,
        }
    })?;
    let temporary = parent.join(format!(
        ".broker-advisory.md.tmp.{}.{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
    ));
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, &target)?;
        Ok(())
    })();
    if let Err(source) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(crate::BrokerOpError::AdvisoryProjectionIo {
            path: target,
            source,
        });
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AdvisoryEvidence, AdvisoryResolutionState, AdvisorySeverity};

    fn advisory(identity: &str) -> Advisory {
        Advisory {
            id: 7,
            identity: identity.into(),
            session_id: Some(3),
            severity: AdvisorySeverity::Warning,
            queue_entry_id: Some(9),
            integration_sha: Some("a".repeat(40)),
            paths: vec!["src/a.rs".into()],
            evidence: vec![AdvisoryEvidence {
                kind: "overlap".into(),
                summary: "caller remains on integration".into(),
            }],
            created_at: 42,
            resolution_state: AdvisoryResolutionState::Outstanding,
            acknowledged_at: None,
        }
    }

    #[test]
    fn projection_is_deterministic_and_quotes_untrusted_text() {
        let rendered = render(&[advisory("heading\n## injected")]);
        assert!(rendered.contains("\"heading\\n## injected\""));
        assert!(rendered.contains("aethyme broker advisories ack 7"));
        assert_eq!(rendered, render(&[advisory("heading\n## injected")]));
    }

    #[test]
    fn empty_projection_remains_explicit() {
        assert!(render(&[]).ends_with("No outstanding advisories.\n"));
    }
}
