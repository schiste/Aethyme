//! Generic path-role classification for source-search ranking.
//!
//! Roles come only from widely shared conventions (directory names such as
//! `vendor/` or `node_modules/`, minified or lock-file names, "generated"
//! headers) and never from any particular repository's layout.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PathRole {
    Source,
    Test,
    Docs,
    Example,
    Config,
    Vendored,
    Generated,
    Other,
}

impl PathRole {
    pub fn as_str(self) -> &'static str {
        match self {
            PathRole::Source => "source",
            PathRole::Test => "test",
            PathRole::Docs => "docs",
            PathRole::Example => "example",
            PathRole::Config => "config",
            PathRole::Vendored => "vendored",
            PathRole::Generated => "generated",
            PathRole::Other => "other",
        }
    }

    /// Ranking weight. A request that itself asks about tests or docs lifts
    /// that role back to parity with source.
    pub fn weight(self, wants_tests: bool, wants_docs: bool) -> f64 {
        match self {
            PathRole::Source => 1.0,
            PathRole::Test if wants_tests => 1.0,
            PathRole::Test => 0.45,
            PathRole::Docs if wants_docs => 1.0,
            PathRole::Docs => 0.35,
            PathRole::Example => 0.6,
            PathRole::Config => 0.5,
            PathRole::Other => 0.5,
            PathRole::Vendored | PathRole::Generated => 0.12,
        }
    }
}

const VENDORED_DIRS: &[&str] = &[
    "vendor",
    "vendors",
    "vendored",
    "third_party",
    "third-party",
    "thirdparty",
    "node_modules",
    "bower_components",
    "site-packages",
    "pods",
];
const GENERATED_DIRS: &[&str] = &[
    "dist",
    "build",
    "out",
    "generated",
    "__generated__",
    "target",
    "coverage",
];
const TEST_DIRS: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "spec",
    "specs",
    "testing",
    "fixtures",
    "testdata",
];
const DOC_DIRS: &[&str] = &["doc", "docs", "documentation"];
const EXAMPLE_DIRS: &[&str] = &["example", "examples", "sample", "samples", "demo", "demos"];
/// Conventional names for code kept only for reference, not live behavior.
const RETIRED_DIRS: &[&str] = &["legacy", "deprecated", "archive", "archived", "obsolete"];
const DOC_EXTENSIONS: &[&str] = &["md", "markdown", "rst", "adoc", "txt", "org"];
const CONFIG_EXTENSIONS: &[&str] = &[
    "json",
    "jsonc",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "xml",
    "csv",
    "lock",
    "env",
    "properties",
    "plist",
];
const CODE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cxx", "h", "hh", "hpp", "cs", "ex", "exs", "go", "hs", "java", "js", "cjs",
    "mjs", "jsx", "kt", "kts", "lua", "php", "py", "pyi", "rb", "rs", "scala", "swift", "ts",
    "tsx", "vue", "svelte", "dart", "m", "mm", "sh", "bash", "zsh", "sql", "gd", "cls", "erl",
    "clj", "ml", "fs", "r", "pl", "pm", "groovy", "gradle",
];
const LOCK_FILES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "cargo.lock",
    "poetry.lock",
    "composer.lock",
    "gemfile.lock",
    "go.sum",
    "uv.lock",
];

pub(super) struct Classified {
    pub role: PathRole,
    /// Under a conventional retired-code directory (`legacy/`, `archive/`).
    pub retired: bool,
    pub is_code: bool,
}

pub(super) fn classify(path: &str) -> Classified {
    let lower = path.to_ascii_lowercase();
    let mut components = lower.split('/').collect::<Vec<_>>();
    let file_name = components.pop().unwrap_or_default();
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or("");
    let has_dir = |names: &[&str]| components.iter().any(|dir| names.contains(dir));
    let retired = has_dir(RETIRED_DIRS);
    let is_code = CODE_EXTENSIONS.contains(&extension);
    let role = if has_dir(VENDORED_DIRS) {
        PathRole::Vendored
    } else if has_dir(GENERATED_DIRS)
        || file_name.contains(".min.")
        || file_name.contains(".generated.")
        || file_name.contains(".bundle.")
        || file_name.ends_with("_pb2.py")
        || file_name.ends_with(".pb.go")
        || file_name.ends_with(".map")
        || LOCK_FILES.contains(&file_name)
    {
        PathRole::Generated
    } else if has_dir(TEST_DIRS)
        || file_name.starts_with("test_")
        || file_name.contains("_test.")
        || file_name.contains(".test.")
        || file_name.contains(".spec.")
        || file_name.contains("_spec.")
        || (is_code && camel_test_suffix(path))
    {
        PathRole::Test
    } else if DOC_EXTENSIONS.contains(&extension) || (has_dir(DOC_DIRS) && !is_code) {
        PathRole::Docs
    } else if has_dir(EXAMPLE_DIRS) {
        PathRole::Example
    } else if is_code {
        PathRole::Source
    } else if CONFIG_EXTENSIONS.contains(&extension) {
        PathRole::Config
    } else {
        PathRole::Other
    };
    Classified {
        role,
        retired,
        is_code,
    }
}

/// `FooTest.java`, `FooTests.swift`: a capitalized `Test` suffix on the
/// file stem (case-sensitive, so `latest.py` is not a test).
fn camel_test_suffix(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    stem.len() > 4 && (stem.ends_with("Test") || stem.ends_with("Tests"))
}

/// True when the first lines carry a conventional machine-generated marker.
pub(super) fn has_generated_header(head: &str) -> bool {
    head.lines().take(6).any(|line| {
        let lower = line.trim_start().to_ascii_lowercase();
        let comment = ["//", "#", "/*", "*", "<!--", "--", ";", "\"\"\""]
            .iter()
            .any(|marker| lower.starts_with(marker));
        comment
            && (lower.contains("@generated")
                || lower.contains("do not edit")
                || lower.contains("auto-generated")
                || lower.contains("autogenerated")
                || lower.contains("automatically generated")
                || lower.contains("generated by "))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_follow_generic_conventions() {
        assert_eq!(classify("src/cache.rs").role, PathRole::Source);
        assert_eq!(classify("lib/vendor/x/cache.js").role, PathRole::Vendored);
        assert_eq!(classify("web/node_modules/a/b.js").role, PathRole::Vendored);
        assert_eq!(classify("static/app.min.js").role, PathRole::Generated);
        assert_eq!(classify("dist/app.js").role, PathRole::Generated);
        assert_eq!(classify("tests/test_cache.py").role, PathRole::Test);
        assert_eq!(classify("src/cache.test.ts").role, PathRole::Test);
        assert_eq!(classify("src/cache_test.go").role, PathRole::Test);
        assert_eq!(classify("docs/design/cache.md").role, PathRole::Docs);
        assert_eq!(classify("README.md").role, PathRole::Docs);
        assert_eq!(classify("config/settings.yaml").role, PathRole::Config);
        assert_eq!(classify("src/latest.py").role, PathRole::Source);
        assert_eq!(classify("src/CacheTest.java").role, PathRole::Test);
        assert!(classify("legacy/cache.py").retired);
        assert!(!classify("src/cache.py").retired);
    }

    #[test]
    fn generated_headers_are_detected() {
        assert!(has_generated_header(
            "// Code generated by protoc. DO NOT EDIT.\n"
        ));
        assert!(has_generated_header("# @generated\nx = 1\n"));
        assert!(!has_generated_header("fn main() {}\n"));
        assert!(!has_generated_header(
            "let s = \"token generated by server\";\n"
        ));
    }
}
