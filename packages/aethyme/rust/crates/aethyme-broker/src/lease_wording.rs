//! What agents read about leases.
//!
//! "Lease" names three different things in the broker, and agents could not
//! tell which one a message was about, so advisory path overlaps read as
//! blocks:
//!
//! - **Path leases** record which session is editing which paths. They never
//!   block an edit or a submit; an overlap is information.
//! - **Host resources** (build and cache slots, the preparation cache) are
//!   capacity. Waiting for one is normal and the operation retries.
//! - **Task leases** tie a session to the agent process holding it. A
//!   state-changing command from another process is refused until the holder
//!   is gone or the session is taken over.
//!
//! Every rendered message about one of them goes through this module, so the
//! three keep one vocabulary each: path leases are "also editing", host
//! resources are a "build/cache slot", task leases are the "session holder".
//! Internal names and machine-readable fields stay as they are.

/// The clause every path-overlap message carries.
pub(crate) const PATH_LEASE_INFORMATIONAL: &str = "informational: path leases never block your edits or submit; coordinate only if you change the same lines";

/// One line for another session editing an overlapping path:
/// `<holder> is also editing <path> (informational: …)`, then `; <detail>`
/// when the detail says anything.
pub(crate) fn path_overlap(holder: &str, path: &str, detail: Option<&str>) -> String {
    let mut line = format!("{holder} is also editing {path} ({PATH_LEASE_INFORMATIONAL})");
    if let Some(detail) = detail.map(str::trim).filter(|detail| !detail.is_empty()) {
        line.push_str("; ");
        line.push_str(detail);
    }
    line
}

/// [`path_overlap`] for a holder known only by its session id.
pub(crate) fn session_path_overlap(session_id: i64, path: &str, detail: Option<&str>) -> String {
    path_overlap(&format!("session {session_id}"), path, detail)
}

/// The merge-conflict-risk label for an assessed overlap severity (`high`
/// when the two sessions changed the same lines, `low` when they merge).
pub(crate) fn merge_conflict_risk(severity: Option<&str>) -> String {
    format!("merge-conflict risk: {}", severity.unwrap_or("low"))
}

/// A host resource (a build or cache slot) that is busy right now.
pub(crate) fn host_resource_busy(what: &str, retry_after_secs: Option<u64>) -> String {
    let retry = retry_after_secs
        .map(|secs| format!("retry after {secs} s"))
        .unwrap_or_else(|| "retry shortly".into());
    format!(
        "build/cache slot busy (host resource, not a path lease): {what}; waiting is normal; {retry}"
    )
}

/// A host resource request that could not be granted, named by what was
/// requested: a build/cache slot (capacity pools and exclusive cache keys), a
/// port, managed storage, or else a host resource. Only ordinary contention
/// (`resource_contention`) on slots is worth waiting for; port exhaustion, a
/// capacity-policy mismatch, an orphaned holder or a managed-storage refusal
/// says what happened without that advice.
pub(crate) fn host_resource_conflict<'a>(
    code: &str,
    message: &str,
    kinds: impl IntoIterator<Item = &'a str>,
) -> String {
    let kinds: Vec<&str> = kinds.into_iter().collect();
    let slot = |kind: &&str| matches!(*kind, "capacity" | "exclusive_key");
    let noun = if code.starts_with("managed_storage") {
        "managed storage"
    } else if !kinds.is_empty() && kinds.iter().all(slot) {
        "build/cache slot"
    } else if !kinds.is_empty() && kinds.iter().all(|kind| *kind == "tcp_port") {
        "port"
    } else {
        "host resource"
    };
    if code == "resource_contention" && !kinds.is_empty() && kinds.iter().all(slot) {
        host_resource_busy(message, None)
    } else if noun == "host resource" {
        format!("host resource unavailable (not a path lease): {message}")
    } else {
        format!("{noun} unavailable (host resource, not a path lease): {message}")
    }
}

/// A state-changing command refused because another live agent process is
/// the session holder with a finite task lease.
pub(crate) fn task_lease_holder_refusal(session_id: i64) -> String {
    format!(
        "session {session_id} is held by another agent process (session holder with a finite task lease, not a path lease); this state-changing command must be run by the session holder or after `--take-over`. Read-only commands such as `broker status` are never refused"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_overlap_says_it_is_informational_and_never_says_holds_or_conflict() {
        let line = session_path_overlap(12, "src/lib.rs", None);
        assert_eq!(
            line,
            "session 12 is also editing src/lib.rs (informational: path leases never block your edits or submit; coordinate only if you change the same lines)"
        );
        for misleading in ["holds", "owned by", "conflict", "blocked"] {
            assert!(!line.contains(misleading), "{misleading}: {line}");
        }
        assert_eq!(
            session_path_overlap(12, "src/lib.rs", Some("  ")),
            line,
            "an empty detail adds nothing"
        );
        assert!(
            session_path_overlap(12, "src/lib.rs", Some("both changed line 4"))
                .ends_with("; both changed line 4")
        );
    }

    #[test]
    fn host_resource_and_task_lease_wording_name_their_kind() {
        let busy = host_resource_busy("rust-workspace cache", Some(30));
        assert!(busy.starts_with("build/cache slot busy"), "{busy}");
        assert!(busy.contains("not a path lease"), "{busy}");
        assert!(
            busy.ends_with("waiting is normal; retry after 30 s"),
            "{busy}"
        );
        assert!(host_resource_busy("x", None).ends_with("retry shortly"));
        assert_eq!(
            host_resource_conflict("resource_contention", "pool full", ["capacity"]),
            host_resource_busy("pool full", None)
        );
        let mismatch =
            host_resource_conflict("capacity_policy_mismatch", "limits differ", ["capacity"]);
        assert_eq!(
            mismatch,
            "build/cache slot unavailable (host resource, not a path lease): limits differ"
        );
        let ports = host_resource_conflict(
            "resource_contention",
            "no broker-free and OS-bindable port remains",
            ["tcp_port"],
        );
        assert_eq!(
            ports,
            "port unavailable (host resource, not a path lease): no broker-free and OS-bindable port remains"
        );
        let storage =
            host_resource_conflict("managed_storage_claim_missing", "claim does not exist", []);
        assert_eq!(
            storage,
            "managed storage unavailable (host resource, not a path lease): claim does not exist"
        );
        for text in [&mismatch, &ports, &storage] {
            assert!(!text.contains("waiting is normal"), "{text}");
            assert!(!text.contains("build/cache slot busy"), "{text}");
        }
        assert_eq!(
            host_resource_conflict("other", "odd", ["tcp_port", "capacity"]),
            "host resource unavailable (not a path lease): odd"
        );

        let refusal = task_lease_holder_refusal(7);
        assert!(refusal.contains("session holder"), "{refusal}");
        assert!(refusal.contains("not a path lease"), "{refusal}");
        assert!(refusal.contains("never refused"), "{refusal}");
        assert_eq!(
            merge_conflict_risk(Some("high")),
            "merge-conflict risk: high"
        );
        assert_eq!(merge_conflict_risk(None), "merge-conflict risk: low");
    }
}
