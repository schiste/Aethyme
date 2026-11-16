//! The caller's arguments for `aethyme mcp connect`.
//!
//! A managed server started through `connect` gets the entry's argv (its
//! program resolution and base arguments) followed by the arguments the
//! caller gave after the package or program. They travel without a second
//! separator:
//!
//! ```text
//! aethyme mcp connect <name> --caller-args <N> [--caller-package <spec>] --fallback -- <command...>
//! aethyme mcp connect <name> --caller-args <N> [--caller-package <spec>] -- <N arguments>
//! ```
//!
//! With `--fallback`, the caller's arguments are the last `N` words of the
//! fallback command (they are its tail by construction: the fallback is the
//! caller's own command). Without it, the words after `--` are exactly the
//! `N` arguments. `--caller-package` replaces the entry's package spec (a
//! caller pinning a version the entry does not). The arguments are applied
//! before the plan, so they are part of the resolved argv and of the key.
//!
//! `connect` marks what it starts so bypass detection can tell it apart:
//! [`CONNECT_MARKER`] on the server, [`FALLBACK_MARKER`] on a fallback.

use super::config::Entry;

/// Set (to the entry name) on a server `connect` started.
pub const CONNECT_MARKER: &str = "AETHYME_MCP_CONNECT";
/// Set (to the entry name) on a fallback `connect` ran.
pub const FALLBACK_MARKER: &str = "AETHYME_MCP_FALLBACK";

/// `--caller-args` and `--caller-package`, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerOptions {
    pub args: Vec<String>,
    pub package: Option<String>,
}

/// Read the caller options from `connect`'s arguments. `Ok(None)`: none.
pub fn scan(args: &[String]) -> Result<Option<CallerOptions>, String> {
    let separator = args.iter().position(|arg| arg == "--");
    let options = &args[..separator.unwrap_or(args.len())];
    let value = |name: &str| -> Result<Option<&String>, String> {
        match options.iter().position(|arg| arg == name) {
            None => Ok(None),
            Some(index) => options
                .get(index + 1)
                .map(Some)
                .ok_or_else(|| format!("{name} needs a value")),
        }
    };
    let count = value("--caller-args")?;
    let package = value("--caller-package")?;
    if count.is_none() && package.is_none() {
        return Ok(None);
    }
    let count: usize = match count {
        Some(text) => text
            .parse()
            .map_err(|_| format!("--caller-args {text:?} is not a count"))?,
        None => 0,
    };
    if let Some(spec) = package
        && super::managed::package_name(spec).is_none()
    {
        return Err(format!("--caller-package {spec:?} is not a package spec"));
    }
    let after: &[String] = match separator {
        Some(index) => &args[index + 1..],
        None => &[],
    };
    let caller_args = if options.iter().any(|arg| arg == "--fallback") {
        if count > 0 && count >= after.len() {
            return Err(format!(
                "--caller-args {count} leaves no program in the fallback command"
            ));
        }
        after[after.len().saturating_sub(count)..].to_vec()
    } else {
        if after.len() != count {
            return Err(format!(
                "--caller-args {count} expects exactly {count} arguments after `--`, found {}",
                after.len()
            ));
        }
        after.to_vec()
    };
    Ok(Some(CallerOptions {
        args: caller_args,
        package: package.cloned(),
    }))
}

/// The entry with the caller's arguments and package applied: the entry's
/// argv, its `base_args`, then the caller's arguments without options the
/// base arguments already hold. Only a managed entry in `args = "caller"`
/// mode takes caller arguments; any other entry refuses them (and
/// `connect` runs the fallback).
pub fn apply(entry: &Entry, options: &CallerOptions) -> Result<Entry, String> {
    let base_args = match &entry.managed {
        Some(spec) if spec.args == super::managed::ArgsMode::Caller => spec.base_args.clone(),
        _ if options.args.is_empty() && options.package.is_none() => Vec::new(),
        _ => {
            return Err(format!(
                "{} takes no caller arguments (it is not a managed entry with args = \"caller\")",
                entry.name
            ));
        }
    };
    let caller_args = super::managed::without_base_options(&options.args, &base_args);
    let mut tail = base_args;
    tail.extend(caller_args);
    let mut adjusted = entry.clone();
    adjusted.argv = super::managed::apply_caller(&entry.argv, &tail, options.package.as_deref())?;
    Ok(adjusted)
}

/// The entry `connect` plans: with the caller's options applied, and a
/// caller-mode entry's `base_args` even when no caller options were given.
/// `Ok(None)`: the entry as configured.
pub fn adjust(entry: &Entry, options: Option<CallerOptions>) -> Result<Option<Entry>, String> {
    let has_base = entry.managed.as_ref().is_some_and(|spec| {
        spec.args == super::managed::ArgsMode::Caller && !spec.base_args.is_empty()
    });
    match options {
        Some(options) => apply(entry, &options).map(Some),
        None if has_base => apply(
            entry,
            &CallerOptions {
                args: Vec::new(),
                package: None,
            },
        )
        .map(Some),
        None => Ok(None),
    }
}

/// The `connect` arguments that carry a route: `--caller-args`,
/// `--caller-package`, then `--fallback -- <fallback...>` (whose tail must
/// be the caller's arguments).
pub fn connect_args(
    entry: &str,
    caller_args: &[String],
    caller_package: Option<&str>,
    fallback: &[String],
) -> Vec<String> {
    let mut args = vec!["mcp".to_string(), "connect".to_string(), entry.to_string()];
    if !caller_args.is_empty() {
        args.push("--caller-args".into());
        args.push(caller_args.len().to_string());
    }
    if let Some(package) = caller_package {
        args.push("--caller-package".into());
        args.push(package.to_string());
    }
    args.push("--fallback".into());
    args.push("--".into());
    args.extend(fallback.iter().cloned());
    args
}
