//! Port of `src/autofixers/fixers/link_fixer.py`.
//!
//! Converts internal ABSOLUTE documentation links into repo-relative
//! ones. Only links whose target actually exists under the repo root
//! are rewritten.

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::fix::fixers::base::Fixer;
use crate::fix::pystr;
use crate::walk;

const INTERNAL_DOMAINS: [&str; 3] = ["localhost", "127.0.0.1", "github.com"];
const TEXT_SUFFIXES: [&str; 4] = [".md", ".mdx", ".rst", ".txt"];

pub struct LinkFixer {
    repo_path: PathBuf,
    markdown_link: Regex,
}

impl LinkFixer {
    pub fn new(repo_path: &Path) -> Self {
        LinkFixer {
            repo_path: repo_path.to_path_buf(),
            markdown_link: Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").unwrap(),
        }
    }

    /// Port of `_convert_to_relative`.
    fn convert_to_relative(&self, current_file: &Path, link_url: &str) -> Option<String> {
        let mut url = link_url.to_string();
        for protocol in ["http://", "https://", "file://"] {
            if let Some(rest) = url.strip_prefix(protocol) {
                url = rest.to_string();
            }
        }

        for domain in INTERNAL_DOMAINS {
            if url.starts_with(domain) {
                // `url.split("/", 1)`: drop the authority, keep the
                // path. A bare domain with no path is left as-is and
                // then rejected by the leading-slash check below.
                if let Some((_, rest)) = url.split_once('/') {
                    url = format!("/{rest}");
                }
                break;
            }
        }

        if !url.starts_with('/') {
            return None;
        }

        // `repo_path / url.lstrip("/")` — ALL leading slashes stripped.
        let target_path = self.repo_path.join(url.trim_start_matches('/'));
        if !target_path.exists() {
            return None;
        }
        let parent = current_file.parent().unwrap_or(Path::new(""));
        Some(pystr::as_posix(&pystr::relpath(&target_path, parent)))
    }
}

impl Fixer for LinkFixer {
    fn fix_type(&self) -> &'static str {
        "link_fix"
    }

    fn can_fix(&self, file_path: &Path) -> bool {
        TEXT_SUFFIXES.contains(&walk::py_suffix(file_path).to_lowercase().as_str())
    }

    /// Port of `fix`.
    ///
    /// The skip condition relies on Python's `and`-binds-tighter-than-`or`
    /// precedence: anchors, `./` and `../` are always skipped, while an
    /// `http(s)://` link is skipped only when it does NOT mention an
    /// internal domain. A `https://github.com/...` link therefore falls
    /// THROUGH to conversion.
    ///
    /// Replacement is `str.replace` with no count, so every occurrence
    /// of an identical `[text](url)` pair is rewritten on the first
    /// match; later matches of the same pair become no-ops that still
    /// set `changes_made`.
    fn fix(&self, file_path: &Path, content: &str) -> Option<String> {
        if !content.contains('[') || !content.contains('(') {
            return None;
        }

        let mut new_content = content.to_string();
        let mut changes_made = false;
        for caps in self.markdown_link.captures_iter(content) {
            let link_text = caps.get(1).unwrap().as_str();
            let link_url = caps.get(2).unwrap().as_str();
            let mentions_internal = INTERNAL_DOMAINS
                .iter()
                .any(|domain| link_url.contains(domain));
            if link_url.starts_with('#')
                || link_url.starts_with("./")
                || link_url.starts_with("../")
                || (link_url.starts_with("http://") && !mentions_internal)
                || (link_url.starts_with("https://") && !mentions_internal)
            {
                continue;
            }

            let Some(new_url) = self.convert_to_relative(file_path, link_url) else {
                continue;
            };
            if new_url.is_empty() || new_url == link_url {
                continue;
            }

            let old_link = format!("[{link_text}]({link_url})");
            let new_link = format!("[{link_text}]({new_url})");
            new_content = pystr::replace_all(&new_content, &old_link, &new_link);
            changes_made = true;
        }

        if changes_made { Some(new_content) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::fixers::base::process_directory;
    use crate::testsupport::tmpdir;
    use std::fs;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn repo() -> PathBuf {
        let tmp = tmpdir("links");
        write(&tmp, "docs/guide.md", "guide\n");
        write(&tmp, "docs/api.md", "api\n");
        write(&tmp, "assets/logo.png", "png\n");
        tmp
    }

    #[test]
    fn can_fix_text_documents_only() {
        let fixer = LinkFixer::new(Path::new("/repo"));
        for name in ["README.md", "docs/guide.md", "a.mdx", "a.rst", "a.txt", "A.MD"] {
            assert!(fixer.can_fix(Path::new(name)), "{name}");
        }
        for name in ["script.py", "a.markdown", "a", "a.md.bak"] {
            assert!(!fixer.can_fix(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn converts_absolute_links_to_relative() {
        let tmp = repo();
        let fixer = LinkFixer::new(&tmp);
        let file = tmp.join("README.md");
        let fixed = fixer
            .fix(&file, "[Link to file](/docs/guide.md)\n")
            .unwrap();
        assert_eq!(fixed, "[Link to file](docs/guide.md)\n");
    }

    #[test]
    fn converts_internal_domain_links_including_github() {
        let tmp = repo();
        let fixer = LinkFixer::new(&tmp);
        let file = tmp.join("README.md");
        let fixed = fixer
            .fix(
                &file,
                "[A](http://localhost:3000/docs/api.md)\n\
                 [B](https://127.0.0.1:8080/assets/logo.png)\n\
                 [C](https://github.com/docs/guide.md)\n",
            )
            .unwrap();
        assert_eq!(
            fixed,
            "[A](docs/api.md)\n[B](assets/logo.png)\n[C](docs/guide.md)\n"
        );
    }

    #[test]
    fn preserves_external_relative_and_anchor_links() {
        let tmp = repo();
        let fixer = LinkFixer::new(&tmp);
        let file = tmp.join("README.md");
        for content in [
            "[External](https://example.com/x.md)",
            "[Relative](./docs/guide.md)",
            "[Parent](../up.md)",
            "[Anchor](#section)",
            "[Bare](docs/api.md)",
            "[Missing](/nope/gone.md)",
            "[Empty]()",
            "no links at all",
        ] {
            assert_eq!(fixer.fix(&file, content), None, "{content}");
        }
    }

    #[test]
    fn file_protocol_links_are_converted() {
        let tmp = repo();
        let fixer = LinkFixer::new(&tmp);
        let fixed = fixer
            .fix(&tmp.join("README.md"), "[F](file:///docs/api.md)\n")
            .unwrap();
        assert_eq!(fixed, "[F](docs/api.md)\n");
    }

    #[test]
    fn relative_paths_walk_up_from_nested_documents() {
        let tmp = repo();
        write(&tmp, "nested/deep/page.md", "x");
        let fixer = LinkFixer::new(&tmp);
        let fixed = fixer
            .fix(&tmp.join("nested/deep/page.md"), "[Up](/docs/api.md)\n")
            .unwrap();
        assert_eq!(fixed, "[Up](../../docs/api.md)\n");
    }

    #[test]
    fn root_absolute_links_lose_their_leading_slash() {
        // The `new_url == link_url` guard is effectively unreachable:
        // a converted URL is always relative and the input always
        // started with "/", so a sibling link becomes a bare name.
        let tmp = repo();
        write(&tmp, "self.md", "x");
        let fixer = LinkFixer::new(&tmp);
        assert_eq!(
            fixer.fix(&tmp.join("self.md"), "[S](/self.md)"),
            Some("[S](self.md)".to_string())
        );
    }

    #[test]
    fn identical_links_are_all_replaced_at_once() {
        // str.replace without a count rewrites every occurrence.
        let tmp = repo();
        let fixer = LinkFixer::new(&tmp);
        let fixed = fixer
            .fix(
                &tmp.join("README.md"),
                "[Same](/docs/guide.md) and [Same](/docs/guide.md)\n",
            )
            .unwrap();
        assert_eq!(fixed, "[Same](docs/guide.md) and [Same](docs/guide.md)\n");
    }

    #[test]
    fn process_directory_skips_unreadable_and_non_text_files() {
        let tmp = repo();
        write(&tmp, "ok.md", "[A](/docs/api.md)\n");
        write(&tmp, "skip.py", "[A](/docs/api.md)\n");
        fs::write(tmp.join("bad.md"), [0xff, 0xfe, b'[']).unwrap();
        let fixer = LinkFixer::new(&tmp);
        let fixes = process_directory(&fixer, &tmp);
        let files: Vec<String> = fixes
            .iter()
            .map(|f| pystr::file_name(&f.file_path))
            .collect();
        assert_eq!(files, vec!["ok.md".to_string()]);
        assert_eq!(fixes[0].fix_type, "link_fix");
    }

    #[test]
    fn relpath_is_lexical() {
        assert_eq!(
            pystr::relpath(Path::new("/a/b/c.md"), Path::new("/a/d")),
            PathBuf::from("../b/c.md")
        );
        assert_eq!(
            pystr::relpath(Path::new("/a/b"), Path::new("/a/b")),
            PathBuf::from(".")
        );
        assert_eq!(
            pystr::relpath(Path::new("/a/b/c"), Path::new("/a")),
            PathBuf::from("b/c")
        );
    }
}
