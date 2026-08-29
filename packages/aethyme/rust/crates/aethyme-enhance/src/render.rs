//! Rendering primitives: placeholder substitution and generated-block
//! splicing — byte-compatible with `enhance.py`'s `_render` and the
//! AETHYME:BEGIN/END handling.

use crate::{BLOCK_BEGIN, BLOCK_END, PLACEHOLDER};

/// Substitute `{{AETHYME_ROOT}}` throughout a template.
pub fn substitute_root(template: &str, root: &str) -> String {
    template.replace(PLACEHOLDER, root)
}

/// Replace the generated block inside `existing` with `generated`,
/// preserving any surrounding hand-written content. When no markers
/// exist, the generated block is the whole document (the Python
/// deploy's created/replaced semantics).
pub fn splice_generated_block(existing: &str, generated: &str) -> String {
    let (Some(begin), Some(end)) = (existing.find(BLOCK_BEGIN), existing.find(BLOCK_END)) else {
        return generated.to_string();
    };
    if end < begin {
        return generated.to_string();
    }
    let after = end + BLOCK_END.len();
    format!("{}{}{}", &existing[..begin], generated, &existing[after..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_all_occurrences() {
        assert_eq!(
            substitute_root(
                "run {{AETHYME_ROOT}}/bin and {{AETHYME_ROOT}}/lib",
                "/opt/ae"
            ),
            "run /opt/ae/bin and /opt/ae/lib"
        );
    }

    #[test]
    fn splice_preserves_surrounding_content() {
        let existing = format!("intro\n{BLOCK_BEGIN}\nold\n{BLOCK_END}\noutro\n");
        let generated = format!("{BLOCK_BEGIN}\nnew\n{BLOCK_END}");
        let spliced = splice_generated_block(&existing, &generated);
        assert_eq!(spliced, format!("intro\n{generated}\noutro\n"));
    }

    #[test]
    fn splice_without_markers_replaces_document() {
        assert_eq!(splice_generated_block("plain", "GEN"), "GEN");
    }
}
