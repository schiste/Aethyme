//! The CLI is described in two places -- `docs/reference/cli.md` and the usage
//! text `aethyme broker` prints -- and nothing tied them together, so they drift
//! one flag at a time.
//!
//! Observed twice in one working session: `--agent` and then the
//! `main reconcile` resolution flags were added to the reference and not the
//! usage text, and `gates affected --why` was documented without ever being
//! implemented. Each was individually trivial and none had a failing check.

use std::collections::{BTreeMap, BTreeSet};

/// Every `--flag` token on a line.
fn flags(line: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let bytes: Vec<char> = line.chars().collect();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == '-' && bytes[index + 1] == '-' && index + 2 < bytes.len() {
            let mut end = index + 2;
            while end < bytes.len()
                && (bytes[end].is_ascii_lowercase()
                    || bytes[end].is_ascii_digit()
                    || bytes[end] == '-')
            {
                end += 1;
            }
            let flag: String = bytes[index..end].iter().collect();
            if flag.len() > 2 {
                found.insert(flag);
            }
            index = end;
        } else {
            index += 1;
        }
    }
    found
}

/// The command words before the first flag or bracket, e.g. `main reconcile plan`.
fn command(line: &str) -> String {
    line.trim()
        .trim_start_matches("- ")
        .trim_matches('`')
        .split('[')
        .next()
        .unwrap_or_default()
        .split("--")
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[test]
fn every_documented_broker_flag_appears_in_the_usage_text() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .expect("repository root");
    let reference = std::fs::read_to_string(root.join("packages/aethyme/docs/reference/cli.md"))
        .expect("cli reference");
    let usage_source = std::fs::read_to_string(
        root.join("packages/aethyme/rust/crates/aethyme-broker/src/cli/mod.rs"),
    )
    .expect("cli source");

    let mut usage: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for line in usage_source.lines() {
        if let Some(rest) = line.strip_prefix("  aethyme broker ") {
            let full = format!("aethyme broker {rest}");
            usage
                .entry(command(&full))
                .or_default()
                .extend(flags(&full));
        }
    }
    assert!(
        usage.len() > 40,
        "usage text was not parsed; found {} commands",
        usage.len()
    );

    let mut problems = Vec::new();
    for line in reference.lines() {
        if !line.starts_with("- `aethyme broker ") {
            continue;
        }
        let mut documented = flags(line);
        if documented.is_empty() {
            continue;
        }
        // The reference uses the public spellings (`advanced leases claim`,
        // `status readiness`, `start --adopt`); the usage text lists internal
        // commands. Resolve the way the router does before comparing.
        let span = line
            .trim_start_matches("- `aethyme broker ")
            .split('`')
            .next()
            .unwrap_or_default();
        // An optional `[--json]` is still `--json` to the resolver, which is
        // what turns a bare `unblock` into the `blockers` listing.
        let words: Vec<String> = span
            .split_whitespace()
            .map(|word| if word == "[--json]" { "--json" } else { word })
            .map(str::to_string)
            .collect();
        let mut args = aethyme_broker::cli::resolve(&words).args;
        // `start` merges by flag, as the dispatcher does; `--adopt` only
        // selects the form, so the adopt usage line does not list it.
        if args.first().map(String::as_str) == Some("start") {
            let has = |flag: &str| args.iter().any(|arg| arg == flag);
            if has("--adopt") || has("--reuse") || has("--replace-stale") {
                args[0] = "adopt".to_string();
                documented.remove("--adopt");
            } else if has("--cmd") {
                args[0] = "start-agent".to_string();
            }
        }
        let name = command(&format!("aethyme broker {}", args.join(" ")));
        match usage.get(&name) {
            // A documented flag absent from the usage text is either an
            // undocumented-in-help feature or, worse, one that does not exist.
            Some(known) => {
                let missing: Vec<_> = documented.difference(known).cloned().collect();
                if !missing.is_empty() {
                    problems.push(format!("{name}: {missing:?} documented but not in usage"));
                }
            }
            None => problems.push(format!("{name}: documented with no usage line")),
        }
    }

    assert!(
        problems.is_empty(),
        "cli.md and the usage text disagree:\n  {}",
        problems.join("\n  ")
    );
}
