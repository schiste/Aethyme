//! Port of `src/autofixers/fixers/selector_inserter.py`.
//!
//! Adds `data-ui` attributes to interactive JSX/Vue elements.

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::fix::fixers::base::Fixer;
use crate::fix::pystr;
use crate::walk;

const INTERACTIVE_ELEMENTS: [&str; 10] = [
    "button", "a", "input", "select", "textarea", "Button", "Link", "Input", "Select", "TextArea",
];

const FIXABLE_SUFFIXES: [&str; 5] = [".tsx", ".jsx", ".vue", ".ts", ".js"];

/// The attribute patterns consulted, in order, for the third segment of
/// the generated selector name. All require DOUBLE quotes, so
/// single-quoted attributes never contribute.
const ATTR_PATTERNS: [&str; 5] = [
    r#"type="([^"]+)""#,
    r#"name="([^"]+)""#,
    r#"id="([^"]+)""#,
    r#"className="([^"]+)""#,
    r#"class="([^"]+)""#,
];

pub struct SelectorInserter {
    repo_path: PathBuf,
    jsx_element: Regex,
    vue_template: Regex,
    attr_patterns: Vec<Regex>,
}

impl SelectorInserter {
    pub fn new(repo_path: &Path) -> Self {
        SelectorInserter {
            repo_path: repo_path.to_path_buf(),
            jsx_element: Regex::new(r"(?m)<([\w.]+)(\s+[^>]*?)?(/?>)").unwrap(),
            vue_template: Regex::new(r"(?s)<template>(.*?)</template>").unwrap(),
            attr_patterns: ATTR_PATTERNS
                .iter()
                .map(|p| Regex::new(p).unwrap())
                .collect(),
        }
    }

    /// Port of `_fix_jsx`. Returns `None` where the Python raises
    /// (see `generate_selector_name`), which `process_file` swallows
    /// into "no fix for this file".
    fn fix_jsx(&self, content: &str, file_path: &Path) -> Option<(String, bool)> {
        let mut new_content = content.to_string();
        let mut changes_made = false;
        for caps in self.jsx_element.captures_iter(content) {
            let element_name = caps.get(1).unwrap().as_str();
            let attributes = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let closing = caps.get(3).unwrap().as_str();
            let base_element = element_name.rsplit('.').next().unwrap_or(element_name);
            if !INTERACTIVE_ELEMENTS.contains(&base_element) {
                continue;
            }
            if attributes.contains("data-ui=") || attributes.contains("data-testid=") {
                continue;
            }

            let selector_name = self.generate_selector_name(element_name, attributes, file_path)?;
            let prefix = if attributes.is_empty() {
                " ".to_string()
            } else {
                format!("{} ", attributes.trim_end_matches(char::is_whitespace))
            };
            let old_element = caps.get(0).unwrap().as_str();
            let new_element =
                format!("<{element_name}{prefix}data-ui=\"{selector_name}\"{closing}");
            // Replace only the FIRST remaining occurrence, so repeated
            // identical elements are each rewritten in turn.
            new_content = pystr::replace_first(&new_content, old_element, &new_element);
            changes_made = true;
        }
        Some((new_content, changes_made))
    }

    /// Port of `_fix_vue`: only the first `<template>` block is
    /// considered, and the rewritten block replaces EVERY occurrence of
    /// the original block text.
    fn fix_vue(&self, content: &str, file_path: &Path) -> Option<(String, bool)> {
        let Some(caps) = self.vue_template.captures(content) else {
            return Some((content.to_string(), false));
        };
        let template_content = caps.get(1).unwrap().as_str();
        let (new_template, changed) = self.fix_jsx(template_content, file_path)?;
        if changed {
            Some((
                pystr::replace_all(content, template_content, &new_template),
                true,
            ))
        } else {
            Some((content.to_string(), false))
        }
    }

    /// Port of `_generate_selector_name`.
    ///
    /// Returns `None` where the Python raises `IndexError`: an
    /// attribute whose captured value is whitespace-only makes
    /// `value.split()[0]` index an empty list. That propagates out of
    /// `fix` and `process_file` skips the file entirely.
    fn generate_selector_name(
        &self,
        element_name: &str,
        attributes: &str,
        file_path: &Path,
    ) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        let component_name = pystr::py_stem(file_path);
        if !component_name.is_empty() && component_name != "index" {
            parts.push(kebab_from_camel(&component_name));
        }

        let base_element = element_name
            .rsplit('.')
            .next()
            .unwrap_or(element_name)
            .to_lowercase();
        parts.push(base_element);

        for pattern in &self.attr_patterns {
            let Some(caps) = pattern.captures(attributes) else {
                continue;
            };
            let raw = caps.get(1).unwrap().as_str();
            // `value.split()[0]` — IndexError on whitespace-only.
            let first = raw.split_whitespace().next()?;
            let value = first.replace("btn-", "").replace("button-", "");
            if !value.is_empty() && !parts.contains(&value) {
                parts.push(value);
            }
            break;
        }

        if parts.len() == 1 {
            parts.push("element".to_string());
        }
        Some(parts[..parts.len().min(3)].join("-"))
    }

    /// Port of `find_missing_selectors` (the reporting surface; the CLI
    /// does not use it).
    pub fn find_missing_selectors(&self) -> Vec<(String, i64, String)> {
        let mut missing = Vec::new();
        for entry in walk::rglob_all(&self.repo_path) {
            if !self.can_fix(&entry.path) {
                continue;
            }
            let Some(content) = walk::read_file_safe(&entry.path) else {
                continue;
            };
            for caps in self.jsx_element.captures_iter(&content) {
                let element_name = caps.get(1).unwrap().as_str();
                let attributes = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                let base_element = element_name.rsplit('.').next().unwrap_or(element_name);
                if !INTERACTIVE_ELEMENTS.contains(&base_element) {
                    continue;
                }
                if attributes.contains("data-ui=") || attributes.contains("data-testid=") {
                    continue;
                }
                let start = caps.get(0).unwrap().start();
                let line = content[..start].matches('\n').count() as i64 + 1;
                let rel = entry
                    .path
                    .strip_prefix(&self.repo_path)
                    .unwrap_or(&entry.path);
                missing.push((pystr::as_posix(rel), line, element_name.to_string()));
            }
        }
        missing
    }
}

impl Fixer for SelectorInserter {
    fn fix_type(&self) -> &'static str {
        "selector_insert"
    }

    fn can_fix(&self, file_path: &Path) -> bool {
        FIXABLE_SUFFIXES.contains(&walk::py_suffix(file_path).to_lowercase().as_str())
    }

    /// Port of `fix`. Note the asymmetry the Python has and this
    /// preserves: `can_fix` lowercases the suffix, but the dispatch
    /// here compares the suffix VERBATIM, so a `.TSX` file passes the
    /// gate and is then silently not handled. `.ts` and `.js` are
    /// accepted by `can_fix` and never dispatched at all.
    fn fix(&self, file_path: &Path, content: &str) -> Option<String> {
        if !INTERACTIVE_ELEMENTS.iter().any(|e| content.contains(e)) {
            return None;
        }
        let suffix = walk::py_suffix(file_path);
        if suffix == ".tsx" || suffix == ".jsx" {
            let (new_content, changed) = self.fix_jsx(content, file_path)?;
            return if changed { Some(new_content) } else { None };
        }
        if suffix == ".vue" {
            let (new_content, changed) = self.fix_vue(content, file_path)?;
            return if changed { Some(new_content) } else { None };
        }
        None
    }
}

/// `re.sub(r"(?<!^)(?=[A-Z])", "-", name).lower()` — a zero-width
/// insertion before every ASCII uppercase letter except at position
/// zero. The `regex` crate has no lookaround, so this is done directly.
fn kebab_from_camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len() * 2);
    for (index, c) in name.char_indices() {
        if index > 0 && c.is_ascii_uppercase() {
            out.push('-');
        }
        out.push(c);
    }
    out.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::fixers::base::process_directory;
    use crate::testsupport::tmpdir;
    use std::fs;

    fn fixer() -> SelectorInserter {
        SelectorInserter::new(Path::new("/repo"))
    }

    #[test]
    fn can_fix_component_files() {
        let f = fixer();
        for name in ["Button.tsx", "Form.jsx", "Component.vue", "a.ts", "a.js", "A.TSX"] {
            assert!(f.can_fix(Path::new(name)), "{name}");
        }
        for name in ["utils.py", "README.md", "a"] {
            assert!(!f.can_fix(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn adds_data_ui_to_buttons() {
        let f = fixer();
        let result = f
            .fix(
                Path::new("Button.tsx"),
                "export function MyButton() {\n  return <button onClick={handleClick}>Click</button>;\n}\n",
            )
            .unwrap();
        assert!(result.contains("data-ui="), "{result}");
    }

    #[test]
    fn generates_meaningful_selector_names() {
        let f = fixer();
        let result = f
            .fix(
                Path::new("LoginForm.tsx"),
                "\nexport function LoginForm() {\n  return <button type=\"submit\">Continue</button>;\n}\n",
            )
            .unwrap();
        assert!(result.contains(r#"data-ui="login-form-button-submit""#), "{result}");
    }

    #[test]
    fn skips_elements_that_already_have_selectors() {
        let f = fixer();
        assert_eq!(
            f.fix(Path::new("Button.tsx"), r#"<button data-ui="my-button">Click</button>"#),
            None
        );
        assert_eq!(
            f.fix(Path::new("Button.tsx"), r#"<button data-testid="my-button">Click</button>"#),
            None
        );
    }

    #[test]
    fn skips_non_interactive_elements() {
        let f = fixer();
        assert_eq!(f.fix(Path::new("Widget.tsx"), "<div>Plain</div>"), None);
    }

    #[test]
    fn namespaced_components_use_the_last_segment() {
        let f = fixer();
        let result = f
            .fix(Path::new("Panel.tsx"), "<Foo.Button>Go</Foo.Button>")
            .unwrap();
        assert!(result.contains(r#"<Foo.Button data-ui="panel-button">"#), "{result}");
    }

    #[test]
    fn index_files_drop_the_component_segment() {
        let f = fixer();
        let result = f.fix(Path::new("index.tsx"), "<button>Idx</button>").unwrap();
        // parts == ["button"] -> the "element" filler is appended.
        assert!(result.contains(r#"data-ui="button-element""#), "{result}");
    }

    #[test]
    fn self_closing_elements_keep_their_slash() {
        let f = fixer();
        let result = f
            .fix(Path::new("Form.tsx"), r#"<input name="email" />"#)
            .unwrap();
        assert_eq!(result, r#"<input name="email" data-ui="form-input-email"/>"#);
    }

    #[test]
    fn single_quoted_attributes_do_not_contribute_a_segment() {
        // Every attribute pattern requires double quotes.
        let f = fixer();
        let result = f
            .fix(Path::new("Form.tsx"), "<input type='text' />")
            .unwrap();
        assert_eq!(result, r#"<input type='text' data-ui="form-input"/>"#);
    }

    #[test]
    fn class_values_drop_btn_prefixes_and_take_the_first_token() {
        let f = fixer();
        let result = f
            .fix(Path::new("Bar.tsx"), r#"<button className="btn-primary large">Go</button>"#)
            .unwrap();
        assert!(result.contains(r#"data-ui="bar-button-primary""#), "{result}");
    }

    #[test]
    fn selector_name_is_capped_at_three_segments() {
        let f = fixer();
        let result = f
            .fix(Path::new("VeryLongComponentName.tsx"), r#"<button id="save">Go</button>"#)
            .unwrap();
        assert!(
            result.contains(r#"data-ui="very-long-component-name-button-save""#),
            "{result}"
        );
    }

    #[test]
    fn whitespace_only_attribute_value_skips_the_whole_file() {
        // Python raises IndexError from value.split()[0]; process_file
        // swallows it and the file is skipped.
        let f = fixer();
        assert_eq!(f.fix(Path::new("Bad.tsx"), "<button class=\" \">Go</button>"), None);
    }

    #[test]
    fn repeated_identical_elements_are_each_rewritten() {
        let f = fixer();
        let result = f
            .fix(Path::new("Dup.jsx"), "<button>Same</button>\n<button>Same</button>\n")
            .unwrap();
        assert_eq!(
            result,
            "<button data-ui=\"dup-button\">Same</button>\n<button data-ui=\"dup-button\">Same</button>\n"
        );
    }

    #[test]
    fn vue_files_are_fixed_inside_the_template_block_only() {
        let f = fixer();
        let result = f
            .fix(
                Path::new("Widget.vue"),
                "<template>\n  <button>Go</button>\n</template>\n<script>const a = <button>x</button></script>\n",
            )
            .unwrap();
        assert_eq!(
            result,
            "<template>\n  <button data-ui=\"widget-button\">Go</button>\n</template>\n<script>const a = <button>x</button></script>\n"
        );
    }

    #[test]
    fn vue_without_a_template_block_is_untouched() {
        let f = fixer();
        assert_eq!(
            f.fix(Path::new("NoTemplate.vue"), "<script>const a = <button>x</button></script>"),
            None
        );
    }

    #[test]
    fn ts_and_js_pass_can_fix_but_are_never_dispatched() {
        let f = fixer();
        assert!(f.can_fix(Path::new("a.ts")));
        assert_eq!(f.fix(Path::new("a.ts"), "<button>x</button>"), None);
        assert_eq!(f.fix(Path::new("a.js"), "<button>x</button>"), None);
        // Uppercase suffix: can_fix lowercases, fix does not.
        assert_eq!(f.fix(Path::new("A.TSX"), "<button>x</button>"), None);
    }

    #[test]
    fn find_missing_selectors_reports_file_line_and_element() {
        let tmp = tmpdir("selectors-find");
        fs::write(
            tmp.join("Component.tsx"),
            "\nexport function Component() {\n  return <button>Click</button>;\n}\n",
        )
        .unwrap();
        let f = SelectorInserter::new(&tmp);
        let missing = f.find_missing_selectors();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0, "Component.tsx");
        assert_eq!(missing[0].1, 3);
        assert_eq!(missing[0].2, "button");
    }

    #[test]
    fn process_directory_walks_and_proposes() {
        let tmp = tmpdir("selectors-walk");
        fs::write(tmp.join("Panel.tsx"), "<button>Go</button>").unwrap();
        fs::write(tmp.join("plain.py"), "<button>Go</button>").unwrap();
        let f = SelectorInserter::new(&tmp);
        let fixes = process_directory(&f, &tmp);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].fix_type, "selector_insert");
        assert_eq!(pystr::file_name(&fixes[0].file_path), "Panel.tsx");
    }

    #[test]
    fn kebab_conversion_matches_the_python_regex() {
        assert_eq!(kebab_from_camel("LoginForm"), "login-form");
        assert_eq!(kebab_from_camel("ABC"), "a-b-c");
        assert_eq!(kebab_from_camel("lower"), "lower");
        assert_eq!(kebab_from_camel("aB"), "a-b");
        assert_eq!(kebab_from_camel(""), "");
    }
}
