//! `KEY=VALUE` environment files, read without a shell.
//!
//! A server started through `sh -c '. ./.env && exec server'` costs a shell
//! and runs whatever the file contains. Here the file is data: one assignment
//! per line, `#` comments, an optional `export ` prefix, and single or double
//! quotes. Nothing is expanded: `$HOME` stays the five characters `$HOME`, and
//! a backtick is a backtick. Errors name the line, never the value, because
//! these files hold credentials.

/// One parse failure: the 1-based line and what was wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFileError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for EnvFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// Whether `name` is a portable environment variable name.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// Parse `text` into ordered assignments. A later assignment to the same name
/// replaces the earlier one, as sourcing the file would.
pub fn parse(text: &str) -> Result<Vec<(String, String)>, EnvFileError> {
    let mut assignments: Vec<(String, String)> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        let fail = |message: &str| EnvFileError {
            line: line_number,
            message: message.to_string(),
        };
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export")
            .filter(|rest| rest.starts_with([' ', '\t']))
            .map(str::trim_start)
            .unwrap_or(line);
        let Some((name, rest)) = line.split_once('=') else {
            return Err(fail("expected NAME=VALUE"));
        };
        let name = name.trim_end();
        if !valid_name(name) {
            return Err(fail("invalid variable name"));
        }
        let value = parse_value(rest).map_err(fail)?;
        assignments.retain(|(existing, _)| existing != name);
        assignments.push((name.to_string(), value));
    }
    Ok(assignments)
}

fn parse_value(rest: &str) -> Result<String, &'static str> {
    let rest = rest.trim_start();
    let mut chars = rest.chars();
    match chars.next() {
        None => Ok(String::new()),
        Some('\'') => {
            let body: String = chars.collect();
            let Some(end) = body.find('\'') else {
                return Err("unterminated single quote");
            };
            trailing_is_comment(&body[end + 1..])?;
            Ok(body[..end].to_string())
        }
        Some('"') => {
            let mut value = String::new();
            let mut escaped = false;
            let mut closed_at = None;
            let body: Vec<char> = chars.collect();
            for (index, c) in body.iter().enumerate() {
                if escaped {
                    value.push(match c {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => *other,
                    });
                    escaped = false;
                } else if *c == '\\' {
                    escaped = true;
                } else if *c == '"' {
                    closed_at = Some(index);
                    break;
                } else {
                    value.push(*c);
                }
            }
            let Some(end) = closed_at else {
                return Err("unterminated double quote");
            };
            let tail: String = body[end + 1..].iter().collect();
            trailing_is_comment(&tail)?;
            Ok(value)
        }
        Some(_) => {
            // Unquoted: a `#` after whitespace starts a comment.
            let mut value = rest;
            if let Some(position) = rest
                .char_indices()
                .find(|(index, c)| {
                    *c == '#' && rest[..*index].ends_with(|p: char| p.is_whitespace())
                })
                .map(|(index, _)| index)
            {
                value = &rest[..position];
            }
            Ok(value.trim_end().to_string())
        }
    }
}

fn trailing_is_comment(tail: &str) -> Result<(), &'static str> {
    let tail = tail.trim_start();
    if tail.is_empty() || tail.starts_with('#') {
        Ok(())
    } else {
        Err("unexpected text after closing quote")
    }
}
