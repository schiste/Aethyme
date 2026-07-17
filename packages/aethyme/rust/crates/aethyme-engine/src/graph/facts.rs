use std::collections::BTreeSet;

use crate::map::RepositoryMap;
use crate::model::analysis::{ExposureKind, FunctionFact, FunctionUsageFact};
use crate::model::edge::EdgeKind;
use crate::model::file::FileRole;

pub fn public_function_facts(
    map: &RepositoryMap,
    scope: &str,
    include_methods: bool,
) -> Vec<FunctionFact> {
    let scope = normalize_prefix(scope);
    let mut facts = map
        .functions
        .iter()
        .filter(|function| function.file_path.starts_with(&scope))
        .filter_map(|function| {
            let exposure_kind = classify_exposure(function, include_methods)?;
            let parent_class = function.parent_class_id.as_ref().and_then(|class_id| {
                map.classes
                    .iter()
                    .find(|class| &class.id == class_id)
                    .map(|class| class.name.to_string())
            });
            Some(FunctionFact {
                id: function.id.to_string(),
                name: function.name.to_string(),
                qualified_name: function.qualified_name.to_string(),
                defined_in: function.file_path.to_string(),
                line: function.line,
                language: function.language.to_string(),
                parent_class,
                exposure_kind,
            })
        })
        .collect::<Vec<_>>();
    facts.sort_by(|left, right| {
        left.defined_in
            .cmp(&right.defined_in)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
    });
    facts
}

pub fn function_usage_fact(
    map: &RepositoryMap,
    fact: &FunctionFact,
    boundary: &str,
    searched_roots: &[String],
) -> FunctionUsageFact {
    let boundary = normalize_prefix(boundary);
    let roots = normalize_roots(searched_roots);
    let mut internal = BTreeSet::new();
    let mut external = BTreeSet::new();
    let mut docs_config = BTreeSet::new();

    // Use the inverted edge index instead of scanning every edge in the map.
    // For 30K functions × 3.8M edges on MediaWiki, the old scan was O(F × E);
    // this is O(F × in_degree).
    for &edge_idx in map.edges_to(&fact.id) {
        let edge = &map.edges[edge_idx];
        if matches!(edge.kind, EdgeKind::References | EdgeKind::Documents) {
            if let Some((reference, reference_path)) = docs_config_reference_for_id(map, &edge.from)
            {
                if roots.is_empty() || roots.iter().any(|root| reference_path.starts_with(root)) {
                    docs_config.insert(reference);
                }
                continue;
            }
        }
        if !matches!(
            edge.kind,
            EdgeKind::Calls | EdgeKind::References | EdgeKind::Imports
        ) {
            continue;
        }
        let Some(caller_path) = source_code_path_for_id(map, &edge.from) else {
            continue;
        };
        if !roots.is_empty() && !roots.iter().any(|root| caller_path.starts_with(root)) {
            continue;
        }
        let display = map.display_for(&edge.from);
        if caller_path.starts_with(&boundary) {
            internal.insert(display);
        } else {
            external.insert(display);
        }
    }

    FunctionUsageFact {
        function: fact.clone(),
        boundary: boundary.trim_end_matches('/').to_string(),
        searched_roots: roots,
        internal_callers: internal.into_iter().collect(),
        external_callers: external.into_iter().collect(),
        docs_config_references: docs_config.into_iter().collect(),
    }
}

fn normalize_prefix(value: &str) -> String {
    let trimmed = value.trim_end_matches('/');
    format!("{trimmed}/")
}

fn normalize_roots(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .collect()
}

pub(crate) fn classify_exposure(
    function: &crate::model::function::FunctionNode,
    include_methods: bool,
) -> Option<ExposureKind> {
    let method_like = looks_like_method(function);

    if function.parent_class_id.is_none() && !method_like {
        if function.name.starts_with('_') {
            return None;
        }
        return Some(ExposureKind::ExportedTopLevel);
    }

    if !include_methods {
        return None;
    }

    let signature = function.signature.trim_start();
    if signature.starts_with("public ") {
        return Some(ExposureKind::PublicMethod);
    }
    if matches!(
        function.language.as_str(),
        "python" | "javascript" | "typescript"
    ) && !function.name.starts_with('_')
    {
        return Some(ExposureKind::PublicMethod);
    }
    None
}

fn looks_like_method(function: &crate::model::function::FunctionNode) -> bool {
    if function.parent_class_id.is_some() {
        return true;
    }

    let signature = function.signature.trim_start();
    let lower = signature.to_ascii_lowercase();

    if lower.starts_with("public ")
        || lower.starts_with("private ")
        || lower.starts_with("protected ")
        || lower.starts_with("internal ")
    {
        return true;
    }

    match function.language.as_str() {
        "python" => {
            let self_sig = format!("def {}(self", function.name);
            let cls_sig = format!("def {}(cls", function.name);
            signature.starts_with(&self_sig) || signature.starts_with(&cls_sig)
        }
        "rust" => {
            let self_sig = format!("fn {}(&self", function.name);
            let mut_self_sig = format!("fn {}(&mut self", function.name);
            let owned_self_sig = format!("fn {}(self", function.name);
            signature.starts_with(&self_sig)
                || signature.starts_with(&mut_self_sig)
                || signature.starts_with(&owned_self_sig)
        }
        "javascript" | "typescript" => {
            if lower.starts_with("function ")
                || lower.starts_with("export function ")
                || lower.starts_with("export async function ")
                || lower.starts_with("async function ")
                || signature.contains("=>")
            {
                return false;
            }
            signature.contains('(')
        }
        _ => false,
    }
}

fn source_code_path_for_id(map: &RepositoryMap, value: &str) -> Option<String> {
    if let Some(function) = map.functions.iter().find(|function| function.id == value) {
        return Some(function.file_path.to_string());
    }
    if let Some(class) = map.classes.iter().find(|class| class.id == value) {
        return Some(class.file_path.to_string());
    }
    if let Some(file) = map.files.iter().find(|file| file.id == value) {
        if matches!(file.role, FileRole::Source | FileRole::Test) {
            return Some(file.path.clone());
        }
    }
    None
}

fn docs_config_reference_for_id(map: &RepositoryMap, value: &str) -> Option<(String, String)> {
    if let Some(doc) = map.docs.iter().find(|doc| doc.id == value) {
        return Some((format!("doc:{}", doc.path), doc.path.clone()));
    }
    if let Some(config) = map.configs.iter().find(|config| config.id == value) {
        return Some((format!("config:{}", config.path), config.path.clone()));
    }
    if let Some(file) = map.files.iter().find(|file| file.id == value) {
        return match file.role {
            FileRole::Doc => Some((format!("doc:{}", file.path), file.path.clone())),
            FileRole::Config => Some((format!("config:{}", file.path), file.path.clone())),
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{function_usage_fact, public_function_facts};
    use crate::map::RepositoryMap;

    fn temp_repo(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("aethyme_engine_{name}_{nonce}"))
    }

    #[test]
    fn public_function_facts_finds_exported_top_level_python_functions() {
        let root = temp_repo("facts_public_functions");
        fs::create_dir_all(root.join("src/indexing")).expect("create repo");
        fs::write(
            root.join("src/indexing/service.py"),
            "def public_one():\n    return 1\n\n\
             def _private_one():\n    return 2\n\n\
             class Thing:\n    def public_method(self):\n        return 3\n",
        )
        .expect("write source");

        let map = RepositoryMap::build(&root).expect("build map");
        let facts = public_function_facts(&map, "src/indexing", false);
        let names = facts
            .iter()
            .map(|fact| fact.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["public_one"]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn function_usage_fact_separates_internal_and_external_callers() {
        let root = temp_repo("facts_usage");
        fs::create_dir_all(root.join("src/indexing")).expect("create indexing");
        fs::create_dir_all(root.join("src/api")).expect("create api");
        fs::create_dir_all(root.join("docs")).expect("create docs");
        fs::write(
            root.join("src/indexing/service.py"),
            "def exported_helper():\n    return 1\n\n\
             def local_user():\n    return exported_helper()\n",
        )
        .expect("write service");
        fs::write(
            root.join("src/api/routes.py"),
            "from src.indexing.service import exported_helper\n\n\
             def handler():\n    return exported_helper()\n",
        )
        .expect("write external caller");
        fs::write(
            root.join("docs/notes.md"),
            "# Notes\n\n`exported_helper` is documented here but is not called.\n",
        )
        .expect("write docs");

        let map = RepositoryMap::build(&root).expect("build map");
        let fact = public_function_facts(&map, "src/indexing", false)
            .into_iter()
            .find(|fact| fact.name == "exported_helper")
            .expect("exported_helper fact");
        let usage = function_usage_fact(&map, &fact, "src/indexing", &["src".to_string()]);

        // The fragment indexer does not emit Python `Calls` edges yet,
        // so same-file call sites are not available here. Linked
        // `Imports` edges still provide external usage evidence.
        assert_eq!(usage.internal_callers.len(), 0);
        assert_eq!(usage.external_callers.len(), 1);
        assert!(!usage
            .external_callers
            .iter()
            .any(|caller| caller.contains("notes.md")));
        assert_eq!(usage.docs_config_references, Vec::<String>::new());

        let usage_with_docs = function_usage_fact(&map, &fact, "src/indexing", &[]);
        assert_eq!(usage_with_docs.docs_config_references, Vec::<String>::new());

        let _ = fs::remove_dir_all(&root);
    }
}
