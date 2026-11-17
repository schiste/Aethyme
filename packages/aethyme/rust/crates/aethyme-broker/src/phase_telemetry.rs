//! Where a command's time went, recorded from wherever the work happens.
//!
//! A command's metric line used to carry phases only when its CLI entry point
//! assembled them by hand, so the commands nobody had instrumented (`status`,
//! `agents`, `finish`, `sync`, `cleanup`, `prepare`, `leases claim`, `git`,
//! `gh`: 22% of calls and 38% of recorded time on 2026-11-11) reported a duration and
//! nothing else. Spans opened here need no plumbing: the command runner
//! installs a recorder, any code on the command's threads adds to it, and the
//! runner writes what accumulated.
//!
//! Spans are totals, not a partition. They nest (a `lease_refresh` contains
//! `subprocess_git` time) and worker threads add to the same total, so a span
//! can exceed the command's wall time and spans must not be summed.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Totals {
    micros: u64,
    count: u64,
}

#[derive(Default)]
struct Accumulated {
    totals: BTreeMap<&'static str, Totals>,
    attachments: BTreeMap<&'static str, serde_json::Value>,
}

/// One command's accumulated spans, shared with its worker threads.
#[derive(Clone, Default)]
pub(crate) struct Recorder(Arc<Mutex<Accumulated>>);

thread_local! {
    static ACTIVE: std::cell::RefCell<Option<Recorder>> = const { std::cell::RefCell::new(None) };
}

/// Restores the thread's previous recorder when dropped.
pub(crate) struct Installed(Option<Recorder>);

impl Drop for Installed {
    fn drop(&mut self) {
        ACTIVE.with(|active| *active.borrow_mut() = self.0.take());
    }
}

/// Record this thread's spans into `recorder` until the guard drops.
pub(crate) fn install(recorder: Recorder) -> Installed {
    Installed(ACTIVE.with(|active| active.borrow_mut().replace(recorder)))
}

/// The recorder a thread about to hand work to other threads should pass on.
pub(crate) fn active() -> Option<Recorder> {
    ACTIVE.with(|active| active.borrow().clone())
}

/// Adds its lifetime to the named span when dropped.
pub(crate) struct Span {
    name: &'static str,
    started: Instant,
}

impl Drop for Span {
    fn drop(&mut self) {
        add(self.name, self.started.elapsed());
    }
}

/// Time the enclosing scope as `name`. Free when no command is recording.
pub(crate) fn span(name: &'static str) -> Span {
    Span {
        name,
        started: Instant::now(),
    }
}

/// Add one occurrence of `name` lasting `elapsed`.
pub(crate) fn add(name: &'static str, elapsed: Duration) {
    ACTIVE.with(|active| {
        if let Some(recorder) = active.borrow().as_ref()
            && let Ok(mut accumulated) = recorder.0.lock()
        {
            let entry = accumulated.totals.entry(name).or_default();
            entry.micros = entry.micros.saturating_add(elapsed.as_micros() as u64);
            entry.count += 1;
        }
    });
}

/// Attach one structured field, `name`, to the command's metric line. For
/// facts that are not durations, such as a lock's queue position or who held
/// it. An object merges into an object already attached under that name (its
/// keys win); anything else replaces it. Free when no command is recording.
pub(crate) fn attach(name: &'static str, value: serde_json::Value) {
    ACTIVE.with(|active| {
        if let Some(recorder) = active.borrow().as_ref()
            && let Ok(mut accumulated) = recorder.0.lock()
        {
            if let serde_json::Value::Object(fields) = &value
                && let Some(serde_json::Value::Object(existing)) =
                    accumulated.attachments.get_mut(name)
            {
                existing.extend(fields.clone());
            } else {
                accumulated.attachments.insert(name, value);
            }
        }
    });
}

/// The span name for a child process: Git is by far the most frequent child
/// and the one whose cost depends on the repository, so it is told apart.
pub(crate) fn subprocess_span(command: &std::process::Command) -> Span {
    let git = std::path::Path::new(command.get_program())
        .file_stem()
        .is_some_and(|stem| stem == "git");
    span(if git {
        "subprocess_git"
    } else {
        "subprocess_other"
    })
}

impl Recorder {
    /// Milliseconds and occurrence counts per span, by name.
    pub(crate) fn totals(&self) -> (BTreeMap<String, u64>, BTreeMap<String, u64>) {
        let Ok(accumulated) = self.0.lock() else {
            return Default::default();
        };
        let totals = &accumulated.totals;
        let millis = totals
            .iter()
            .map(|(name, total)| (name.to_string(), total.micros / 1000))
            .collect();
        let counts = totals
            .iter()
            .map(|(name, total)| (name.to_string(), total.count))
            .collect();
        (millis, counts)
    }

    /// The structured fields attached with [`attach`], by name.
    pub(crate) fn attachments(&self) -> BTreeMap<String, serde_json::Value> {
        let Ok(accumulated) = self.0.lock() else {
            return BTreeMap::new();
        };
        accumulated
            .attachments
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_accumulate_across_threads_and_only_while_installed() {
        add("ignored", Duration::from_millis(5));
        let recorder = Recorder::default();
        {
            let _installed = install(recorder.clone());
            add("work", Duration::from_millis(3));
            drop(span("scoped"));
            let handed_on = active().unwrap();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let _installed = install(handed_on);
                    add("work", Duration::from_millis(4));
                });
            });
        }
        add("ignored", Duration::from_millis(5));
        let (millis, counts) = recorder.totals();
        assert_eq!(millis.get("work"), Some(&7));
        assert_eq!(counts.get("work"), Some(&2));
        assert_eq!(counts.get("scoped"), Some(&1));
        assert!(!counts.contains_key("ignored"));
    }

    #[test]
    fn attachments_merge_by_name_and_only_while_installed() {
        attach("ignored", serde_json::json!(1));
        let recorder = Recorder::default();
        {
            let _installed = install(recorder.clone());
            attach(
                "lock",
                serde_json::json!({"queue_position": 3, "wait_ms": 7}),
            );
            attach(
                "lock",
                serde_json::json!({"queue_position": 0, "held_ms": 2}),
            );
            attach("scalar", serde_json::json!(1));
            attach("scalar", serde_json::json!(2));
        }
        let attachments = recorder.attachments();
        assert_eq!(
            attachments.get("lock"),
            Some(&serde_json::json!({"queue_position": 0, "wait_ms": 7, "held_ms": 2}))
        );
        assert_eq!(attachments.get("scalar"), Some(&serde_json::json!(2)));
        assert!(!attachments.contains_key("ignored"));
    }

    #[test]
    fn git_children_are_told_apart_from_other_children() {
        let recorder = Recorder::default();
        let _installed = install(recorder.clone());
        drop(subprocess_span(&std::process::Command::new(
            "/opt/homebrew/bin/git",
        )));
        drop(subprocess_span(&std::process::Command::new("gh")));
        let (_, counts) = recorder.totals();
        assert_eq!(counts.get("subprocess_git"), Some(&1));
        assert_eq!(counts.get("subprocess_other"), Some(&1));
    }
}
