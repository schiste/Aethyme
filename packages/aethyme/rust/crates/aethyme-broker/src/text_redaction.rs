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

/// Keys whose value follows them. Matched case-insensitively anywhere; the
/// assignment markers are also searched with separators removed so whitespace
/// or invisible formatting cannot split a credential key.
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
    let (compact, source_offsets) = compact_marker_separators(text);
    let split_assignment = ASSIGNMENT_MARKERS.iter().flat_map(|marker| {
        let normalized_marker: String = marker
            .chars()
            .filter(|character| !character.is_whitespace())
            .map(|character| character.to_ascii_lowercase())
            .collect();
        compact
            .match_indices(&normalized_marker)
            .filter_map(|(index, _)| {
                let marker_end = index + normalized_marker.len();
                if marker.ends_with(' ') {
                    let source_end = source_offsets.get(marker_end - 1).copied()? + 1;
                    let source_after = source_offsets
                        .get(marker_end)
                        .copied()
                        .unwrap_or(text.len());
                    if !text
                        .get(source_end..source_after)?
                        .chars()
                        .any(is_marker_separator)
                    {
                        return None;
                    }
                }
                source_offsets.get(index).copied()
            })
            .collect::<Vec<_>>()
    });
    let split_value = VALUE_PREFIXES.iter().flat_map(|prefix| {
        compact
            .match_indices(prefix)
            .filter_map(|(index, _)| {
                source_offsets
                    .get(index)
                    .copied()
                    .filter(|&index| at_token_boundary(text, index))
            })
            .collect::<Vec<_>>()
    });
    assignment
        .chain(split_assignment)
        .chain(value)
        .chain(split_value)
        .min()
}

// Unicode 18.0.0 DerivedCoreProperties.txt Default_Ignorable_Code_Point.
// This property has no stability guarantee; update the ranges and test count
// when adopting a newer Unicode data release.
const DEFAULT_IGNORABLE_CODE_POINT_RANGES: &[(u32, u32)] = &[
    (0x00ad, 0x00ad),
    (0x034f, 0x034f),
    (0x061c, 0x061c),
    (0x115f, 0x1160),
    (0x17b4, 0x17b5),
    (0x180b, 0x180f),
    (0x200b, 0x200f),
    (0x202a, 0x202e),
    (0x2060, 0x206f),
    (0x3164, 0x3164),
    (0xfe00, 0xfe0f),
    (0xfeff, 0xfeff),
    (0xffa0, 0xffa0),
    (0xfff0, 0xfff8),
    (0x1bca0, 0x1bca3),
    (0x1d173, 0x1d17a),
    (0xe0000, 0xe0fff),
];

fn is_marker_separator(character: char) -> bool {
    let code_point = u32::from(character);
    character.is_whitespace()
        || character.is_control()
        || DEFAULT_IGNORABLE_CODE_POINT_RANGES
            .iter()
            .any(|&(start, end)| (start..=end).contains(&code_point))
}

/// Remove whitespace and invisible formatting for conservative marker
/// detection while retaining each normalized byte's position in the original
/// text, so redaction can cut at the beginning of a split marker.
fn compact_marker_separators(text: &str) -> (String, Vec<usize>) {
    let mut compact = String::with_capacity(text.len());
    let mut source_offsets = Vec::with_capacity(text.len());
    for (byte_offset, character) in text.char_indices() {
        if is_marker_separator(character) {
            continue;
        }
        let normalized = character.to_ascii_lowercase();
        compact.push(normalized);
        source_offsets.extend(std::iter::repeat_n(byte_offset, normalized.len_utf8()));
    }
    (compact, source_offsets)
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
    fn credential_markers_split_by_whitespace_are_redacted() {
        assert_eq!(
            redact_secrets("pre\nTO\nKEN=split-newline-secret"),
            "pre\n[redacted]"
        );
        assert_eq!(
            redact_secrets("pre: g\nhp_split-prefix-secret"),
            "pre: [redacted]"
        );
        assert_eq!(
            redact_secrets("pre: BEA\nRER split-bearer-secret"),
            "pre: [redacted]"
        );
        assert_eq!(
            redact_secrets("pre: Bearer\u{200b}invisible-separator-secret"),
            "pre: [redacted]"
        );
        assert_eq!(
            redact_secrets("pre: bearerless; BEA\nRER split-bearer-secret"),
            "pre: bearerless; [redacted]"
        );
        assert_eq!(
            redact_secrets("disk-space before s\nk-split-value-secret"),
            "disk-space before [redacted]"
        );
        for (separator, secret) in [
            ('\u{180b}', "mongolian-selector"),
            ('\u{e0000}', "tag-plane"),
            ('\u{e0100}', "supplementary-selector-start"),
            ('\u{e01ef}', "supplementary-selector-end"),
        ] {
            let input = format!("pre: TO{separator}KEN={secret}");
            assert_eq!(redact_secrets(&input), "pre: [redacted]", "{secret}");
        }
    }

    #[test]
    fn unicode_18_default_ignorables_cannot_hide_credential_markers() {
        let code_point_count: u32 = DEFAULT_IGNORABLE_CODE_POINT_RANGES
            .iter()
            .map(|&(start, end)| end - start + 1)
            .sum();
        assert_eq!(code_point_count, 4_174);

        for &(start, end) in DEFAULT_IGNORABLE_CODE_POINT_RANGES {
            for code_point in start..=end {
                let separator = char::from_u32(code_point)
                    .expect("the Unicode property ranges contain only scalar values");
                let input = format!("TO{separator}KEN=unicode-hidden-secret");
                assert_eq!(
                    redact_secrets(&input),
                    "[redacted]",
                    "U+{code_point:04X} must not split a credential marker"
                );
            }
        }
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
