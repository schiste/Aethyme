use super::*;

const FAILURE_STDERR_TAIL_LINES: usize = 12;
const FAILURE_STDERR_LINE_CHARS: usize = 256;

fn consume_csi_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for character in chars.by_ref() {
        if ('@'..='~').contains(&character) {
            break;
        }
    }
}

fn consume_escape_intermediate_sequence(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    first: char,
) {
    // ISO-2022 character-set designations such as ESC ( B have one or more
    // intermediate bytes before their final byte. Consume the whole sequence
    // so its final byte cannot splice into a credential marker.
    if !('\x20'..='\x2f').contains(&first) {
        return;
    }
    for character in chars.by_ref() {
        if ('\x30'..='\x7e').contains(&character) {
            break;
        }
    }
}

fn consume_terminated_escape_sequence(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    bell_terminates: bool,
) {
    while let Some(character) = chars.next() {
        if (bell_terminates && character == '\x07') || character == '\u{009c}' {
            break;
        }
        if character == '\x1b' && chars.peek() == Some(&'\\') {
            chars.next();
            break;
        }
    }
}

/// Decode stderr lossily while preserving raw C1 bytes as control characters.
/// `from_utf8_lossy` would replace a standalone 0x80..=0x9f byte, preventing
/// the terminal parser from consuming a C1 control string around a marker.
fn decode_stderr_preserving_c1(stderr: &[u8]) -> String {
    let mut decoded = String::with_capacity(stderr.len());
    let mut index = 0;
    while index < stderr.len() {
        let byte = stderr[index];
        if byte < 0x80 {
            decoded.push(char::from(byte));
            index += 1;
            continue;
        }
        if (0x80..=0x9f).contains(&byte) {
            decoded.push(char::from(byte));
            index += 1;
            continue;
        }

        let width = match byte {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 1,
        };
        let end = index.saturating_add(width);
        if width == 1 || end > stderr.len() {
            decoded.push(char::REPLACEMENT_CHARACTER);
            index += 1;
            continue;
        }
        match std::str::from_utf8(&stderr[index..end]) {
            Ok(valid) => {
                decoded.push_str(valid);
                index = end;
            }
            Err(_) => {
                decoded.push(char::REPLACEMENT_CHARACTER);
                index += 1;
            }
        }
    }
    decoded
}

/// Remove terminal controls without splitting a credential marker that was
/// deliberately or accidentally interrupted by terminal formatting.
fn stderr_without_terminal_controls(stderr: &str) -> String {
    let mut chars = stderr.chars().peekable();
    let mut printable = String::with_capacity(stderr.len());
    while let Some(character) = chars.next() {
        match character {
            '\x1b' => match chars.next() {
                Some('[') => consume_csi_sequence(&mut chars),
                Some(']') => consume_terminated_escape_sequence(&mut chars, true),
                Some('P' | 'X' | '^' | '_') => {
                    consume_terminated_escape_sequence(&mut chars, false)
                }
                Some(first) => consume_escape_intermediate_sequence(&mut chars, first),
                None => {}
            },
            '\u{009b}' => consume_csi_sequence(&mut chars),
            '\u{009d}' => consume_terminated_escape_sequence(&mut chars, true),
            '\u{0090}' | '\u{0098}' | '\u{009e}' | '\u{009f}' => {
                consume_terminated_escape_sequence(&mut chars, false)
            }
            '\n' => printable.push('\n'),
            control if control.is_control() => {}
            printable_character => printable.push(printable_character),
        }
    }
    printable
}

/// Redact both before and after terminal-control removal: either stream may
/// contain a complete credential marker that the other representation hides.
pub(super) fn redacted_failure_stderr(stderr: &[u8]) -> String {
    let decoded = decode_stderr_preserving_c1(stderr);
    let redacted_raw = crate::text_redaction::redact_secrets(&decoded);
    let printable = stderr_without_terminal_controls(&redacted_raw);
    crate::text_redaction::redact_secrets(&printable)
}

/// Preserve a small, credential-redacted tail of a failed push's stderr.
/// Hook output is the actionable reason for local pre-push refusals, but it is
/// untrusted free text and must not be copied wholesale into durable history.
pub(super) fn add_failure_stderr(details: &mut serde_json::Value, stderr: &[u8]) {
    let redacted = redacted_failure_stderr(stderr);
    let mut tail = VecDeque::with_capacity(FAILURE_STDERR_TAIL_LINES);
    for line in redacted.lines() {
        if let Some(line) = crate::text_redaction::failure_message(line, FAILURE_STDERR_LINE_CHARS)
        {
            if tail.len() == FAILURE_STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    }
    if !tail.is_empty() {
        details["failure_output"] = json!({ "stderr_tail": tail.into_iter().collect::<Vec<_>>() });
    }
}

#[cfg(test)]
mod failure_stderr_tests {
    use super::{FAILURE_STDERR_LINE_CHARS, FAILURE_STDERR_TAIL_LINES, add_failure_stderr};
    use serde_json::{Value, json};

    #[test]
    fn persisted_stderr_is_a_redacted_bounded_tail() {
        let mut lines: Vec<String> = (0..16).map(|index| format!("diagnostic {index}")).collect();
        lines.push("é".repeat(400));
        lines.push("TOKEN=ghp_example_secret_not_real".into());
        let mut details = json!({});

        add_failure_stderr(&mut details, lines.join("\n").as_bytes());

        let stderr_tail: &Vec<Value> = details["failure_output"]["stderr_tail"]
            .as_array()
            .expect("failure stderr is recorded");
        assert_eq!(stderr_tail.len(), FAILURE_STDERR_TAIL_LINES);
        assert_eq!(stderr_tail.last().unwrap(), "[redacted]");
        assert!(stderr_tail.iter().all(|line| {
            line.as_str()
                .is_some_and(|line| line.chars().count() <= FAILURE_STDERR_LINE_CHARS + 1)
        }));
        assert!(
            stderr_tail.iter().any(|line| {
                line.as_str().is_some_and(|line| {
                    line.chars().count() == FAILURE_STDERR_LINE_CHARS + 1 && line.ends_with('…')
                })
            }),
            "long diagnostic lines are capped with an ellipsis: {stderr_tail:?}"
        );
        assert!(!details.to_string().contains("ghp_example_secret_not_real"));
        assert_eq!(stderr_tail.first().unwrap(), "diagnostic 6");
    }

    #[test]
    fn redacts_markers_split_across_lines_before_persisting_stderr() {
        let mut details = json!({});
        add_failure_stderr(&mut details, b"gate refused\nTO\nKEN=split-newline-secret");

        assert!(details.to_string().contains("gate refused"));
        assert!(
            !details.to_string().contains("split-newline-secret"),
            "a credential marker split across lines must be redacted before tailing: {details}"
        );
    }

    #[test]
    fn redacts_markers_that_ansi_sequence_parsing_would_obscure() {
        let mut details = json!({});
        add_failure_stderr(&mut details, b"TO\x1b[31mKEN=ansi-csi-secret");

        assert!(
            !details.to_string().contains("ansi-csi-secret"),
            "ANSI parsing must not make credential markers disappear before redaction: {details}"
        );
    }

    #[test]
    fn redacts_markers_obscured_by_escape_charset_sequences() {
        let mut details = json!({});
        add_failure_stderr(&mut details, b"TO\x1b(BKEN=ansi-charset-secret");

        assert!(
            !details.to_string().contains("ansi-charset-secret"),
            "ANSI charset parsing must not make credential markers disappear before redaction: {details}"
        );
    }

    #[test]
    fn redacts_markers_obscured_by_seven_bit_sos_sequences() {
        let mut details = json!({});
        add_failure_stderr(&mut details, b"TO\x1bXx\x1b\\KEN=ansi-sos-secret");

        assert!(
            !details.to_string().contains("ansi-sos-secret"),
            "SOS controls must not expose credential markers hidden in string payloads: {details}"
        );
    }

    #[test]
    fn redacts_markers_obscured_by_raw_c1_sos_bytes() {
        let mut details = json!({});
        add_failure_stderr(&mut details, b"TO\x98x\x9cKEN=raw-c1-sos-secret");

        assert!(
            !details.to_string().contains("raw-c1-sos-secret"),
            "raw C1 SOS bytes must not hide credential markers before redaction: {details}"
        );
    }
}
