//! Port of `src/autofixers/fixers/i18n_scaffolder.py`.
//!
//! Wraps hardcoded JSX text and string props in `t("...")` calls and
//! scaffolds the `useTranslation` import plus hook.

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::fix::fixers::base::Fixer;
use crate::fix::pystr;
use crate::util::py_strip;
use crate::walk;

const FIXABLE_SUFFIXES: [&str; 3] = [".tsx", ".jsx", ".vue"];

const CODE_INDICATORS: [&str; 14] = [
    "${", "{{", "{", "}", "(", ")", "function", "const", "let", "var", "===", "!==", "&&", "||",
];

const IMPORT_STATEMENT: &str = "import { useTranslation } from 'react-i18next';\n";
const HOOK_STATEMENT: &str = "\n  const { t } = useTranslation();\n";

pub struct I18nScaffolder {
    repo_path: PathBuf,
    jsx_text: Regex,
    string_literal: Regex,
    i18n_patterns: Vec<Regex>,
    camel_start: Regex,
    non_word: Regex,
    whitespace_run: Regex,
    first_import: Regex,
    component_head: Regex,
    open_brace: Regex,
}

impl I18nScaffolder {
    pub fn new(repo_path: &Path) -> Self {
        I18nScaffolder {
            repo_path: repo_path.to_path_buf(),
            jsx_text: Regex::new(r">\s*([A-Z][^<>{}\n]{3,50})\s*<").unwrap(),
            string_literal: Regex::new(
                r#"(?:title|label|placeholder|text|message|description|alt)\s*[=:]\s*["']([^"']{3,})["']"#,
            )
            .unwrap(),
            i18n_patterns: [
                r"\bt\s*\(",
                r"\btranslate\s*\(",
                r"\bi18n\b",
                r"\$t\s*\(",
                r"\bformatMessage\s*\(",
            ]
            .iter()
            .map(|p| Regex::new(p).unwrap())
            .collect(),
            camel_start: Regex::new(r"^[a-z]+[A-Z]").unwrap(),
            // NOTE on \w: CPython's \w for str patterns follows
            // str.isalnum(), which additionally covers Nl and No (e.g.
            // roman numerals, vulgar fractions); the regex crate's \w
            // is Alphabetic + M + Nd + Pc + Join_Control. The
            // difference only shows up on those exotic categories.
            non_word: Regex::new(r"[^\w\s]").unwrap(),
            whitespace_run: Regex::new(r"\s+").unwrap(),
            first_import: Regex::new(r"(?m)^import\s").unwrap(),
            component_head: Regex::new(
                r"(export\s+(?:default\s+)?function\s+\w+|const\s+\w+\s*=\s*\([^)]*\)\s*=>)",
            )
            .unwrap(),
            open_brace: Regex::new(r"\{").unwrap(),
        }
    }

    fn has_i18n(&self, content: &str) -> bool {
        self.i18n_patterns.iter().any(|p| p.is_match(content))
    }

    /// Port of `_is_likely_code`.
    fn is_likely_code(&self, text: &str) -> bool {
        let len = pystr::char_len(text);
        if len < 3 || len > 100 {
            return true;
        }
        if CODE_INDICATORS
            .iter()
            .any(|indicator| text.contains(indicator))
        {
            return true;
        }
        if py_islower(text) && !text.contains(' ') {
            return true;
        }
        if self.camel_start.is_match(text) || text.contains('_') {
            return true;
        }
        false
    }

    /// Port of `_generate_i18n_key`.
    fn generate_i18n_key(&self, text: &str, file_path: &Path) -> String {
        let namespace = pystr::py_stem(file_path).to_lowercase();
        let lowered = text.to_lowercase();
        let key = self.non_word.replace_all(&lowered, "");
        let key = self.whitespace_run.replace_all(&key, "_").to_string();
        let key = if pystr::char_len(&key) > 40 {
            key.split('_').take(4).collect::<Vec<_>>().join("_")
        } else {
            key
        };
        format!("{namespace}.{key}")
    }

    /// Port of `_fix_jsx_strings`.
    fn fix_jsx_strings(&self, content: &str, file_path: &Path) -> (String, bool) {
        let mut new_content = content.to_string();
        let mut changes_made = false;
        let mut replacements: Vec<(String, String)> = Vec::new();

        for caps in self.jsx_text.captures_iter(content) {
            let raw = caps.get(1).unwrap().as_str();
            let text = py_strip(raw);
            if self.is_likely_code(text) {
                continue;
            }
            let key = self.generate_i18n_key(text, file_path);
            // The needle is built from the UNSTRIPPED capture, while
            // the regex consumed surrounding whitespace OUTSIDE the
            // group. When the source had leading space (`> Text <`),
            // the reconstructed `>Text <` is not a substring and the
            // replacement is silently dropped below.
            replacements.push((format!(">{raw}<"), format!(">{{t(\"{key}\")}}<")));
        }

        for caps in self.string_literal.captures_iter(content) {
            let raw = caps.get(1).unwrap().as_str();
            let text = py_strip(raw);
            if self.is_likely_code(text) {
                continue;
            }
            let key = self.generate_i18n_key(text, file_path);
            let old_literal = caps.get(0).unwrap().as_str();
            // `old_literal.split("=")[0].strip()` — for a colon-style
            // property (`label: "x"`) there is no `=`, so this is the
            // WHOLE literal and the rewrite produces
            // `label: "x"={t("...")}`. Preserved.
            let prop_name = py_strip(old_literal.split('=').next().unwrap_or(""));
            replacements.push((
                old_literal.to_string(),
                format!("{prop_name}={{t(\"{key}\")}}"),
            ));
        }

        for (old, new) in &replacements {
            if !new_content.contains(old.as_str()) {
                continue;
            }
            new_content = pystr::replace_first(&new_content, old, new);
            changes_made = true;
        }

        if changes_made && new_content.contains("import") {
            if let Some(m) = self.first_import.find(&new_content)
                && !new_content.contains(IMPORT_STATEMENT)
            {
                let start = m.start();
                new_content = format!(
                    "{}{}{}",
                    &new_content[..start],
                    IMPORT_STATEMENT,
                    &new_content[start..]
                );
            }

            if let Some(m) = self.component_head.find(&new_content) {
                let start = m.end();
                if let Some(brace) = self.open_brace.find(&new_content[start..]) {
                    let pos = start + brace.end();
                    if !new_content.contains(HOOK_STATEMENT) {
                        new_content = format!(
                            "{}{}{}",
                            &new_content[..pos],
                            HOOK_STATEMENT,
                            &new_content[pos..]
                        );
                    }
                }
            }
        }

        (new_content, changes_made)
    }

    /// Port of `find_hardcoded_strings` (reporting surface; the CLI
    /// does not use it). Entries are `(file, line, text, kind)`.
    pub fn find_hardcoded_strings(&self) -> Vec<(String, i64, String, &'static str)> {
        let mut hardcoded = Vec::new();
        for entry in walk::rglob_all(&self.repo_path) {
            if !self.can_fix(&entry.path) {
                continue;
            }
            let Some(content) = walk::read_file_safe(&entry.path) else {
                continue;
            };
            if self.has_i18n(&content) {
                continue;
            }
            let rel = entry
                .path
                .strip_prefix(&self.repo_path)
                .unwrap_or(&entry.path);
            let rel = pystr::as_posix(rel);
            for (regex, kind) in [(&self.jsx_text, "jsx_text"), (&self.string_literal, "prop")] {
                for caps in regex.captures_iter(&content) {
                    let text = py_strip(caps.get(1).unwrap().as_str());
                    if self.is_likely_code(text) {
                        continue;
                    }
                    let start = caps.get(0).unwrap().start();
                    let line = content[..start].matches('\n').count() as i64 + 1;
                    hardcoded.push((rel.clone(), line, text.to_string(), kind));
                }
            }
        }
        hardcoded
    }
}

impl Fixer for I18nScaffolder {
    fn fix_type(&self) -> &'static str {
        "i18n_scaffold"
    }

    fn can_fix(&self, file_path: &Path) -> bool {
        FIXABLE_SUFFIXES.contains(&walk::py_suffix(file_path).to_lowercase().as_str())
    }

    fn fix(&self, file_path: &Path, content: &str) -> Option<String> {
        if self.has_i18n(content) {
            return None;
        }
        let (new_content, changes_made) = self.fix_jsx_strings(content, file_path);
        if changes_made {
            Some(new_content)
        } else {
            None
        }
    }
}

/// CPython `str.islower()`: at least one cased character, and no cased
/// character is upper- or titlecase.
fn py_islower(text: &str) -> bool {
    let mut has_cased = false;
    for c in text.chars() {
        if c.is_uppercase() {
            return false;
        }
        if c.is_lowercase() {
            has_cased = true;
        }
    }
    has_cased
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::fixers::base::process_directory;
    use crate::testsupport::tmpdir;
    use std::fs;

    fn fixer() -> I18nScaffolder {
        I18nScaffolder::new(Path::new("/repo"))
    }

    #[test]
    fn can_fix_component_files() {
        let f = fixer();
        for name in ["Component.tsx", "Component.jsx", "Widget.vue", "A.TSX"] {
            assert!(f.can_fix(Path::new(name)), "{name}");
        }
        for name in ["README.md", "script.ts", "script.js"] {
            assert!(!f.can_fix(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn generates_i18n_keys_from_jsx_text() {
        let f = fixer();
        let result = f
            .fix(
                Path::new("UserGreeting.tsx"),
                "\nexport function UserGreeting() {\n  return <h1>Welcome Back</h1>;\n}\n",
            )
            .unwrap();
        assert!(
            result.contains(r#"t("usergreeting.welcome_back")"#),
            "{result}"
        );
    }

    #[test]
    fn rewrites_string_props() {
        let f = fixer();
        let result = f
            .fix(
                Path::new("Component.tsx"),
                "\nexport function Component() {\n  return <Widget label=\"Welcome to our app\" />;\n}\n",
            )
            .unwrap();
        assert!(
            result.contains(r#"label={t("component.welcome_to_our_app")}"#),
            "{result}"
        );
    }

    #[test]
    fn skips_files_that_already_use_i18n() {
        let f = fixer();
        assert_eq!(
            f.fix(
                Path::new("Component.tsx"),
                "\nimport { useTranslation } from 'react-i18next';\n\nexport function Component() {\n  const { t } = useTranslation();\n  return <h1>{t('greeting')}</h1>;\n}\n",
            ),
            None
        );
        for marker in ["translate(", "i18n ", "$t(", "formatMessage("] {
            let content = format!("const x = {marker};\nreturn <h1>Some Plain Text</h1>;");
            assert_eq!(f.fix(Path::new("C.tsx"), &content), None, "{marker}");
        }
    }

    #[test]
    fn skips_code_like_strings() {
        let f = fixer();
        assert_eq!(
            f.fix(
                Path::new("Component.tsx"),
                "\nexport function Component() {\n  return <Widget label=\"camelCaseVariable\" />;\n}\n",
            ),
            None
        );
        // Underscores, lowercase-single-word, parens/braces, too short,
        // too long — all rejected.
        for text in [
            "snake_case_here",
            "lowercase",
            "Has (parens) here",
            "Has {braces} here",
            "ab",
            &"L".repeat(101),
        ] {
            assert!(f.is_likely_code(text), "{text}");
        }
        for text in ["Welcome Back", "Dashboard Home", "Hello, World!"] {
            assert!(!f.is_likely_code(text), "{text}");
        }
    }

    #[test]
    fn leading_whitespace_before_jsx_text_drops_the_replacement() {
        // The needle is rebuilt from the unstripped capture, but the
        // regex consumed the leading space outside the group, so the
        // needle is not a substring and the fix is silently skipped.
        let f = fixer();
        assert_eq!(f.fix(Path::new("C.tsx"), "<h1> Welcome Back </h1>"), None);
        // Without the leading space it applies.
        assert!(f.fix(Path::new("C.tsx"), "<h1>Welcome Back</h1>").is_some());
    }

    #[test]
    fn scaffolds_the_import_and_hook_when_imports_exist() {
        let f = fixer();
        let result = f
            .fix(
                Path::new("Panel.tsx"),
                "import React from 'react';\nexport function Panel() {\n  return <h1>Save Your Changes</h1>;\n}\n",
            )
            .unwrap();
        assert!(result.starts_with(IMPORT_STATEMENT), "{result}");
        assert!(result.contains(HOOK_STATEMENT), "{result}");
    }

    #[test]
    fn without_an_import_no_scaffolding_is_added() {
        let f = fixer();
        let result = f
            .fix(
                Path::new("Bare.tsx"),
                "export const a = <h1>No Import Here</h1>;\n",
            )
            .unwrap();
        assert!(!result.contains("useTranslation"), "{result}");
        assert!(result.contains(r#"{t("bare.no_import_here")}"#), "{result}");
    }

    #[test]
    fn arrow_components_get_the_hook_too() {
        let f = fixer();
        let result = f
            .fix(
                Path::new("Arrow.tsx"),
                "import React from 'react';\nconst Arrow = (props) => {\n  return <h1>Arrow Component Text</h1>;\n};\n",
            )
            .unwrap();
        assert!(result.contains(HOOK_STATEMENT), "{result}");
    }

    #[test]
    fn long_keys_are_truncated_to_four_segments() {
        let f = fixer();
        let key = f.generate_i18n_key(
            "One Two Three Four Five Six Seven Eight Nine Ten",
            Path::new("Ns.tsx"),
        );
        assert_eq!(key, "ns.one_two_three_four");
    }

    #[test]
    fn punctuation_is_stripped_from_keys() {
        let f = fixer();
        assert_eq!(
            f.generate_i18n_key("Hello, World!", Path::new("Greet.tsx")),
            "greet.hello_world"
        );
    }

    #[test]
    fn colon_style_props_produce_the_python_quirk() {
        // `old_literal.split("=")[0]` is the whole literal when there
        // is no "=", so the rewrite keeps the original text and appends
        // the call.
        let f = fixer();
        let result = f
            .fix(
                Path::new("Cfg.tsx"),
                "const o = { label: \"Some Plain Words\" };\n",
            )
            .unwrap();
        assert!(
            result.contains(r#"label: "Some Plain Words"={t("cfg.some_plain_words")}"#),
            "{result}"
        );
    }

    #[test]
    fn duplicate_texts_each_consume_one_occurrence() {
        let f = fixer();
        let result = f
            .fix(
                Path::new("Dup.jsx"),
                "<div><h1>Same Text Here</h1><h2>Same Text Here</h2></div>",
            )
            .unwrap();
        assert_eq!(result.matches(r#"{t("dup.same_text_here")}"#).count(), 2);
    }

    #[test]
    fn find_hardcoded_strings_reports_text_and_line() {
        let tmp = tmpdir("i18n-find");
        fs::write(
            tmp.join("Greeting.tsx"),
            "\nexport function Greeting() {\n  return <h1>Welcome Back</h1>;\n}\n",
        )
        .unwrap();
        let f = I18nScaffolder::new(&tmp);
        let hardcoded = f.find_hardcoded_strings();
        assert!(
            hardcoded
                .iter()
                .any(|h| h.2 == "Welcome Back" && h.3 == "jsx_text")
        );
        assert_eq!(hardcoded[0].0, "Greeting.tsx");
        assert_eq!(hardcoded[0].1, 3);
    }

    #[test]
    fn process_directory_walks_and_proposes() {
        let tmp = tmpdir("i18n-walk");
        fs::write(
            tmp.join("Panel.tsx"),
            "import React from 'react';\nexport function Panel() { return <h1>Save Your Work</h1>; }\n",
        )
        .unwrap();
        fs::write(tmp.join("skip.ts"), "<h1>Not Scanned Text</h1>").unwrap();
        let f = I18nScaffolder::new(&tmp);
        let fixes = process_directory(&f, &tmp);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].fix_type, "i18n_scaffold");
    }

    #[test]
    fn islower_matches_cpython() {
        assert!(py_islower("lowercase"));
        assert!(!py_islower("Mixed"));
        assert!(!py_islower("123"));
        assert!(py_islower("abc123"));
        assert!(!py_islower(""));
    }
}
