//! Bounded terminal rendering for repository-derived text previews.

pub(crate) const DIFF_PREVIEW_LIMIT_BYTES: usize = 256 * 1024;

pub(crate) fn escape_terminal_field(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            escaped.extend(character.escape_debug());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

pub(crate) fn bounded_terminal_text_preview(text: &str, byte_limit: usize) -> (String, bool) {
    let mut preview = String::with_capacity(byte_limit.min(text.len()));
    let mut truncated = false;
    for character in text.chars() {
        if character.is_control() && !matches!(character, '\n' | '\t') {
            let escaped: String = character.escape_debug().collect();
            if preview.len().saturating_add(escaped.len()) > byte_limit {
                truncated = true;
                break;
            }
            preview.push_str(&escaped);
        } else {
            let mut buffer = [0; 4];
            let encoded = character.encode_utf8(&mut buffer);
            if preview.len().saturating_add(encoded.len()) > byte_limit {
                truncated = true;
                break;
            }
            preview.push_str(encoded);
        }
    }
    if truncated {
        if let Some(last_complete_line) = preview.rfind('\n') {
            preview.truncate(last_complete_line + 1);
        } else {
            preview.clear();
        }
    }
    (preview, truncated)
}
