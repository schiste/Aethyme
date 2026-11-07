//! Configurable thresholds for source-code shape diagnostics.
//!
//! Metrics are review signals rather than proof of cohesion or correctness.
use serde::{Deserialize, Serialize};

/// Whether findings are advisory or cause a nonzero exit in enforcement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    #[default]
    Warning,
    Error,
    Off,
}

/// Preferred limits for a behavior module. Bounds are inclusive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub level: Level,
    pub min_source_lines: usize,
    pub max_source_lines: usize,
    pub min_callables: usize,
    pub max_callables: usize,
    pub min_exports: usize,
    pub max_exports: usize,
    pub allow_zero_exports: bool,
    pub min_internal_ratio: f64,
    pub internal_ratio_min_callables: usize,
    pub max_runtime_fan_out: usize,
    pub max_runtime_fan_in: usize,
    pub max_callable_lines: usize,
    pub max_cyclomatic_complexity: usize,
    pub allow_runtime_cycles: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            level: Level::Warning,
            min_source_lines: 100,
            max_source_lines: 300,
            min_callables: 3,
            max_callables: 7,
            min_exports: 1,
            max_exports: 3,
            allow_zero_exports: true,
            min_internal_ratio: 0.3,
            internal_ratio_min_callables: 4,
            max_runtime_fan_out: 7,
            max_runtime_fan_in: 20,
            max_callable_lines: 50,
            max_cyclomatic_complexity: 10,
            allow_runtime_cycles: false,
        }
    }
}

impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.min_source_lines > self.max_source_lines {
            return Err("min_source_lines must not exceed max_source_lines".into());
        }
        if self.min_callables > self.max_callables {
            return Err("min_callables must not exceed max_callables".into());
        }
        if self.min_exports > self.max_exports {
            return Err("min_exports must not exceed max_exports".into());
        }
        if !(0.0..=1.0).contains(&self.min_internal_ratio) {
            return Err("min_internal_ratio must be between 0 and 1".into());
        }
        Ok(())
    }
}

/// A complete set of measured metrics for one behavior module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    pub source_lines: usize,
    pub callables: usize,
    pub exports: usize,
    pub internal_callables: usize,
    pub runtime_fan_out: usize,
    pub runtime_fan_in: usize,
    pub max_callable_lines: usize,
    pub max_cyclomatic_complexity: usize,
    pub runtime_cycle: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    pub metric: &'static str,
    pub actual: String,
    pub expected: String,
    pub level: Level,
}

/// Compare complete measurements with the configured preferred ranges.
pub fn violations(policy: &Policy, metrics: &Metrics) -> Result<Vec<Violation>, String> {
    policy.validate()?;
    if metrics.internal_callables > metrics.callables {
        return Err("internal_callables cannot exceed callables".into());
    }
    if policy.level == Level::Off {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut record = |metric, actual: String, expected: String| {
        out.push(Violation {
            metric,
            actual,
            expected,
            level: policy.level,
        });
    };
    for (name, actual, min, max) in [
        (
            "source_lines",
            metrics.source_lines,
            policy.min_source_lines,
            policy.max_source_lines,
        ),
        (
            "callables",
            metrics.callables,
            policy.min_callables,
            policy.max_callables,
        ),
    ] {
        if !(min..=max).contains(&actual) {
            record(name, actual.to_string(), format!("{min}..={max}"));
        }
    }
    if !(policy.min_exports..=policy.max_exports).contains(&metrics.exports)
        && !(policy.allow_zero_exports && metrics.exports == 0)
    {
        record(
            "exports",
            metrics.exports.to_string(),
            format!(
                "{}..={}{}",
                policy.min_exports,
                policy.max_exports,
                if policy.allow_zero_exports {
                    " or 0"
                } else {
                    ""
                }
            ),
        );
    }
    if metrics.callables >= policy.internal_ratio_min_callables && metrics.callables > 0 {
        let ratio = metrics.internal_callables as f64 / metrics.callables as f64;
        if ratio < policy.min_internal_ratio {
            record(
                "internal_ratio",
                format!("{ratio:.4}"),
                format!(">= {}", policy.min_internal_ratio),
            );
        }
    }
    for (name, actual, max) in [
        (
            "runtime_fan_out",
            metrics.runtime_fan_out,
            policy.max_runtime_fan_out,
        ),
        (
            "runtime_fan_in",
            metrics.runtime_fan_in,
            policy.max_runtime_fan_in,
        ),
        (
            "callable_lines",
            metrics.max_callable_lines,
            policy.max_callable_lines,
        ),
        (
            "cyclomatic_complexity",
            metrics.max_cyclomatic_complexity,
            policy.max_cyclomatic_complexity,
        ),
    ] {
        if actual > max {
            record(name, actual.to_string(), format!("<= {max}"));
        }
    }
    if metrics.runtime_cycle && !policy.allow_runtime_cycles {
        record("runtime_cycle", "true".into(), "false".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside() -> Metrics {
        Metrics {
            source_lines: 100,
            callables: 4,
            exports: 0,
            internal_callables: 2,
            runtime_fan_out: 7,
            runtime_fan_in: 20,
            max_callable_lines: 50,
            max_cyclomatic_complexity: 10,
            runtime_cycle: false,
        }
    }

    #[test]
    fn default_boundaries_and_zero_exports_are_accepted() {
        assert!(
            violations(&Policy::default(), &inside())
                .unwrap()
                .is_empty()
        );
        let mut m = inside();
        m.source_lines = 300;
        m.callables = 7;
        m.exports = 3;
        m.internal_callables = 3;
        assert!(violations(&Policy::default(), &m).unwrap().is_empty());
    }

    #[test]
    fn every_threshold_reports_and_uses_selected_level() {
        let p = Policy {
            level: Level::Error,
            ..Policy::default()
        };
        let m = Metrics {
            source_lines: 301,
            callables: 8,
            exports: 4,
            internal_callables: 2,
            runtime_fan_out: 8,
            runtime_fan_in: 21,
            max_callable_lines: 51,
            max_cyclomatic_complexity: 11,
            runtime_cycle: true,
        };
        let result = violations(&p, &m).unwrap();
        assert_eq!(result.len(), 9);
        assert!(result.iter().all(|v| v.level == Level::Error));
    }

    #[test]
    fn configuration_is_partial_validated_and_rejects_typos() {
        let p: Policy = toml::from_str("level = 'error'\nmax_source_lines = 500\n").unwrap();
        assert_eq!(p.max_source_lines, 500);
        assert_eq!(p.min_callables, 3);
        assert_eq!(p.level, Level::Error);
        assert!(toml::from_str::<Policy>("max_source_linnes = 500").is_err());
        let p = Policy {
            min_internal_ratio: 1.1,
            ..p
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn small_callable_sample_skips_internal_ratio_and_off_disables_findings() {
        let mut m = inside();
        m.callables = 3;
        m.internal_callables = 0;
        assert!(violations(&Policy::default(), &m).unwrap().is_empty());
        m.source_lines = 1;
        let p = Policy {
            level: Level::Off,
            ..Policy::default()
        };
        assert!(violations(&p, &m).unwrap().is_empty());
    }
}
