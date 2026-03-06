use std::path::Path;

use crate::anchors::resolve_anchors;
use crate::context_pack::{ContextPack, DependencyEdge, ImpactItem};
use crate::guidance::{build_in_scope, build_out_of_scope, navigation_order};
use crate::map::RepositoryMap;
use crate::neighborhood::{dependency_frontier, impact_frontier};
use crate::snippets::select_snippets;
use crate::task::TaskInput;

pub fn build_context_pack(root: &Path, map: &RepositoryMap, task: TaskInput) -> ContextPack {
    let anchor_limit = if task.kind.is_explain_repo() { 5 } else { 3 };
    let scope_limit = if task.kind.is_explain_repo() { 8 } else { 5 };
    let anchors = resolve_anchors(map, &task, anchor_limit);
    let anchor_targets: Vec<String> = anchors.iter().map(|anchor| anchor.id.clone()).collect();
    let mut dependencies = Vec::new();
    let mut impact = Vec::new();

    for target in &anchor_targets {
        for dependency in dependency_frontier(map, target) {
            dependencies.push(DependencyEdge {
                from: map.display_for(target),
                to: dependency,
                kind: "related".to_string(),
            });
        }
        for item in impact_frontier(map, target) {
            impact.push(ImpactItem {
                symbol: item.clone(),
                file: item,
                reason: "reverse dependency".to_string(),
            });
        }
    }

    dependencies.sort();
    dependencies.dedup();
    impact.sort();
    impact.dedup();

    let in_scope = build_in_scope(map, &anchors, scope_limit);
    let out_of_scope = build_out_of_scope(map, &anchors, &task.kind);
    let snippets = select_snippets(root, &anchors, 8);
    let navigation = navigation_order(&anchors);

    let anchor_confidence = if anchors.is_empty() {
        0.0
    } else if task.kind.is_explain_repo() && anchors.len() >= 3 {
        0.85
    } else {
        0.75
    };
    let scope_confidence = if in_scope.files.is_empty() && in_scope.areas.is_empty() {
        0.0
    } else if task.kind.is_explain_repo() && !in_scope.areas.is_empty() {
        0.8
    } else {
        0.70
    };

    let mut pack = ContextPack {
        task,
        anchors,
        in_scope,
        out_of_scope,
        dependencies,
        impact,
        snippets,
        risk_flags: map.risk_flags.clone(),
        navigation_order: navigation,
        budget: Default::default(),
        confidence: crate::context_pack::Confidence {
            anchor_confidence,
            scope_confidence,
        },
    };
    pack.budget.max_anchors = anchor_limit;
    pack.budget.max_files = scope_limit;
    pack.sort();
    pack
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::build_context_pack;
    use crate::map::RepositoryMap;
    use crate::task::TaskInput;

    #[test]
    fn explain_repo_pack_includes_readme_anchor() {
        let root = std::env::temp_dir().join("aethyme_engine_pack_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(root.join("README.md"), "# Demo\n").expect("write readme");
        fs::write(root.join("src/main.py"), "def main():\n    return 1\n").expect("write entrypoint");

        let map = RepositoryMap::build(&root).expect("build map");
        let pack = build_context_pack(&root, &map, TaskInput::from_task_text("Explain this repo"));

        assert!(!pack.anchors.is_empty());
        assert!(pack.navigation_order.iter().any(|value| value == "README.md"));
        assert!(pack.navigation_order.iter().any(|value| value == "src"));
        assert!(pack.out_of_scope.areas.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn change_symbol_pack_uses_anchor_file_for_dependency_frontier() {
        let root = std::env::temp_dir().join("aethyme_engine_change_symbol_pack_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(
            root.join("src/main.py"),
            "from auth import validate_token\n\n".to_string() + "def main():\n    return validate_token()\n",
        )
        .expect("write entrypoint");
        fs::write(root.join("src/auth.py"), "def validate_token():\n    return True\n").expect("write source file");

        let map = RepositoryMap::build(&root).expect("build map");
        let pack = build_context_pack(&root, &map, TaskInput::from_task_text("Update validate_token flow"));

        assert!(pack.anchors.iter().any(|anchor| anchor.id.contains("validate_token")));
        assert!(pack.in_scope.files.iter().any(|item| item.value == "src/auth.py"));

        let _ = fs::remove_dir_all(&root);
    }
}
