//! Generate Chau7 snippets from the repository graph.
//!
//! After indexing, the engine produces repo-specific snippets where the body
//! IS the prompt — pre-computed navigation context baked in. When a user
//! triggers a snippet in Chau7, the full prompt (task + context) gets pasted
//! directly into the agent session. No CLI command at runtime.

use std::path::Path;

use crate::context::prompt;
use crate::map::RepositoryMap;
use crate::model::file::FileRole;

/// Generate and write Chau7 snippets for a repository.
pub fn generate_and_write(repo_root: &Path, map: &RepositoryMap) -> Result<(), String> {
    let snippets = generate(repo_root, map);
    let file = SnippetFile {
        version: 1,
        snippets,
    };
    let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    let dir = repo_root.join(".chau7");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("snippets.json"), json).map_err(|e| e.to_string())?;
    Ok(())
}

/// Generate snippets from the repository map.
/// Each snippet body IS the prompt — context pre-baked, task as a fill-in field.
fn generate(repo_root: &Path, map: &RepositoryMap) -> Vec<Snippet> {
    let areas = top_areas(map);
    let keys = &mut KeyAssigner::new();
    let mut snippets = Vec::new();

    // 1. Overview prompt
    let overview_context = prompt::generate_prompt(
        repo_root,
        map,
        "${input:task:Explain this repository}",
        Some("overview"),
    );
    snippets.push(Snippet {
        id: "aethyme-explore-overview".into(),
        title: "Explore: Overview".into(),
        body: overview_context,
        tags: vec!["aethyme".into(), "explore".into()],
        folder: Some("Aethyme".into()),
        key: keys.next(),
        is_pinned: true,
    });

    // 2. One snippet per top-level area
    for area in areas.iter().take(8) {
        let context = prompt::generate_prompt(
            repo_root,
            map,
            &format!("${{input:task:Explain the {} subsystem}}", area),
            Some(area),
        );
        snippets.push(Snippet {
            id: format!("aethyme-explore-{}", sanitize_snippet_id(area)),
            title: format!("Explore: {}/", area),
            body: context,
            tags: vec!["aethyme".into(), "explore".into()],
            folder: Some("Aethyme".into()),
            key: keys.next(),
            is_pinned: false,
        });
    }

    // 3. Testing prompt
    let has_tests = map.files.iter().any(|f| matches!(f.role, FileRole::Test));
    if has_tests {
        let context = prompt::generate_prompt(
            repo_root,
            map,
            "${input:task:Explain the testing strategy}",
            Some("testing"),
        );
        snippets.push(Snippet {
            id: "aethyme-explore-testing".into(),
            title: "Explore: Testing".into(),
            body: context,
            tags: vec!["aethyme".into(), "explore".into()],
            folder: Some("Aethyme".into()),
            key: keys.next(),
            is_pinned: false,
        });
    }

    // 4. Utility snippets (these ARE commands, not prompts)
    snippets.push(Snippet {
        id: "aethyme-search".into(),
        title: "Search Symbols".into(),
        body: "aethyme-engine-cli symbol --repo . --query ${input:query}".into(),
        tags: vec!["aethyme".into(), "tool".into()],
        folder: Some("Aethyme Tools".into()),
        key: None,
        is_pinned: false,
    });

    snippets.push(Snippet {
        id: "aethyme-reindex".into(),
        title: "Re-index Repository".into(),
        body: "aethyme-engine-cli index --repo .".into(),
        tags: vec!["aethyme".into(), "tool".into()],
        folder: Some("Aethyme Tools".into()),
        key: None,
        is_pinned: false,
    });

    snippets
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Directories to exclude from snippet options.
pub(crate) const EXCLUDED_AREAS: &[&str] = &[
    ".pnpm-store",
    "node_modules",
    "vendor",
    ".git",
    ".cache",
    "dist",
    "build",
    "target",
    "coverage",
    "__pycache__",
    ".next",
    ".nuxt",
    ".svelte-kit",
    "test-results",
    "logs",
    ".aethyme",
    ".chau7",
    ".codex",
];

/// Extract top-level area names, sorted by file count, excluding vendor/generated.
fn top_areas(map: &RepositoryMap) -> Vec<String> {
    let mut areas: Vec<(&str, usize)> = map
        .areas
        .iter()
        .filter(|a| {
            !a.inferred
                && !a.path_prefix.contains('/')
                && !EXCLUDED_AREAS.contains(&a.path_prefix.as_str())
                && !a.name.starts_with('.')
        })
        .map(|a| {
            let count = map
                .files
                .iter()
                .filter(|f| f.path.starts_with(&format!("{}/", a.path_prefix)))
                .count();
            (a.path_prefix.as_str(), count)
        })
        .filter(|(_, count)| *count > 0)
        .collect();

    areas.sort_by(|a, b| b.1.cmp(&a.1));
    areas
        .into_iter()
        .map(|(name, _)| name.to_string())
        .collect()
}

fn sanitize_snippet_id(name: &str) -> String {
    name.replace('/', "-").replace(' ', "-").to_lowercase()
}

/// Assigns single-letter keys a-z to snippets in order.
struct KeyAssigner {
    next: u8,
}

impl KeyAssigner {
    fn new() -> Self {
        Self { next: b'a' }
    }

    fn next(&mut self) -> Option<String> {
        if self.next > b'z' {
            return None;
        }
        let key = (self.next as char).to_string();
        self.next += 1;
        Some(key)
    }
}

// ── Serialization types matching Chau7's SnippetFile format ─────────

#[derive(serde::Serialize)]
struct SnippetFile {
    version: i32,
    snippets: Vec<Snippet>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Snippet {
    id: String,
    title: String,
    body: String,
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    is_pinned: bool,
}

fn is_false(v: &bool) -> bool {
    !v
}
