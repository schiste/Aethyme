//! Shared command-line flag handling for the engine front ends.
//!
//! ## Why this module exists
//!
//! Every graph front end (`graph_cli`, `query_cli`, `task_cli`,
//! `facts_cli`, `analyze_cli`, `explore_cli`) had its own hand-rolled
//! `read_option` (`args.windows(2).find(|p| p[0] == flag)`) and its own
//! `has_flag`. Three consequences followed, all observed in production:
//!
//! 1. **Unknown flags were silently ignored.** Nothing validated the
//!    argument list, so a misspelled or misplaced flag produced a
//!    plausible result with no indication the request had been
//!    misunderstood.
//! 2. **`--json` and `--json-output` were mutually exclusive per
//!    subcommand family.** `graph node` accepted only
//!    `--json-output`, while `graph status` accepted only `--json`, so
//!    an agent that guessed wrong got human text on stdout and exit 0.
//! 3. **Malformed numeric values silently fell back to defaults.**
//!    `--budget-ms abc` became the default budget with no diagnostic,
//!    so an agent could believe it had constrained a query it had not.
//!
//! [`ParsedArgs`] fixes all three by parsing once, rejecting anything
//! unrecognised, and treating both JSON spellings as equivalent.
//!
//! ## Contract
//!
//! - Both `--json` and `--json-output` mean the same thing everywhere.
//! - A flag that is unknown for the calling command is a usage error,
//!   never a silent no-op. The error names the offending flag and the
//!   flags the command does accept, so the caller can correct itself
//!   without reading the source.
//! - A flag missing its value is a usage error rather than being read
//!   as a positional or defaulted.
//! - Numeric options reject unparseable values instead of defaulting.

use std::collections::BTreeSet;

/// Whether a flag takes a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    /// A boolean switch such as `--show-observability`.
    Flag,
    /// A flag that consumes the following argument, e.g. `--repo PATH`.
    Value,
}

/// A parsed argument list for one command.
///
/// Positional arguments keep their relative order. Flag values are
/// consumed, so `--repo /r target` leaves `target` as the single
/// positional rather than treating `/r` as the repo and losing it.
#[derive(Debug, Default, Clone)]
pub struct ParsedArgs {
    positionals: Vec<String>,
    values: BTreeSet<String>,
    options: Vec<(String, String)>,
}

/// Description of one accepted flag, used to build the `unknown flag`
/// error message.
#[derive(Debug, Clone, Copy)]
pub struct FlagSpec {
    pub name: &'static str,
    pub arity: Arity,
}

impl FlagSpec {
    pub const fn flag(name: &'static str) -> Self {
        Self {
            name,
            arity: Arity::Flag,
        }
    }

    pub const fn value(name: &'static str) -> Self {
        Self {
            name,
            arity: Arity::Value,
        }
    }
}

/// Parse `args` against `specs`.
///
/// `command` is used only in error messages, e.g. `aethyme graph node`.
/// Returns a usage error naming the first unrecognised token.
pub fn parse(command: &str, args: &[String], specs: &[FlagSpec]) -> Result<ParsedArgs, String> {
    let mut parsed = ParsedArgs::default();
    let mut index = 0usize;

    while index < args.len() {
        let token = &args[index];
        if !token.starts_with('-') || token == "-" {
            parsed.positionals.push(token.clone());
            index += 1;
            continue;
        }

        // Accept `--flag=value` as well as `--flag value`, so a value
        // containing `=` (JSON is the common case) survives intact.
        let (name, inline_value) = match token.split_once('=') {
            Some((name, value)) => (name.to_string(), Some(value.to_string())),
            None => (token.clone(), None),
        };

        let Some(spec) = specs.iter().find(|spec| spec.name == name) else {
            return Err(unknown_flag_error(command, &name, specs));
        };

        match spec.arity {
            Arity::Flag => {
                if inline_value.is_some() {
                    return Err(format!(
                        "usage: {command}: flag {name} does not take a value"
                    ));
                }
                parsed.values.insert(name);
                index += 1;
            }
            Arity::Value => {
                let value = match inline_value {
                    Some(value) => {
                        index += 1;
                        value
                    }
                    None => {
                        let Some(next) = args.get(index + 1) else {
                            return Err(format!("usage: {command}: flag {name} requires a value"));
                        };
                        index += 2;
                        next.clone()
                    }
                };
                parsed.options.push((name, value));
            }
        }
    }

    Ok(parsed)
}

fn unknown_flag_error(command: &str, name: &str, specs: &[FlagSpec]) -> String {
    let mut accepted: Vec<&str> = specs.iter().map(|spec| spec.name).collect();
    accepted.sort_unstable();
    accepted.dedup();
    format!(
        "usage: {command}: unknown flag {name}\naccepted flags: {}",
        accepted.join(", ")
    )
}

impl ParsedArgs {
    /// Positional arguments in the order they appeared.
    pub fn positionals(&self) -> &[String] {
        &self.positionals
    }

    /// The nth positional, if present.
    pub fn positional(&self, index: usize) -> Option<&String> {
        self.positionals.get(index)
    }

    /// True when a boolean switch is present.
    pub fn has(&self, name: &str) -> bool {
        self.values.contains(name)
    }

    /// True when either JSON spelling is present.
    ///
    /// `--json` and `--json-output` are accepted interchangeably on
    /// every command, so a caller that guesses the wrong spelling gets
    /// machine-readable output rather than silently receiving prose.
    pub fn wants_json(&self) -> bool {
        self.has("--json") || self.has("--json-output")
    }

    /// The last value given for an option, or `None` when absent.
    ///
    /// Last-wins matches the previous `windows(2).find(...)`-style
    /// parsers for the common case and is defined rather than
    /// order-dependent for repeats.
    pub fn option(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Every value given for a repeatable option, in order.
    pub fn options(&self, name: &str) -> Vec<&str> {
        self.options
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// Parse a numeric option, rejecting unparseable values.
    ///
    /// The previous behaviour of `.ok().and_then(|s| s.parse().ok())
    /// .unwrap_or(DEFAULT)` meant `--budget-ms abc` silently used the
    /// default, so an agent could believe it had bounded a query it had
    /// not. A malformed value is now an error.
    pub fn parse_number<T>(&self, command: &str, name: &str) -> Result<Option<T>, String>
    where
        T: std::str::FromStr,
    {
        let Some(raw) = self.option(name) else {
            return Ok(None);
        };
        raw.parse::<T>()
            .map(Some)
            .map_err(|_| format!("usage: {command}: flag {name} expects a number, got {raw:?}"))
    }

    /// Reject surplus positionals so a typo'd extra argument cannot be
    /// silently ignored.
    pub fn expect_max_positionals(&self, command: &str, max: usize) -> Result<(), String> {
        if self.positionals.len() > max {
            return Err(format!(
                "usage: {command}: expected at most {max} positional argument(s), got {} ({})",
                self.positionals.len(),
                self.positionals.join(", ")
            ));
        }
        Ok(())
    }
}

/// The two JSON spellings, as specs. Pass to [`parse`] on every command
/// that emits JSON.
pub fn json_specs() -> [FlagSpec; 2] {
    [FlagSpec::flag("--json"), FlagSpec::flag("--json-output")]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn specs() -> Vec<FlagSpec> {
        vec![
            FlagSpec::flag("--json"),
            FlagSpec::flag("--json-output"),
            FlagSpec::value("--repo"),
        ]
    }

    #[test]
    fn both_json_spellings_are_equivalent() {
        for spelling in ["--json", "--json-output"] {
            let parsed = parse("aethyme graph node", &args(&[spelling]), &specs()).unwrap();
            assert!(parsed.wants_json(), "{spelling} must request JSON");
        }
    }

    #[test]
    fn no_json_flag_means_text() {
        let parsed = parse("aethyme graph node", &args(&[]), &specs()).unwrap();
        assert!(!parsed.wants_json());
    }

    #[test]
    fn unknown_flag_is_an_error_naming_the_accepted_set() {
        let error = parse("aethyme graph node", &args(&["--jsn"]), &specs()).unwrap_err();
        assert!(error.contains("unknown flag --jsn"), "{error}");
        assert!(error.contains("--json"), "{error}");
        assert!(error.contains("--repo"), "{error}");
    }

    #[test]
    fn flag_missing_its_value_is_an_error() {
        let error = parse("aethyme graph node", &args(&["--repo"]), &specs()).unwrap_err();
        assert!(error.contains("requires a value"), "{error}");
    }

    #[test]
    fn inline_values_survive_embedded_equals() {
        let parsed = parse(
            "aethyme task pack",
            &args(&["--repo=/r", "--params={\"a\":1}"]),
            &[
                FlagSpec::flag("--json"),
                FlagSpec::value("--repo"),
                FlagSpec::value("--params"),
            ],
        )
        .unwrap();
        assert_eq!(parsed.option("--repo"), Some("/r"));
        assert_eq!(parsed.option("--params"), Some("{\"a\":1}"));
    }

    #[test]
    fn flag_value_is_not_treated_as_a_positional() {
        // Regression: the old `filter(|a| !a.starts_with("--"))` kept
        // flag *values*, so `--repo /r target` yielded two positionals
        // and `/r` was silently accepted as the repo by accident.
        let parsed = parse(
            "aethyme graph node",
            &args(&["--repo", "/r", "target"]),
            &specs(),
        )
        .unwrap();
        assert_eq!(parsed.option("--repo"), Some("/r"));
        assert_eq!(parsed.positionals(), &["target".to_string()]);
    }

    #[test]
    fn repeated_option_is_last_wins_and_defined() {
        let parsed = parse(
            "aethyme graph node",
            &args(&["--repo", "/a", "--repo", "/b"]),
            &specs(),
        )
        .unwrap();
        assert_eq!(parsed.option("--repo"), Some("/b"));
    }

    #[test]
    fn repeatable_option_collects_every_value() {
        let parsed = parse(
            "aethyme explore",
            &args(&["--search-root", "a", "--search-root", "b"]),
            &[FlagSpec::value("--search-root")],
        )
        .unwrap();
        assert_eq!(parsed.options("--search-root"), vec!["a", "b"]);
    }

    #[test]
    fn malformed_number_is_an_error_not_a_silent_default() {
        // Regression: `--budget-ms abc` used to fall back to the default
        // budget with no diagnostic.
        let parsed = parse(
            "aethyme explore",
            &args(&["--budget-ms", "abc"]),
            &[FlagSpec::value("--budget-ms")],
        )
        .unwrap();
        let result: Result<Option<u64>, String> =
            parsed.parse_number("aethyme explore", "--budget-ms");
        let error = result.unwrap_err();
        assert!(error.contains("expects a number"), "{error}");
        assert!(error.contains("abc"), "{error}");
    }

    #[test]
    fn valid_number_round_trips() {
        let parsed = parse(
            "aethyme explore",
            &args(&["--budget-ms", "2500"]),
            &[FlagSpec::value("--budget-ms")],
        )
        .unwrap();
        let value: Option<u64> = parsed
            .parse_number("aethyme explore", "--budget-ms")
            .unwrap();
        assert_eq!(value, Some(2500));
    }

    #[test]
    fn absent_number_is_none_not_an_error() {
        let parsed = parse("aethyme explore", &args(&[]), &specs()).unwrap();
        let value: Option<u64> = parsed
            .parse_number("aethyme explore", "--budget-ms")
            .unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn surplus_positionals_are_rejected() {
        let parsed = parse(
            "aethyme graph node",
            &args(&["/r", "target", "extra"]),
            &specs(),
        )
        .unwrap();
        let error = parsed
            .expect_max_positionals("aethyme graph node", 2)
            .unwrap_err();
        assert!(error.contains("at most 2"), "{error}");
        assert!(error.contains("extra"), "{error}");
    }

    #[test]
    fn boolean_flag_rejects_an_inline_value() {
        let error = parse("aethyme graph node", &args(&["--json=1"]), &specs()).unwrap_err();
        assert!(error.contains("does not take a value"), "{error}");
    }

    #[test]
    fn double_dash_separator_is_not_yet_special() {
        // `-` alone is a positional, not a flag.
        let parsed = parse("aethyme graph node", &args(&["-"]), &specs()).unwrap();
        assert_eq!(parsed.positionals(), &["-".to_string()]);
    }
}
