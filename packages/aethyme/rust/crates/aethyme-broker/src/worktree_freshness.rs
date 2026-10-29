//! Per-repository policy for reporting old session-base observations.
//!
//! The observation is always revalidated at the next `start --reuse` task
//! boundary. This window only classifies the previous observation as stale; it
//! is never an ancestry cache or permission to move a checkout. An age at or
//! above the threshold is stale, so zero means every prior observation is
//! stale. A future timestamp has unknown age and is reported as stale.
//!
//! Example `.aethyme/config.toml`:
//!
//! ```toml
//! [worktree_freshness]
//! max_age_seconds = 900
//! ```

use std::path::Path;

pub(crate) const DEFAULT_MAX_AGE_SECONDS: u64 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct WorktreeFreshnessPolicy {
    pub(crate) max_age_seconds: u64,
}
impl Default for WorktreeFreshnessPolicy {
    fn default() -> Self {
        Self {
            max_age_seconds: DEFAULT_MAX_AGE_SECONDS,
        }
    }
}

impl WorktreeFreshnessPolicy {
    pub(crate) fn load(main_root: &Path) -> Result<Self, String> {
        match crate::merge::repository_config_checked(main_root)
            .map_err(|error| error.to_string())?
        {
            Some((text, _)) => Self::from_config_text(&text),
            None => Ok(Self::default()),
        }
    }

    fn from_config_text(text: &str) -> Result<Self, String> {
        let value = text
            .parse::<toml::Value>()
            .map_err(|error| format!(".aethyme/config.toml is invalid: {error}"))?;
        let Some(section) = value.get("worktree_freshness") else {
            return Ok(Self::default());
        };
        section
            .clone()
            .try_into()
            .map_err(|error| format!("[worktree_freshness] is invalid: {error}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PriorObservationAge {
    /// Whole seconds for display; sub-second precision is retained for the
    /// stale classification below.
    pub(crate) age_seconds: Option<u64>,
    /// Missing/future timestamps have unknown age and are conservatively stale.
    pub(crate) stale: bool,
}

pub(crate) fn classify_prior_observation(
    observed_at_ms: Option<i64>,
    now_ms: i64,
    max_age_seconds: u64,
) -> PriorObservationAge {
    let age_ms = observed_at_ms
        .and_then(|observed_at| now_ms.checked_sub(observed_at))
        .filter(|age| *age >= 0)
        .and_then(|age| u64::try_from(age).ok());
    let threshold_ms = max_age_seconds.saturating_mul(1_000);
    PriorObservationAge {
        age_seconds: age_ms.map(|age| age / 1_000),
        stale: age_ms.is_none_or(|age| age >= threshold_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_defaults_to_fifteen_minutes_and_accepts_zero() {
        assert_eq!(WorktreeFreshnessPolicy::default().max_age_seconds, 900);
        assert_eq!(
            WorktreeFreshnessPolicy::from_config_text(
                "[worktree_freshness]\nmax_age_seconds = 0\n"
            )
            .unwrap()
            .max_age_seconds,
            0
        );
    }

    #[test]
    fn policy_refuses_unknown_keys_and_non_unsigned_values() {
        assert!(
            WorktreeFreshnessPolicy::from_config_text("[worktree_freshness]\nmax_age = 900\n")
                .is_err()
        );
        assert!(
            WorktreeFreshnessPolicy::from_config_text(
                "[worktree_freshness]\nmax_age_seconds = -1\n"
            )
            .is_err()
        );
    }

    #[test]
    fn age_uses_raw_milliseconds_and_zero_means_always_stale() {
        let at_zero = classify_prior_observation(Some(10_000), 10_000, 0);
        assert_eq!(at_zero.age_seconds, Some(0));
        assert!(at_zero.stale);

        let just_under_one_second = classify_prior_observation(Some(1), 1_000, 1);
        assert_eq!(just_under_one_second.age_seconds, Some(0));
        assert!(!just_under_one_second.stale);

        let exact_threshold = classify_prior_observation(Some(0), 1_000, 1);
        assert_eq!(exact_threshold.age_seconds, Some(1));
        assert!(exact_threshold.stale);
    }

    #[test]
    fn missing_and_future_observations_have_unknown_age_and_are_stale() {
        for observed_at_ms in [None, Some(2_000)] {
            let age = classify_prior_observation(observed_at_ms, 1_000, 900);
            assert_eq!(age.age_seconds, None);
            assert!(age.stale);
        }
    }
}
