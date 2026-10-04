//! Load-based admission for expensive gates.
//!
//! A gate's `timeout_seconds` is a deadline for the gate's own work, but on a
//! host whose CPUs are already saturated by other repositories' builds, an
//! expensive gate can run several times slower than it was sized for and
//! time out through no fault of the diff. Before such a gate is spawned, the
//! broker therefore waits while the one-minute load average per logical CPU
//! is above the gate's threshold, bounded by the gate's existing
//! `resource_wait_seconds`. If load remains above the threshold at the bound,
//! the broker defers the gate before it starts; if the load cannot be read,
//! the gate runs without waiting because saturation was not established.
//!
//! Which gates are admitted this way:
//! - a gate that sets `max_load_per_cpu` uses that threshold;
//! - otherwise a gate with `cost >= 3` uses [`DEFAULT_MAX_LOAD_PER_CPU`];
//! - every other gate is spawned without consulting the load.
//!
//! The wait happens before owner locks and host resource leases are taken,
//! so a gate waiting on load never holds a lease another gate needs, and
//! before the timeout clock starts, so the waited time is never charged to
//! the gate's deadline. It is counted in the gate's `wait_duration_ms`.

use std::time::Duration;

use crate::gates::{Gate, GateConfigError, GateProgressSink};

/// Gates at or above this cost are admitted by host load without opting in.
pub(crate) const EXPENSIVE_GATE_MIN_COST: i64 = 3;

/// Threshold used for an expensive gate that sets no `max_load_per_cpu`.
pub(crate) const DEFAULT_MAX_LOAD_PER_CPU: f64 = 3.0;

/// How often the load is re-read while waiting. The kernel refreshes the
/// load average roughly every five seconds, so polling faster sees nothing new.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How often a still-waiting gate reports progress.
const REPORT_INTERVAL: Duration = Duration::from_secs(30);

/// One reading of the host's load.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LoadSample {
    pub load_1m: f64,
    pub cpus: i64,
}

impl LoadSample {
    fn per_cpu(self) -> f64 {
        self.load_1m / self.cpus.max(1) as f64
    }
}

/// Where admission reads the load and how it waits. Production reads the
/// kernel and sleeps; tests inject a scripted load and a virtual clock, so no
/// test depends on the real host's load.
pub(crate) trait AdmissionHost {
    fn sample(&self) -> Option<LoadSample>;
    fn sleep(&self, duration: Duration);
}

pub(crate) struct SystemAdmissionHost;

impl AdmissionHost for SystemAdmissionHost {
    fn sample(&self) -> Option<LoadSample> {
        Some(LoadSample {
            load_1m: crate::gates::load_average_1m()?,
            cpus: crate::gates::logical_cpu_count()?,
        })
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// What admission decided, for callers and tests.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Admission {
    /// The gate is not admitted by load (cheap, or no wait budget).
    NotApplicable,
    /// The load could not be read; the gate runs without waiting.
    Unmeasured,
    /// The load was at or below the threshold after `waited`.
    Admitted { waited: Duration },
    /// The load stayed above the threshold for the whole bound; the caller
    /// must record a host deferral without starting the gate.
    BoundReached {
        waited: Duration,
        last: LoadSample,
        threshold: f64,
    },
}

/// Parse the optional per-gate `max_load_per_cpu` key. Integers are accepted
/// (`max_load_per_cpu = 4`); zero, negative, and non-finite values are not,
/// because a threshold nothing can satisfy would only ever delay the gate.
pub(crate) fn parse_max_load_per_cpu(
    entry: &toml::Value,
    gate: &str,
) -> Result<Option<f64>, GateConfigError> {
    let Some(value) = entry.get("max_load_per_cpu") else {
        return Ok(None);
    };
    let number = value
        .as_float()
        .or_else(|| value.as_integer().map(|integer| integer as f64));
    match number {
        Some(number) if number.is_finite() && number > 0.0 => Ok(Some(number)),
        _ => Err(GateConfigError::Parse(format!(
            "gate {gate:?}: max_load_per_cpu must be a positive number"
        ))),
    }
}

/// The threshold that applies to a gate, or `None` when the gate is not
/// admitted by load.
pub(crate) fn effective_max_load_per_cpu(cost: i64, configured: Option<f64>) -> Option<f64> {
    configured.or((cost >= EXPENSIVE_GATE_MIN_COST).then_some(DEFAULT_MAX_LOAD_PER_CPU))
}

/// Resolve load admission for one configured gate using an injected host.
/// Production supplies the system host; tests use deterministic samples.
pub(crate) fn admit_gate_with(
    gate: &Gate,
    progress: &dyn GateProgressSink,
    host: &dyn AdmissionHost,
) -> Admission {
    admit(
        &gate.name,
        effective_max_load_per_cpu(gate.cost, gate.max_load_per_cpu),
        Duration::from_secs(gate.resource_wait_seconds),
        host,
        progress,
    )
}

pub(crate) fn admit(
    gate_name: &str,
    max_load_per_cpu: Option<f64>,
    bound: Duration,
    host: &dyn AdmissionHost,
    progress: &dyn GateProgressSink,
) -> Admission {
    let Some(max) = max_load_per_cpu else {
        return Admission::NotApplicable;
    };
    if bound.is_zero() {
        return Admission::NotApplicable;
    }
    let mut waited = Duration::ZERO;
    let mut next_report = Duration::ZERO;
    loop {
        let Some(sample) = host.sample() else {
            if !waited.is_zero() {
                progress.report(&format!(
                    "gate {gate_name} host load became unreadable after {}s; running now",
                    waited.as_secs()
                ));
            }
            return Admission::Unmeasured;
        };
        if sample.per_cpu() <= max {
            if !waited.is_zero() {
                progress.report(&format!(
                    "gate {gate_name} admitted after {}s waiting for host load: {}",
                    waited.as_secs(),
                    describe(sample, max)
                ));
            }
            return Admission::Admitted { waited };
        }
        if waited >= bound {
            progress.report(&format!(
                "gate {gate_name} host load wait bound of {}s reached ({}); deferring without starting the gate",
                bound.as_secs(),
                describe(sample, max)
            ));
            return Admission::BoundReached {
                waited,
                last: sample,
                threshold: max,
            };
        }
        if waited >= next_report {
            progress.report(&format!(
                "gate {gate_name} waiting for host load: {} (waited {}s of {}s)",
                describe(sample, max),
                waited.as_secs(),
                bound.as_secs()
            ));
            next_report = waited + REPORT_INTERVAL;
        }
        let step = POLL_INTERVAL.min(bound - waited);
        host.sleep(step);
        waited += step;
    }
}

pub(crate) fn describe(sample: LoadSample, max: f64) -> String {
    format!(
        "load 1m {:.1}/{} cpus = {:.2} per cpu, max {:.2}",
        sample.load_1m,
        sample.cpus,
        sample.per_cpu(),
        max
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::sync::Mutex;

    /// Replays a fixed series of per-cpu loads on 10 CPUs; the last one
    /// repeats. Sleeping only advances a virtual clock.
    struct ScriptedHost {
        loads: Vec<Option<f64>>,
        next: Cell<usize>,
        slept: RefCell<Vec<Duration>>,
    }

    impl ScriptedHost {
        fn new(loads: &[Option<f64>]) -> Self {
            Self {
                loads: loads.to_vec(),
                next: Cell::new(0),
                slept: RefCell::new(Vec::new()),
            }
        }

        fn total_slept(&self) -> Duration {
            self.slept.borrow().iter().sum()
        }
    }

    impl AdmissionHost for ScriptedHost {
        fn sample(&self) -> Option<LoadSample> {
            let index = self.next.get().min(self.loads.len() - 1);
            self.next.set(self.next.get() + 1);
            self.loads[index].map(|per_cpu| LoadSample {
                load_1m: per_cpu * 10.0,
                cpus: 10,
            })
        }

        fn sleep(&self, duration: Duration) {
            self.slept.borrow_mut().push(duration);
        }
    }

    #[derive(Default)]
    struct Lines(Mutex<Vec<String>>);

    impl GateProgressSink for Lines {
        fn report(&self, line: &str) {
            self.0.lock().unwrap().push(line.to_string());
        }
    }

    impl Lines {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut self.0.lock().unwrap())
        }
    }

    #[test]
    fn only_expensive_or_configured_gates_are_admitted_by_load() {
        assert_eq!(effective_max_load_per_cpu(1, None), None);
        assert_eq!(effective_max_load_per_cpu(2, None), None);
        assert_eq!(
            effective_max_load_per_cpu(3, None),
            Some(DEFAULT_MAX_LOAD_PER_CPU)
        );
        assert_eq!(effective_max_load_per_cpu(1, Some(1.5)), Some(1.5));
        assert_eq!(effective_max_load_per_cpu(5, Some(8.0)), Some(8.0));
    }

    #[test]
    fn a_calm_host_admits_immediately_without_sleeping_or_reporting() {
        let host = ScriptedHost::new(&[Some(1.0)]);
        let lines = Lines::default();
        let admission = admit("g", Some(3.0), Duration::from_secs(900), &host, &lines);
        assert_eq!(
            admission,
            Admission::Admitted {
                waited: Duration::ZERO
            }
        );
        assert!(host.slept.borrow().is_empty());
        assert!(lines.take().is_empty());
    }

    #[test]
    fn a_loaded_host_delays_the_gate_until_the_load_drops() {
        let host = ScriptedHost::new(&[Some(9.5), Some(6.0), Some(3.2), Some(2.9)]);
        let lines = Lines::default();
        let admission = admit(
            "cargo-test",
            Some(3.0),
            Duration::from_secs(900),
            &host,
            &lines,
        );
        assert_eq!(
            admission,
            Admission::Admitted {
                waited: Duration::from_secs(15)
            }
        );
        assert_eq!(host.total_slept(), Duration::from_secs(15));
        let lines = lines.take();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].starts_with("gate cargo-test waiting for host load: load 1m 95.0/10 cpus"),
            "{lines:?}"
        );
        assert!(
            lines[1].starts_with("gate cargo-test admitted after 15s waiting for host load"),
            "{lines:?}"
        );
    }

    #[test]
    fn the_wait_is_bounded_and_defers_without_starting_the_gate() {
        let host = ScriptedHost::new(&[Some(9.0)]);
        let lines = Lines::default();
        let admission = admit("g", Some(3.0), Duration::from_secs(12), &host, &lines);
        assert_eq!(
            admission,
            Admission::BoundReached {
                waited: Duration::from_secs(12),
                last: LoadSample {
                    load_1m: 90.0,
                    cpus: 10
                },
                threshold: 3.0,
            }
        );
        // The last step is clipped to the bound rather than overshooting it.
        assert_eq!(
            *host.slept.borrow(),
            vec![
                Duration::from_secs(5),
                Duration::from_secs(5),
                Duration::from_secs(2)
            ]
        );
        let lines = lines.take();
        assert!(
            lines
                .last()
                .unwrap()
                .contains("host load wait bound of 12s reached"),
            "{lines:?}"
        );
        assert!(
            lines
                .last()
                .unwrap()
                .ends_with("deferring without starting the gate")
        );
    }

    #[test]
    fn progress_is_reported_periodically_not_on_every_poll() {
        let host = ScriptedHost::new(&[Some(9.0)]);
        let lines = Lines::default();
        admit("g", Some(3.0), Duration::from_secs(65), &host, &lines);
        let waiting = lines
            .take()
            .into_iter()
            .filter(|line| line.contains("waiting for host load"))
            .collect::<Vec<_>>();
        // Reports at 0s, 30s and 60s of a 65s wait polled every 5s.
        assert_eq!(waiting.len(), 3, "{waiting:?}");
        assert!(waiting[1].contains("(waited 30s of 65s)"), "{waiting:?}");
    }

    #[test]
    fn unreadable_load_or_no_wait_budget_never_delays_the_gate() {
        let lines = Lines::default();
        let host = ScriptedHost::new(&[None]);
        assert_eq!(
            admit("g", Some(3.0), Duration::from_secs(900), &host, &lines),
            Admission::Unmeasured
        );
        let host = ScriptedHost::new(&[Some(50.0)]);
        assert_eq!(
            admit("g", Some(3.0), Duration::ZERO, &host, &lines),
            Admission::NotApplicable
        );
        assert_eq!(
            admit("g", None, Duration::from_secs(900), &host, &lines),
            Admission::NotApplicable
        );
        assert!(host.slept.borrow().is_empty());
        assert!(lines.take().is_empty());
    }

    #[test]
    fn the_key_is_parsed_and_changes_the_definition_hash_only_when_set() {
        let base = "[[gate]]\nname='g'\ncommand='true'\ncost=3\nresource_wait_seconds=60\n";
        let plain = crate::gates::parse_gates(base).unwrap();
        let tuned = crate::gates::parse_gates(&format!("{base}max_load_per_cpu = 1.5\n")).unwrap();
        assert_eq!(plain[0].max_load_per_cpu, None);
        assert_eq!(tuned[0].max_load_per_cpu, Some(1.5));
        assert_ne!(plain[0].definition_hash, tuned[0].definition_hash);
        // A gate that does not opt in keeps the hash it had before the key
        // existed, so its cached verdicts stay valid.
        let expected_without_key = format!(
            "{:x}",
            <sha2::Sha256 as sha2::Digest>::digest(
                serde_json::to_vec(&serde_json::json!({
                    "name": "g", "command": "true", "cost": 3, "triggers": [],
                    "cache": true, "timeout_seconds": null, "resources": [],
                    "resource_ttl_seconds": 300, "resource_wait_seconds": 60,
                    "managed_cache": null,
                }))
                .unwrap()
            )
        );
        assert_eq!(plain[0].definition_hash, expected_without_key);
        assert!(crate::gates::parse_gates(&format!("{base}max_load_per_cpu = 0\n")).is_err());
    }

    #[test]
    fn max_load_per_cpu_accepts_positive_numbers_only() {
        let parse = |text: &str| {
            let value: toml::Value = toml::from_str(text).unwrap();
            parse_max_load_per_cpu(&value, "g")
        };
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("max_load_per_cpu = 2.5").unwrap(), Some(2.5));
        assert_eq!(parse("max_load_per_cpu = 4").unwrap(), Some(4.0));
        for bad in [
            "max_load_per_cpu = 0",
            "max_load_per_cpu = -1.0",
            "max_load_per_cpu = nan",
            "max_load_per_cpu = inf",
            "max_load_per_cpu = \"3\"",
        ] {
            assert!(parse(bad).is_err(), "{bad} should be rejected");
        }
    }
}
