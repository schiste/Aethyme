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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
struct Totals {
    micros: u64,
    count: u64,
}

#[derive(Clone, Default)]
struct Accumulated {
    totals: BTreeMap<&'static str, Totals>,
    attachments: BTreeMap<&'static str, serde_json::Value>,
    intervals: Vec<(Instant, Instant, bool)>,
    distinct: BTreeMap<&'static str, BTreeSet<i64>>,
    /// Spans opened and not yet dropped, by opening order: what an
    /// interrupted command was doing, and since when.
    open: BTreeMap<u64, (&'static str, Instant)>,
    next_open: u64,
}

impl Accumulated {
    fn add(&mut self, name: &'static str, start: Instant, end: Instant) {
        let entry = self.totals.entry(name).or_default();
        entry.micros = entry
            .micros
            .saturating_add(end.saturating_duration_since(start).as_micros() as u64);
        entry.count += 1;
        if matches!(
            name,
            "subprocess_git" | "subprocess_other" | "review_on_push_child_wait"
        ) {
            self.intervals.push((start, end, true));
        } else if matches!(
            name,
            "lifecycle_lock_wait"
                | "lifecycle_lock_promoted_wait"
                | "process_spawn_lock_wait"
                | "repository_write_lock_wait"
                | "install_slot_wait"
                | "shared_cache_wait_exhausted"
                | "shared_cache_waited_for_fill"
                | "operation_outcome_record_wait"
                | "gate_verdict_wait"
                | "broker_db_busy_wait"
        ) {
            self.intervals.push((start, end, false));
        }
    }
}

/// A span still open when a command was interrupted.
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenSpan {
    pub(crate) name: &'static str,
    pub(crate) elapsed: Duration,
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
    /// Where this span is registered as open, so an interrupted command can
    /// say which spans it was inside.
    open: Option<(Recorder, u64)>,
}

impl Drop for Span {
    fn drop(&mut self) {
        let ended = Instant::now();
        // Registered as open: close it and add its time under one lock, in
        // the recorder it was opened in.
        if let Some((recorder, id)) = self.open.take() {
            if let Ok(mut accumulated) = recorder.0.lock() {
                accumulated.open.remove(&id);
                accumulated.add(self.name, self.started, ended);
            }
            return;
        }
        record(self.name, self.started, ended);
    }
}

/// Time the enclosing scope as `name`. Free when no command is recording.
pub(crate) fn span(name: &'static str) -> Span {
    let started = Instant::now();
    let open = ACTIVE.with(|active| {
        let recorder = active.borrow().clone()?;
        let id = {
            let mut accumulated = recorder.0.lock().ok()?;
            let id = accumulated.next_open;
            accumulated.next_open += 1;
            accumulated.open.insert(id, (name, started));
            id
        };
        Some((recorder, id))
    });
    Span {
        name,
        started,
        open,
    }
}

/// Add one occurrence of `name` lasting `elapsed`.
pub(crate) fn add(name: &'static str, elapsed: Duration) {
    let end = Instant::now();
    record(name, end.checked_sub(elapsed).unwrap_or(end), end);
}

fn record(name: &'static str, start: Instant, end: Instant) {
    ACTIVE.with(|active| {
        if let Some(recorder) = active.borrow().as_ref()
            && let Ok(mut accumulated) = recorder.0.lock()
        {
            accumulated.add(name, start, end);
        }
    });
}

/// Record a duration that was configured or granted (a budget, a reserve,
/// a retry delay), not one that elapsed. It must not go into
/// `phase_timings_ms`, where it would be summed as time spent: on the busiest
/// repository `landed_proof_session_budget_doubled` summed to 1,617,540 s
/// over 21 worker runs. The occurrence is counted (`phase_counts`, with a
/// zero timing) and the value goes to the `phase_values_ms` attachment, the
/// largest seen per name.
pub(crate) fn value(name: &'static str, configured: Duration) {
    add(name, Duration::ZERO);
    ACTIVE.with(|active| {
        if let Some(recorder) = active.borrow().as_ref()
            && let Ok(mut accumulated) = recorder.0.lock()
        {
            let millis = u64::try_from(configured.as_millis()).unwrap_or(u64::MAX);
            let values = accumulated
                .attachments
                .entry("phase_values_ms")
                .or_insert_with(|| serde_json::Value::Object(Default::default()));
            if let serde_json::Value::Object(fields) = values {
                let largest = fields
                    .get(name)
                    .and_then(serde_json::Value::as_u64)
                    .map_or(millis, |seen| seen.max(millis));
                fields.insert(name.to_string(), largest.into());
            }
        }
    });
}

/// Accumulate one occurrence of `elapsed` under `key` in the attachment
/// `name`: `{key: {"count", "total_ms", "max_ms"}}`. For a breakdown whose
/// keys are not known in advance (which claim held a lock, say). `key` must
/// name an operation, never a path or an id. Free when no command is
/// recording.
pub(crate) fn add_keyed(name: &'static str, key: &str, elapsed: Duration) {
    ACTIVE.with(|active| {
        if let Some(recorder) = active.borrow().as_ref()
            && let Ok(mut accumulated) = recorder.0.lock()
        {
            let millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
            let object = accumulated
                .attachments
                .entry(name)
                .or_insert_with(|| serde_json::Value::Object(Default::default()));
            if let serde_json::Value::Object(keys) = object {
                let entry = keys
                    .entry(key.to_string())
                    .or_insert_with(|| serde_json::json!({"count": 0, "total_ms": 0, "max_ms": 0}));
                let read = |entry: &serde_json::Value, field: &str| {
                    entry
                        .get(field)
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0)
                };
                let updated = serde_json::json!({
                    "count": read(entry, "count").saturating_add(1),
                    "total_ms": read(entry, "total_ms").saturating_add(millis),
                    "max_ms": read(entry, "max_ms").max(millis),
                });
                *entry = updated;
            }
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

/// Count `id` under `name` once, however often it is seen while recording.
/// The metric line carries only how many distinct ids were seen
/// (`phase_distinct_counts`), never the ids.
pub(crate) fn add_distinct(name: &'static str, id: i64) {
    ACTIVE.with(|active| {
        if let Some(recorder) = active.borrow().as_ref()
            && let Ok(mut accumulated) = recorder.0.lock()
        {
            accumulated.distinct.entry(name).or_default().insert(id);
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
    /// A copy of what accumulated, with every still-open span closed at
    /// `now`, and those open spans from outermost (opened first) to innermost.
    /// For a command interrupted mid-span: its row then carries the time
    /// spent in the span it never left. Waits at most `wait` for the lock,
    /// polling, because the interrupted thread may hold it; `None` when it
    /// stays busy. A poisoned lock still yields its data.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn snapshot_closing_open_spans(
        &self,
        now: Instant,
        wait: Duration,
    ) -> Option<(Recorder, Vec<OpenSpan>)> {
        let deadline = Instant::now() + wait;
        let mut copy = loop {
            match self.0.try_lock() {
                Ok(accumulated) => break accumulated.clone(),
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    break poisoned.into_inner().clone();
                }
                Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(std::sync::TryLockError::WouldBlock) => return None,
            }
        };
        let mut open: Vec<(u64, &'static str, Instant)> = std::mem::take(&mut copy.open)
            .into_iter()
            .map(|(id, (name, started))| (id, name, started))
            .collect();
        open.sort_by_key(|&(id, _, started)| (started, id));
        let open = open
            .into_iter()
            .map(|(_, name, started)| {
                copy.add(name, started, now);
                OpenSpan {
                    name,
                    elapsed: now.saturating_duration_since(started),
                }
            })
            .collect();
        Some((Recorder(Arc::new(Mutex::new(copy))), open))
    }

    /// The spans open right now, outermost first, without closing them.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn open_spans(&self, now: Instant) -> Vec<OpenSpan> {
        let Ok(accumulated) = self.0.try_lock() else {
            return Vec::new();
        };
        let mut open: Vec<_> = accumulated.open.iter().map(|(id, v)| (*id, *v)).collect();
        open.sort_by_key(|&(id, (_, started))| (started, id));
        open.into_iter()
            .map(|(_, (name, started))| OpenSpan {
                name,
                elapsed: now.saturating_duration_since(started),
            })
            .collect()
    }

    /// Disjoint wall-clock attribution. Queue wait takes precedence over a
    /// containing child span (which can also include the spawn-lock wait).
    pub(crate) fn attribution(&self, started: Instant, ended: Instant) -> serde_json::Value {
        let accumulated = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let endpoints = accumulated
            .intervals
            .iter()
            .filter_map(|&(from, to, child)| {
                let from = from.max(started);
                let to = to.min(ended);
                (from < to).then_some([(from, child, 1), (to, child, -1)])
            })
            .flatten();
        let mut endpoints: Vec<_> = endpoints.collect();
        endpoints.sort_unstable();
        let mut child_count = 0;
        let mut queue_count = 0;
        let mut child = Duration::ZERO;
        let mut queue = Duration::ZERO;
        let mut previous = started;
        for (at, is_child, change) in endpoints {
            if queue_count > 0 {
                queue += at - previous;
            } else if child_count > 0 {
                child += at - previous;
            }
            if is_child {
                child_count += change;
            } else {
                queue_count += change;
            }
            previous = at;
        }
        let wall_ms = (ended - started).as_millis() as u64;
        let child_ms = child.as_millis() as u64;
        let queue_ms = queue.as_millis() as u64;
        serde_json::json!({
            "schema_version": 1,
            "child_command_ms": child_ms,
            "queue_wait_ms": queue_ms,
            "broker_and_unattributed_ms": wall_ms - queue_ms - child_ms,
            "retries": accumulated.totals.get("push_remote_tip_redecided").map_or(0, |span| span.count),
        })
    }

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
        let mut attachments: BTreeMap<String, serde_json::Value> = accumulated
            .attachments
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect();
        if !accumulated.distinct.is_empty() {
            let counts: BTreeMap<&str, usize> = accumulated
                .distinct
                .iter()
                .map(|(name, ids)| (*name, ids.len()))
                .collect();
            attachments.insert("phase_distinct_counts".into(), serde_json::json!(counts));
        }
        attachments
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
    fn attribution_partitions_parallel_children_and_nested_queue_time() {
        let recorder = Recorder::default();
        let _installed = install(recorder.clone());
        let start = Instant::now();
        let ms = |value| start + Duration::from_millis(value);
        record("subprocess_git", ms(1), ms(8));
        record("subprocess_other", ms(3), ms(9));
        record("process_spawn_lock_wait", ms(2), ms(4));
        record("repository_write_lock_wait", ms(8), ms(12));
        record("gate_verdict_wait", ms(0), ms(1));
        add("push_remote_tip_redecided", Duration::ZERO);
        let costs = recorder.attribution(start, ms(10));
        assert_eq!(costs["child_command_ms"], 5);
        assert_eq!(costs["queue_wait_ms"], 5);
        assert_eq!(costs["broker_and_unattributed_ms"], 0);
        assert_eq!(costs["retries"], 1);
    }

    #[test]
    fn distinct_counts_count_each_id_once_and_attach_no_id() {
        let recorder = Recorder::default();
        {
            let _installed = install(recorder.clone());
            for id in [4_711, 4_712, 4_711] {
                add_distinct("judged", id);
            }
        }
        add_distinct("judged", 4_713);
        assert_eq!(
            recorder.attachments()["phase_distinct_counts"],
            serde_json::json!({ "judged": 2 })
        );
        assert!(
            Recorder::default().attachments().is_empty(),
            "absent when nothing was counted"
        );
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
    fn configured_values_are_counted_not_timed() {
        let recorder = Recorder::default();
        {
            let _installed = install(recorder.clone());
            value("budget", Duration::from_secs(3_600));
            value("budget", Duration::from_secs(60));
        }
        let (millis, counts) = recorder.totals();
        assert_eq!(millis.get("budget"), Some(&0));
        assert_eq!(counts.get("budget"), Some(&2));
        assert_eq!(
            recorder.attachments().get("phase_values_ms"),
            Some(&serde_json::json!({"budget": 3_600_000}))
        );
    }

    #[test]
    fn keyed_occurrences_accumulate_count_total_and_max() {
        let recorder = Recorder::default();
        {
            let _installed = install(recorder.clone());
            add_keyed("holders", "retire strip.rs:10", Duration::from_secs(6));
            add_keyed("holders", "retire strip.rs:10", Duration::from_secs(9));
            add_keyed("holders", "gc gc.rs:20", Duration::from_secs(5));
        }
        assert_eq!(
            recorder.attachments().get("holders"),
            Some(&serde_json::json!({
                "retire strip.rs:10": {"count": 2, "total_ms": 15_000, "max_ms": 9_000},
                "gc gc.rs:20": {"count": 1, "total_ms": 5_000, "max_ms": 5_000},
            }))
        );
    }

    #[test]
    fn interrupted_snapshots_close_open_spans_without_touching_the_recorder() {
        let recorder = Recorder::default();
        let _installed = install(recorder.clone());
        add("finished", Duration::from_millis(2));
        let outer = span("outer");
        std::thread::sleep(Duration::from_millis(3));
        let inner = span("subprocess_git");
        std::thread::sleep(Duration::from_millis(3));
        let started = outer.started;
        let now = Instant::now();
        let (snapshot, open) = recorder
            .snapshot_closing_open_spans(now, Duration::from_millis(100))
            .expect("the lock is free");
        let names: Vec<_> = open.iter().map(|span| span.name).collect();
        assert_eq!(names, ["outer", "subprocess_git"], "outermost first");
        assert!(open[0].elapsed >= open[1].elapsed);
        assert!(open[1].elapsed >= Duration::from_millis(3));
        let (millis, counts) = snapshot.totals();
        assert!(millis["outer"] >= 6, "{millis:?}");
        assert_eq!(counts["finished"], 1);
        assert_eq!(counts["subprocess_git"], 1);
        assert!(snapshot.attribution(started, now)["child_command_ms"].as_u64() >= Some(3));
        assert_eq!(
            recorder.open_spans(now).len(),
            2,
            "still open in the original"
        );
        assert!(
            !recorder.totals().1.contains_key("outer"),
            "not closed in the original"
        );
        drop(inner);
        drop(outer);
        assert!(recorder.open_spans(Instant::now()).is_empty());
        assert_eq!(recorder.totals().1["outer"], 1);
    }

    #[test]
    fn spans_opened_without_a_recorder_are_not_tracked() {
        let outer = span("unrecorded");
        let recorder = Recorder::default();
        let _installed = install(recorder.clone());
        drop(outer);
        assert!(recorder.open_spans(Instant::now()).is_empty());
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
