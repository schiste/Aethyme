//! Secret removal for free text the broker stores or publishes.
//!
//! Coordinated operations redact their *arguments* structurally
//! (`operations::redacted_command`), because they know which flag carries a
//! value. Error messages and provider stderr are free text with no such
//! structure, so this module cuts them at the first thing that looks like a
//! credential and keeps everything before it. Cutting rather than masking is
//! deliberate: a secret's length and the text that follows it (often more of
//! the same request) are not worth the risk of leaving part of it behind.

/// Prefixes that begin a credential value. Matched only at a token boundary,
/// so `disk-space` or `task-text` do not read as an `sk-` key.
const VALUE_PREFIXES: &[&str] = &["ghp_", "gho_", "ghs_", "ghu_", "ghr_", "github_pat_", "sk-"];

/// Keys whose value follows them. Matched case-insensitively anywhere, so
/// `GITHUB_TOKEN=` and `Authorization:` are both caught.
const ASSIGNMENT_MARKERS: &[&str] = &[
    "bearer ",
    "token=",
    "secret=",
    "password=",
    "api_key=",
    "apikey=",
    "authorization:",
];

const REDACTED: &str = "[redacted]";

/// Remove credentials from `text`: URL user-info is replaced in place, and
/// the text is cut at the earliest remaining secret marker.
pub(crate) fn redact_secrets(text: &str) -> String {
    let mut redacted = redact_url_userinfo(text);
    if let Some(index) = earliest_secret(&redacted) {
        redacted.truncate(index);
        redacted.push_str(REDACTED);
    }
    redacted
}

/// A failure message fit for durable local storage: secrets removed, then
/// capped at `max_chars` characters on a character boundary.
pub(crate) fn failure_message(text: &str, max_chars: usize) -> Option<String> {
    let redacted = redact_secrets(text.trim());
    if redacted.is_empty() {
        return None;
    }
    if redacted.chars().count() <= max_chars {
        return Some(redacted);
    }
    let mut capped: String = redacted.chars().take(max_chars).collect();
    capped.push('…');
    Some(capped)
}

/// The cut must be at the earliest marker in the *text*, not at whichever
/// marker happens to come first in a list: ordering by the list published
/// everything that appeared before the matched one.
fn earliest_secret(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let assignment = ASSIGNMENT_MARKERS
        .iter()
        .filter_map(|marker| lower.find(marker));
    let value = VALUE_PREFIXES.iter().flat_map(|prefix| {
        text.match_indices(prefix)
            .map(|(index, _)| index)
            .filter(|&index| at_token_boundary(text, index))
    });
    assignment.chain(value).min()
}

fn at_token_boundary(text: &str, index: usize) -> bool {
    text[..index]
        .chars()
        .next_back()
        .is_none_or(|previous| !previous.is_ascii_alphanumeric() && previous != '-')
}

/// `https://user:token@host/...` keeps its host and path; the user-info goes.
fn redact_url_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(scheme_end) = rest.find("://") {
        let authority_start = scheme_end + 3;
        out.push_str(&rest[..authority_start]);
        let after = &rest[authority_start..];
        let authority_len = after
            .find(|c: char| c == '/' || c.is_whitespace() || matches!(c, '"' | '\'' | '>'))
            .unwrap_or(after.len());
        let authority = &after[..authority_len];
        match authority.rfind('@') {
            Some(at) => {
                out.push_str(REDACTED);
                out.push_str(&authority[at..]);
            }
            None => out.push_str(authority),
        }
        rest = &after[authority_len..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_at_the_earliest_secret_whatever_the_marker_order() {
        let redacted = redact_secrets("make ci PASSWORD=hunter2 TOKEN=ghp_abc");
        assert_eq!(redacted, "make ci [redacted]");
        let redacted = redact_secrets("run TOKEN=ghp_abc PASSWORD=hunter2");
        assert_eq!(redacted, "run [redacted]");
    }

    #[test]
    fn catches_prefixed_keys_and_case_variants() {
        for (text, secret) in [
            ("auth failed for ghp_0123456789abcdef", "0123456789abcdef"),
            ("GITHUB_TOKEN=abc123 rejected", "abc123"),
            ("header Authorization: Bearer q9z", "q9z"),
            ("sent bearer q9z", "q9z"),
            ("key github_pat_11ABC is expired", "11ABC"),
            ("gho_xyz used", "xyz"),
        ] {
            let redacted = redact_secrets(text);
            assert!(redacted.ends_with(REDACTED), "{text} -> {redacted}");
            assert!(!redacted.contains(secret), "{text} -> {redacted}");
        }
    }

    #[test]
    fn ordinary_words_containing_a_prefix_survive() {
        let text = "disk-space low; task-text unchanged; risk-free";
        assert_eq!(redact_secrets(text), text);
    }

    #[test]
    fn url_userinfo_is_replaced_and_the_rest_kept() {
        let redacted = redact_secrets(
            "fatal: could not read from https://x-access-token:s3cr3t@github.com/o/r.git: 403",
        );
        assert_eq!(
            redacted,
            "fatal: could not read from https://[redacted]@github.com/o/r.git: 403"
        );
        assert_eq!(
            redact_secrets("see https://github.com/o/r/pull/1"),
            "see https://github.com/o/r/pull/1"
        );
    }

    #[test]
    fn failure_message_is_capped_on_a_character_boundary() {
        let long = "é".repeat(600);
        let message = failure_message(&long, 500).unwrap();
        assert_eq!(message.chars().count(), 501);
        assert!(message.ends_with('…'));
        assert_eq!(failure_message("  \n ", 500), None);
        assert_eq!(failure_message("short", 500).as_deref(), Some("short"));
    }
}
