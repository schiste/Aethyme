use std::fs;
use std::path::Path;

use crate::context_pack::{Anchor, AnchorKind, Snippet};

pub fn select_snippets(root: &Path, anchors: &[Anchor], limit: usize) -> Vec<Snippet> {
    let mut snippets = Vec::new();

    for anchor in anchors {
        let Some(file) = &anchor.file else {
            continue;
        };
        let absolute_path = root.join(file);
        let contents = fs::read_to_string(&absolute_path).unwrap_or_default();
        let line_count = contents.lines().count();
        if line_count == 0 {
            continue;
        }
        let kind = match anchor.kind {
            AnchorKind::Symbol => "definition",
            AnchorKind::File => "overview",
            AnchorKind::Folder => "overview",
        }
        .to_string();
        snippets.push(Snippet {
            file: file.clone(),
            start_line: 1,
            end_line: line_count.min(20),
            kind,
        });
        if snippets.len() >= limit {
            break;
        }
    }

    snippets.sort();
    snippets
}
