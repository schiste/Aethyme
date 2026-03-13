use std::fs;
use std::path::Path;

use crate::context_pack::{Anchor, AnchorKind, Budget, Snippet};
use crate::map::RepositoryMap;

pub fn select_snippets(root: &Path, map: &RepositoryMap, anchors: &[Anchor], budget: &Budget) -> Vec<Snippet> {
    let mut snippets = Vec::new();
    let window = budget.snippet_window;

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
        let (start_line, end_line, kind) = match anchor.kind {
            AnchorKind::Symbol => symbol_window(map, &anchor.id, file, line_count, window),
            AnchorKind::File => file_header_window(map, file, line_count, window),
            AnchorKind::Folder => (1, line_count.min(window), "overview".to_string()),
        };
        snippets.push(Snippet {
            file: file.clone(),
            start_line,
            end_line,
            kind,
        });
        if snippets.len() >= budget.max_snippets {
            break;
        }
    }

    snippets.sort();
    snippets
}

fn symbol_window(
    map: &RepositoryMap,
    symbol_id: &str,
    file_path: &str,
    line_count: usize,
    window: usize,
) -> (usize, usize, String) {
    let def_line = map
        .functions
        .iter()
        .find(|f| f.id == symbol_id)
        .map(|f| f.line)
        .or_else(|| map.classes.iter().find(|c| c.id == symbol_id).map(|c| c.line));

    let start = match def_line {
        Some(line) if line >= 1 => line,
        _ => 1,
    };
    let end = find_symbol_end(map, file_path, start, line_count, window);
    (start, end, "definition".to_string())
}

fn file_header_window(
    map: &RepositoryMap,
    file_path: &str,
    line_count: usize,
    window: usize,
) -> (usize, usize, String) {
    let first_def_line = definition_lines_in_file(map, file_path).into_iter().min();

    let end = match first_def_line {
        Some(line) if line > 1 => (line + 3).min(line_count).min(window),
        _ => line_count.min(window),
    };
    (1, end, "overview".to_string())
}

fn find_symbol_end(
    map: &RepositoryMap,
    file_path: &str,
    start_line: usize,
    line_count: usize,
    window: usize,
) -> usize {
    let next_def = definition_lines_in_file(map, file_path)
        .into_iter()
        .filter(|&line| line > start_line)
        .min();

    let window_end = start_line + window - 1;
    let mut end = window_end.min(line_count);
    if let Some(next) = next_def {
        end = end.min(next.saturating_sub(1));
    }
    end.max(start_line)
}

fn definition_lines_in_file(map: &RepositoryMap, file_path: &str) -> Vec<usize> {
    let mut lines: Vec<usize> = Vec::new();
    for f in &map.functions {
        if f.file_path == file_path {
            lines.push(f.line);
        }
    }
    for c in &map.classes {
        if c.file_path == file_path {
            lines.push(c.line);
        }
    }
    lines
}
