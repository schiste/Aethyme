//! First-run safety for archiving that is on by default.
//!
//! A host that never wrote `archive = true|false` archives by default, and
//! archiving moves the owner's files. So before the background worker's first
//! automatic pass on such a host, `aethyme broker status` shows one notice
//! (`transcripts.archiving-default-on`: what is archived, after how many
//! inactive days, how to change the days or turn it off), and the worker
//! archives nothing until 24 hours after that notice was first shown. The time
//! is recorded in `<host state>/transcripts/first-run.json`. An explicit
//! setting (`aethyme transcripts enable`) or a manual
//! `aethyme transcripts archive --apply` skips the wait.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::config::{EnabledSource, TranscriptsConfig};
use super::providers::Provider;

/// The first-run state file, under `<host state>/transcripts/`.
pub(crate) const STATE_FILE: &str = "first-run.json";
/// How long after the notice is first shown the worker waits.
pub(crate) const NOTICE_WAIT_MS: i64 = 24 * 60 * 60 * 1000;
/// The advice id `broker status` shows during the wait.
pub(crate) const ADVICE_ID: &str = "transcripts.archiving-default-on";

/// What the host recorded about the default-on notice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirstRunState {
    /// When `broker status` first showed the notice.
    #[serde(default)]
    pub noticed_at_ms: Option<i64>,
    /// When the owner archived by hand, which skips the wait for good.
    #[serde(default)]
    pub acknowledged_at_ms: Option<i64>,
    /// What acknowledged it (`archive_apply`).
    #[serde(default)]
    pub acknowledged_by: Option<String>,
}

/// Whether the background worker may archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    /// Explicitly enabled, acknowledged, or the wait is over.
    Open,
    /// On by default and the notice has not been shown yet.
    NoticePending,
    /// On by default and the notice was shown less than 24 hours ago.
    Waiting,
}

impl Gate {
    pub fn label(self) -> &'static str {
        match self {
            Gate::Open => "open",
            Gate::NoticePending => "notice_pending",
            Gate::Waiting => "waiting",
        }
    }
}

/// The recorded state; empty when the file is absent or unreadable (which
/// keeps the worker waiting, the safe side).
pub(crate) fn read(host_dir: &Path) -> FirstRunState {
    fs::read_to_string(host_dir.join(STATE_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write(host_dir: &Path, state: &FirstRunState) -> Result<(), String> {
    let path = host_dir.join(STATE_FILE);
    let text = serde_json::to_string(state)
        .map_err(|error| format!("cannot render the first-run state: {error}"))?;
    super::ensure_private_directory(host_dir)
        .map_err(|error| format!("cannot create the transcript state directory: {error}"))?;
    crate::atomic_file::with_synced_temporary(&path, text.as_bytes(), |temporary| {
        fs::rename(temporary, &path)?;
        crate::host_state::protect_host_state_path(&path, false)
    })
    .map_err(|error| format!("cannot write the first-run state: {error}"))?;
    super::sync_directory(host_dir);
    Ok(())
}

/// Record that the notice was shown at `now_ms`, unless it already was.
pub(crate) fn record_notice(host_dir: &Path, now_ms: i64) -> Result<FirstRunState, String> {
    let mut state = read(host_dir);
    if state.noticed_at_ms.is_none() {
        state.noticed_at_ms = Some(now_ms);
        write(host_dir, &state)?;
    }
    Ok(state)
}

/// Record that the owner archived by hand (`by` names how), unless already
/// recorded.
pub(crate) fn acknowledge(host_dir: &Path, now_ms: i64, by: &str) -> Result<(), String> {
    let mut state = read(host_dir);
    if state.acknowledged_at_ms.is_none() {
        state.acknowledged_at_ms = Some(now_ms);
        state.acknowledged_by = Some(by.to_string());
        write(host_dir, &state)?;
    }
    Ok(())
}

/// Whether the background worker may archive at `now_ms`.
pub(crate) fn gate(config: &TranscriptsConfig, state: &FirstRunState, now_ms: i64) -> Gate {
    if config.enabled_source == EnabledSource::Settings || state.acknowledged_at_ms.is_some() {
        return Gate::Open;
    }
    match state.noticed_at_ms {
        None => Gate::NoticePending,
        Some(noticed) if now_ms < noticed.saturating_add(NOTICE_WAIT_MS) => Gate::Waiting,
        Some(_) => Gate::Open,
    }
}

/// The first-run part of `transcripts status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FirstRunStatus {
    pub gate: Gate,
    #[serde(flatten)]
    pub state: FirstRunState,
    /// When automatic archiving may start, once the notice was shown.
    pub automatic_archiving_from_ms: Option<i64>,
}

/// Disabled reads nothing beyond the settings, so it reports an empty state.
pub(crate) fn status(config: &TranscriptsConfig, host_dir: &Path, now_ms: i64) -> FirstRunStatus {
    if !config.archive {
        return FirstRunStatus {
            gate: Gate::Open,
            state: FirstRunState::default(),
            automatic_archiving_from_ms: None,
        };
    }
    let state = read(host_dir);
    let gate = gate(config, &state, now_ms);
    let automatic_archiving_from_ms = match gate {
        Gate::Open | Gate::NoticePending => None,
        Gate::Waiting => state
            .noticed_at_ms
            .map(|noticed| noticed.saturating_add(NOTICE_WAIT_MS)),
    };
    FirstRunStatus {
        gate,
        state,
        automatic_archiving_from_ms,
    }
}

/// What is archived and after how many inactive days, in a few words.
fn scope(config: &TranscriptsConfig) -> String {
    let parts: Vec<String> = config
        .providers
        .iter()
        .map(|provider| {
            let name = match provider {
                Provider::Codex => "Codex",
                Provider::Claude => "Claude Code",
            };
            format!(
                "{name} transcripts after {} inactive days",
                config.days_for(*provider)
            )
        })
        .collect();
    if parts.is_empty() {
        "no transcripts (no provider is configured)".to_string()
    } else {
        parts.join(" and ")
    }
}

/// The notice for a default-on host, recording when it is first shown.
/// `None` once the wait is over, when acknowledged, or when the host set
/// `archive` explicitly.
pub(crate) fn notice_advice_at(
    host_dir: &Path,
    config: &TranscriptsConfig,
    now_ms: i64,
) -> Option<crate::StatusAdvice> {
    if !config.archive || config.enabled_source != EnabledSource::Default {
        return None;
    }
    let state = read(host_dir);
    if state.acknowledged_at_ms.is_some() {
        return None;
    }
    // A notice that cannot be recorded is not shown: the worker keeps
    // waiting until it can be.
    let state = match state.noticed_at_ms {
        Some(_) => state,
        None => record_notice(host_dir, now_ms).ok()?,
    };
    let noticed = state.noticed_at_ms?;
    let starts = noticed.saturating_add(NOTICE_WAIT_MS);
    if now_ms >= starts {
        return None;
    }
    // Positive here, so rounding up is plain integer arithmetic.
    let hour_ms = 60 * 60 * 1000;
    let hours = (starts - now_ms + hour_ms - 1) / hour_ms;
    Some(crate::StatusAdvice {
        id: ADVICE_ID,
        severity: crate::StatusAdviceSeverity::Notice,
        reason: "transcript archiving is on by default and starts after a 24-hour notice",
        summary: format!(
            "transcript archiving is on by default: in about {hours} h the background worker starts compressing and moving {} (closed sessions only, open in no process); change the days with `aethyme transcripts enable --after-inactive-days <n>` (Claude Code: archive_after_inactive_days_claude in the settings file) or turn it off with `aethyme transcripts disable`",
            scope(config)
        ),
        session_id: None,
        queue_entry_id: None,
        evidence: vec!["enabled_source=default".into(), "gate=waiting".into()],
        commands: vec![
            "aethyme transcripts status".into(),
            "aethyme transcripts enable --after-inactive-days <n>".into(),
            "aethyme transcripts disable".into(),
        ],
    })
}

/// The notice for this host (a no-op in tests and inside the worker).
pub(crate) fn default_on_advice() -> Option<crate::StatusAdvice> {
    if crate::gc::background_process_is_suppressed() {
        return None;
    }
    let (environment, config) = super::host_settings()?;
    notice_advice_at(&environment.host_dir, config, super::now_ms())
}
