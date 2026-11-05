//! Secret removal for free text the broker stores or publishes.
//!
//! Coordinated operations redact their *arguments* structurally
//! (`operations::redacted_command`), because they know which flag carries a
//! value. Error messages and provider stderr are free text with no such
//! structure, so this module cuts them at the first thing that looks like a
//! credential and keeps everything before it. Cutting rather than masking is
//! deliberate: a secret's length and the text that follows it (often more of
//! the same request) are not worth the risk of leaving part of it behind.
//!
//! This is pattern matching, so it is best effort: it stops the credentials it
//! recognises and cannot promise there are no others. A caller that puts another
//! agent's text in front of a reader should also withhold files that exist to
//! hold secrets (`overlap_diff`).

/// Prefixes that begin a credential value. Matched only at a token boundary,
/// so `disk-space` or `task-text` do not read as an `sk-` key.
const VALUE_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "ghr_",
    "github_pat_",
    "sk-",
    "sk_live_",
    "rk_live_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxs-",
];

/// Prefixes too short or too common to mean a credential on their own: each
/// counts only when followed by enough characters of the value's alphabet.
const SIZED_PREFIXES: &[(&str, usize, fn(char) -> bool)] = &[
    ("AKIA", 16, is_upper_alphanumeric),
    ("ASIA", 16, is_upper_alphanumeric),
    ("AIza", 35, is_url_safe_value),
    ("npm_", 36, is_url_safe_value),
    ("eyJ", 20, is_url_safe_value),
];

fn is_upper_alphanumeric(character: char) -> bool {
    character.is_ascii_uppercase() || character.is_ascii_digit()
}

fn is_url_safe_value(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

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
    "api-key=",
    "api-key:",
    "authorization:",
    "passwd=",
    "secret_key=",
    "secret_access_key=",
    "access_key=",
    "private_key=",
];

/// Keys that hold a credential in `key: value` form. Unlike the `=` markers these
/// need a look at the value, because `token: Token,` is ordinary code.
const COLON_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "secret_key",
    "secret_access_key",
    "access_key",
    "private_key",
];

/// A line of nothing but base64 this long is key material or a blob, not prose
/// or code, even when the `BEGIN` line is out of view. With a slash it could be a
/// path, so it must be as long as a PEM body line to count.
const OPAQUE_LINE_CHARS: usize = 40;
const OPAQUE_LINE_WITH_SLASH_CHARS: usize = 60;

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
    // ASCII lowercasing keeps every byte offset, so `source_offsets` indexes
    // both. Value prefixes stay case-sensitive, as in the uncompacted search:
    // `SK-1234` is a ticket key, not a key.
    let compact_lower = compact.to_ascii_lowercase();
    let split_assignment = ASSIGNMENT_MARKERS.iter().flat_map(|marker| {
        let normalized_marker: String = marker
            .chars()
            .filter(|character| !character.is_whitespace())
            .map(|character| character.to_ascii_lowercase())
            .collect();
        compact_lower
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
        .chain(private_key_block(&lower))
        .chain(sized_value(text))
        .chain(colon_assignment(&lower))
        .chain(upper_snake_key_assignment(text))
        .chain(opaque_line(text))
        .min()
}

/// Where a PEM or PGP private key begins. Certificates and public keys are not
/// secrets, and are left alone.
fn private_key_block(lower: &str) -> Option<usize> {
    lower.match_indices("-----begin").find_map(|(index, _)| {
        let header = lower.get(index..)?.lines().next()?;
        header.contains("private key").then_some(index)
    })
}

/// Whether `line` opens a private key block that continues on later lines.
pub(crate) fn opens_private_key(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    private_key_block(&lower).is_some_and(|index| !lower[index..].contains("-----end"))
}

/// Whether `line` closes a PEM or PGP block.
pub(crate) fn closes_key_block(line: &str) -> bool {
    line.to_ascii_lowercase().contains("-----end")
}

fn sized_value(text: &str) -> Option<usize> {
    SIZED_PREFIXES
        .iter()
        .flat_map(|(prefix, length, alphabet)| {
            text.match_indices(prefix).filter_map(|(index, _)| {
                let value = text[index + prefix.len()..]
                    .chars()
                    .take_while(|character| alphabet(*character))
                    .take(*length)
                    .count();
                (value == *length && at_token_boundary(text, index)).then_some(index)
            })
        })
        .min()
}

/// `password: "hunter2"`, `"api_key": "abc"` or `token: abc123`, but not
/// `token: Token,`, `password: String` or `secret: &str`.
fn colon_assignment(lower: &str) -> Option<usize> {
    COLON_KEYS
        .iter()
        .flat_map(|key| {
            lower.match_indices(key).filter_map(|(index, _)| {
                let after = lower[index + key.len()..]
                    .strip_prefix(['"', '\'', '`'])
                    .unwrap_or(&lower[index + key.len()..]);
                let value = after.trim_start_matches([' ', '\t']).strip_prefix(':')?;
                let value = value.lines().next().unwrap_or("").trim();
                is_credential_value(value).then_some(index)
            })
        })
        .min()
}

fn is_credential_value(value: &str) -> bool {
    if let Some(quote) = value.chars().next().filter(|c| matches!(c, '"' | '\'')) {
        return value[1..].find(quote).is_some_and(|end| end > 0);
    }
    // Unquoted, as in YAML: one word holding a digit. Code carries a type or an
    // identifier here, which ends in punctuation or has no digit.
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '='))
        && value.chars().any(|c| c.is_ascii_digit())
}

/// `SIGNING_KEY=value` in an environment file: an upper-case name ending in
/// `_KEY` with a value after it.
fn upper_snake_key_assignment(text: &str) -> Option<usize> {
    text.match_indices('=').find_map(|(equals, _)| {
        let name_start = text[..equals]
            .rfind(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .map_or(0, |index| {
                // The scan stopped at a character that may be several bytes.
                index + text[index..].chars().next().map_or(1, char::len_utf8)
            });
        let has_value = text[equals + 1..]
            .chars()
            .next()
            .is_some_and(|c| !c.is_whitespace());
        (text[name_start..equals].ends_with("_KEY") && has_value).then_some(name_start)
    })
}

/// The start of a line that is only a long run of base64, after any diff prefix.
fn opaque_line(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end();
        let content = body.strip_prefix(['+', '-', ' ']).unwrap_or(body).trim();
        let needed = if content.contains('/') {
            OPAQUE_LINE_WITH_SLASH_CHARS
        } else {
            OPAQUE_LINE_CHARS
        };
        if content.len() >= needed
            && content
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
            && content.chars().any(|c| c.is_ascii_uppercase())
            && content.chars().any(|c| c.is_ascii_lowercase())
            && content.chars().any(|c| c.is_ascii_digit())
        {
            return Some(offset + line.find(content).unwrap_or(0));
        }
        offset += line.len();
    }
    None
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
/// detection, keeping case, while retaining each normalized byte's position in the original
/// text, so redaction can cut at the beginning of a split marker.
fn compact_marker_separators(text: &str) -> (String, Vec<usize>) {
    let mut compact = String::with_capacity(text.len());
    let mut source_offsets = Vec::with_capacity(text.len());
    for (byte_offset, character) in text.char_indices() {
        if is_marker_separator(character) {
            continue;
        }
        compact.push(character);
        source_offsets.extend(std::iter::repeat_n(byte_offset, character.len_utf8()));
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
    fn catches_private_keys_and_cloud_credentials() {
        for (text, secret) in [
            ("-----BEGIN RSA PRIVATE KEY-----\nMIIEabc\n", "MIIEabc"),
            ("+-----BEGIN OPENSSH PRIVATE KEY-----", "OPENSSH"),
            ("-----BEGIN PGP PRIVATE KEY BLOCK-----\nabc", "PGP"),
            (
                r#"{"private_key": "-----BEGIN PRIVATE KEY-----\nMIIEabc"}"#,
                "MIIEabc",
            ),
            ("id AKIAIOSFODNN7EXAMPLE used", "IOSFODNN7"),
            ("id ASIAIOSFODNN7EXAMPLE used", "IOSFODNN7"),
            ("aws_secret_access_key = wJalrXUtnFEMI", "wJalrXUtnFEMI"),
            ("AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI", "wJalrXUtnFEMI"),
            ("SIGNING_KEY=abc123", "abc123"),
            ("export ENCRYPTION_KEY=abc123", "abc123"),
            ("x-api-key: abc123", "abc123"),
            ("password: \"hunter\"", "hunter"),
            ("\"api_key\": \"abc\"", "abc"),
            ("db_password: hunter2", "hunter2"),
            ("key sk_live_abc used", "abc"),
            ("slack xoxb-123-abc", "123"),
            ("gitlab glpat-abc", "abc"),
            (
                "jwt eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxIn0.sig",
                "eyJzdWI",
            ),
            ("AIzaSyA1234567890abcdefghijklmnopqrstuv", "SyA1234567890"),
        ] {
            let redacted = redact_secrets(text);
            assert!(redacted.ends_with(REDACTED), "{text} -> {redacted}");
            assert!(!redacted.contains(secret), "{text} -> {redacted}");
        }
    }

    #[test]
    fn a_key_body_out_of_sight_of_its_begin_line_is_still_cut() {
        let line = "+MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7VJTUt9Us8cKj";
        assert_eq!(redact_secrets(line), "+[redacted]");
        let shorter = " MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjA";
        assert_eq!(redact_secrets(shorter), " [redacted]");
    }

    #[test]
    fn ordinary_code_and_public_material_survive() {
        for text in [
            "token: Token,",
            "password: String,",
            "secret: &str,",
            "fn check(token: &str, secret: Option<String>) -> bool {",
            "    private_key: PrivateKey,",
            "token_count: usize",
            "// the secret: rotate it",
            "-----BEGIN CERTIFICATE-----",
            "-----BEGIN PUBLIC KEY-----",
            "SORT_ORDER=asc",
            "AKIA is the prefix for access keys",
            "npm_config_registry=https://registry.npmjs.org",
            "packages/aethyme/rust/crates/aethyme-broker/src/overlap_diff.rs",
            "packages/aethyme/rust/crates/aethyme-broker/src/Overlap2",
            "0123456789abcdef0123456789abcdef01234567",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "eyJ is how encoded JSON starts",
            "const SIGNING_KEY: &str = NAME;",
        ] {
            assert_eq!(redact_secrets(text), text);
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
        assert_eq!(
            redact_secrets("ticket SK-1234 and GHP_NOTE stay readable"),
            "ticket SK-1234 and GHP_NOTE stay readable"
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
