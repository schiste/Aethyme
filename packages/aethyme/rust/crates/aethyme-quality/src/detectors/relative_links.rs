//! Detector for absolute paths that should be relative (port of
//! `src/scorecard/detectors/relative_links.py`).

use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::model::{Finding, Severity};
use crate::util::{py_slice, py_strip};
use crate::walk::{py_suffix, read_file_safe, rglob_all, should_skip_file};

pub struct RelativeLinksDetector;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetectionMode {
    Legacy,
    QualityInspection,
}

const EXTENSIONS: [&str; 9] = [
    ".md", ".py", ".ts", ".tsx", ".jsx", ".js", ".json", ".yaml", ".yml",
];

fn patterns() -> &'static Vec<(Regex, &'static str)> {
    static CELL: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    CELL.get_or_init(|| {
        vec![
            (Regex::new(r"/home/[\w/.-]+").unwrap(), "Linux home path"),
            (Regex::new(r"/Users/[\w/.-]+").unwrap(), "macOS home path"),
            (Regex::new(r"C:\\[\w\\.-]+").unwrap(), "Windows path"),
            (Regex::new(r"/var/[\w/.-]+").unwrap(), "System path"),
            (Regex::new(r"/tmp/[\w/.-]+").unwrap(), "Temp path"),
        ]
    })
}

fn markdown_link_pattern() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r#"!?\[[^\]\r\n]*\]\(\s*<?([^\s)>]+)"#).unwrap())
}

fn markdown_autolink_pattern() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"<((?:file://)?[^\s>]+)>").unwrap())
}

fn markdown_reference_pattern() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r#"^\s{0,3}\[[^\]\r\n]+\]:\s*<?([^\s>]+)"#).unwrap())
}

fn markdown_html_link_pattern() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r#"(?i)\b(?:href|src)\s*=\s*["']([^"']+)["']"#).unwrap())
}

fn local_destination(destination: &str) -> bool {
    destination.starts_with('/')
        || destination.starts_with(r"C:\")
        || destination.starts_with("file:///")
}

fn markdown_destination_ranges(line: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    for pattern in [
        markdown_link_pattern(),
        markdown_autolink_pattern(),
        markdown_reference_pattern(),
        markdown_html_link_pattern(),
    ] {
        ranges.extend(
            pattern
                .captures_iter(line)
                .filter_map(|captures| captures.get(1))
                .filter(|destination| local_destination(destination.as_str()))
                .map(|destination| destination.start()..destination.end()),
        );
    }
    ranges
}

fn inline_code_ranges(line: &str) -> Vec<Range<usize>> {
    let bytes = line.as_bytes();
    let mut ranges = Vec::new();
    let mut offset = 0;

    while offset < bytes.len() {
        if bytes[offset] != b'`' {
            offset += 1;
            continue;
        }
        let start = offset;
        while offset < bytes.len() && bytes[offset] == b'`' {
            offset += 1;
        }
        let delimiter_len = offset - start;
        let mut cursor = offset;
        let mut end = None;
        while cursor < bytes.len() {
            if bytes[cursor] != b'`' {
                cursor += 1;
                continue;
            }
            let delimiter_start = cursor;
            while cursor < bytes.len() && bytes[cursor] == b'`' {
                cursor += 1;
            }
            if cursor - delimiter_start == delimiter_len {
                end = Some(cursor);
                break;
            }
        }
        if let Some(end) = end {
            ranges.push(start..end);
            offset = end;
        } else {
            break;
        }
    }

    ranges
}

fn fence_delimiter(line: &str) -> Option<(u8, usize)> {
    let trimmed = line.trim_start();
    let delimiter = *trimmed.as_bytes().first()?;
    if delimiter != b'`' && delimiter != b'~' {
        return None;
    }
    let len = trimmed
        .as_bytes()
        .iter()
        .take_while(|byte| **byte == delimiter)
        .count();
    (len >= 3).then_some((delimiter, len))
}

fn markdown_line_is_example(line: &str, fence: &mut Option<(u8, usize)>) -> bool {
    if let Some((active_delimiter, active_len)) = *fence {
        if fence_delimiter(line)
            .is_some_and(|(delimiter, len)| delimiter == active_delimiter && len >= active_len)
        {
            *fence = None;
        }
        return true;
    }

    if let Some(delimiter) = fence_delimiter(line) {
        *fence = Some(delimiter);
        return true;
    }

    line.starts_with("    ") || line.starts_with('\t')
}

impl RelativeLinksDetector {
    fn detect_with_mode(&self, repo_path: &Path, mode: DetectionMode) -> Vec<Finding> {
        let mut findings = Vec::new();

        for entry in rglob_all(repo_path) {
            let suffix = py_suffix(&entry.path);
            if !entry.is_file || !EXTENSIONS.contains(&suffix.as_str()) {
                continue;
            }
            if should_skip_file(&entry.path) {
                continue;
            }
            let Some(content) = read_file_safe(&entry.path) else {
                continue;
            };
            if content.is_empty() {
                continue;
            }

            let rel = entry
                .path
                .strip_prefix(repo_path)
                .unwrap_or(&entry.path)
                .to_string_lossy()
                .to_string();
            let inspect_markdown =
                mode == DetectionMode::QualityInspection && suffix.as_str() == ".md";
            let mut fence = None;

            for (line_num, line) in content.split('\n').enumerate() {
                let line_num = line_num + 1;
                if inspect_markdown && markdown_line_is_example(line, &mut fence) {
                    continue;
                }
                let destination_ranges = inspect_markdown
                    .then(|| markdown_destination_ranges(line))
                    .unwrap_or_default();
                let code_ranges = inspect_markdown
                    .then(|| inline_code_ranges(line))
                    .unwrap_or_default();

                for (pattern, path_type) in patterns() {
                    for matched_path in pattern.find_iter(line) {
                        if inspect_markdown
                            && (!destination_ranges.iter().any(|range| {
                                range.start <= matched_path.start()
                                    && matched_path.end() <= range.end
                            }) || code_ranges.iter().any(|range| {
                                range.start <= matched_path.start()
                                    && matched_path.end() <= range.end
                            }))
                        {
                            continue;
                        }
                        let absolute_path = matched_path.as_str();
                        // Python skipped matches referenced after a `#` or
                        // `//` anywhere earlier in the line (dynamic
                        // `#.*<escaped>` / `//.*<escaped>` searches).
                        let escaped = regex::escape(absolute_path);
                        let hash_re = Regex::new(&format!("#.*{escaped}")).unwrap();
                        let slash_re = Regex::new(&format!("//.*{escaped}")).unwrap();
                        if !inspect_markdown && (hash_re.is_match(line) || slash_re.is_match(line))
                        {
                            continue;
                        }
                        findings.push(Finding {
                            detector: "relative-links".to_string(),
                            severity: Severity::Warning,
                            message: format!("Absolute {path_type} should use relative path"),
                            file_path: rel.clone(),
                            line_number: Some(line_num as i64),
                            evidence: Some(py_slice(py_strip(line), 150)),
                            suggestion: Some(
                                "Use relative paths for better portability across environments"
                                    .to_string(),
                            ),
                        });
                    }
                }
            }
        }

        findings
    }
}

impl super::Detector for RelativeLinksDetector {
    fn name(&self) -> &'static str {
        "relative-links"
    }

    fn description(&self) -> &'static str {
        "Checks for absolute file paths that should use relative links"
    }

    fn applicability(&self, repo_path: &Path) -> super::DetectorApplicability {
        super::applies_to_extensions(repo_path, &EXTENSIONS, "portable text or source files")
    }

    fn detect(&self, repo_path: &Path) -> Vec<Finding> {
        self.detect_with_mode(repo_path, DetectionMode::Legacy)
    }

    fn inspect(&self, repo_path: &Path) -> Vec<Finding> {
        self.detect_with_mode(repo_path, DetectionMode::QualityInspection)
    }
}

#[cfg(test)]
mod tests {
    use super::super::Detector;
    use super::*;
    use crate::testsupport::{build_good_scorecard_repo, build_problematic_scorecard_repo, tmpdir};

    // Translated from tests/scorecard/test_detectors.py::TestRelativeLinksDetector.

    #[test]
    fn detects_absolute_paths() {
        let tmp = tmpdir("rellinks-bad");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = RelativeLinksDetector.detect(&repo);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.severity == Severity::Warning));
        assert!(
            findings
                .iter()
                .any(|f| f.message.to_lowercase().contains("absolute"))
        );
    }

    #[test]
    fn relative_links_ok() {
        let tmp = tmpdir("rellinks-good");
        let repo = build_good_scorecard_repo(&tmp);
        assert!(RelativeLinksDetector.detect(&repo).is_empty());
    }

    #[test]
    fn exact_findings_and_comment_skip() {
        let tmp = tmpdir("rellinks-shape");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = RelativeLinksDetector.detect(&repo);
        // README.md line 3 (/Users/...) and line 4 (C:\...).
        assert_eq!(findings.len(), 2);
        assert_eq!(
            findings[0].message,
            "Absolute macOS home path should use relative path"
        );
        assert_eq!(findings[0].line_number, Some(3));
        assert_eq!(
            findings[1].message,
            "Absolute Windows path should use relative path"
        );
        assert_eq!(findings[1].line_number, Some(4));

        // Comment-referenced paths are skipped.
        let repo2 = tmp.join("c");
        std::fs::create_dir_all(&repo2).unwrap();
        std::fs::write(
            repo2.join("code.py"),
            "x = 1  # see /home/user/example\nurl = \"https://x.io/var/data\"\n",
        )
        .unwrap();
        assert!(RelativeLinksDetector.detect(&repo2).is_empty());
    }

    #[test]
    fn inspection_ignores_markdown_examples_and_flags_real_link_destinations() {
        let tmp = tmpdir("rellinks-inspection-markdown");
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("README.md"),
            concat!(
                "Example checkout: /Users/example/repo\n",
                "Run `cd /tmp/example` to follow the walkthrough.\n",
                "`[example](/Users/example/not-a-link.md)`\n",
                "```json\n",
                "{\"root\": \"/home/example/repo\"}\n",
                "```\n",
                "    CACHE=/var/tmp/example\n",
                "[local notes](/Users/alice/notes.md)\n",
                "<file:///home/alice/reference.md>\n",
                "[reference]: /var/lib/alice/reference.md\n",
                "<a href=\"C:\\Users\\alice\\notes.md\">notes</a>\n",
                "[remote](https://example.test/Users/alice/reference.md)\n",
            ),
        )
        .unwrap();

        let legacy = RelativeLinksDetector.detect(&repo);
        assert_eq!(legacy.len(), 9);

        let findings = RelativeLinksDetector.inspect(&repo);
        assert_eq!(findings.len(), 4);
        assert_eq!(findings[0].line_number, Some(8));
        assert_eq!(findings[1].line_number, Some(9));
        assert_eq!(findings[2].line_number, Some(10));
        assert_eq!(findings[3].line_number, Some(11));
    }

    #[test]
    fn inspection_keeps_actionable_source_and_configuration_paths() {
        let tmp = tmpdir("rellinks-inspection-source");
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("settings.py"),
            "CACHE_ROOT = \"/var/lib/example/cache\"\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("config.json"),
            "{\"root\": \"/Users/alice/app\"}\n",
        )
        .unwrap();

        let findings = RelativeLinksDetector.inspect(&repo);
        assert_eq!(findings.len(), 2);
        // Detectors emit in filesystem-walk order; `rendered_findings` in
        // quality_cli is the one place that promises an order. Asserting the
        // walk order here read as alphabetical only because APFS returns
        // sorted `readdir` entries -- ext4 does not, so this passed on every
        // developer machine and failed on every Linux CI run.
        let mut paths: Vec<&str> = findings.iter().map(|f| f.file_path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, ["config.json", "settings.py"]);
    }
}
