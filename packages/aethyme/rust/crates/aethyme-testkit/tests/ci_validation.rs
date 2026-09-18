//! Text contracts for the deliberately explicit CI schedule. YAML syntax is
//! validated separately; these checks guard ownership of expensive coverage.
use std::path::Path;

fn workflow(name: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap();
    std::fs::read_to_string(root.join(".github/workflows").join(name)).unwrap()
}

/// Extract an explicitly indented mapping body, excluding adjacent mappings.
fn block<'a>(text: &'a str, header: &str) -> Vec<&'a str> {
    let indent = header.len() - header.trim_start().len();
    let mut lines = text.lines();
    assert!(lines.any(|line| line == header), "missing {header}");
    lines
        .take_while(|line| {
            line.trim().is_empty()
                || line.trim_start().starts_with('#')
                || line.len() - line.trim_start().len() > indent
        })
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .collect()
}

#[test]
fn pr_and_main_have_one_automatic_full_workspace_owner() {
    let oss = workflow("oss-ci.yml");
    assert_eq!(
        block(&oss, "on:"),
        [
            "  pull_request:",
            "  push:",
            "    branches: [\"main\", \"master\"]"
        ]
    );
    let rust = block(&oss, "  rust-tests:");
    assert!(rust.contains(&"    if: github.event_name == 'pull_request'"));
    assert!(rust.contains(&"        run: cargo test --locked --workspace"));
    // No second condition can silently skip a step within this job.
    assert_eq!(
        rust.iter()
            .filter(|line| line.trim_start().starts_with("if:"))
            .count(),
        1
    );

    let gates = workflow("aethyme-gates.yml");
    assert_eq!(
        block(&gates, "on:"),
        [
            "  push:",
            "    branches: [\"main\", \"master\"]",
            "  workflow_dispatch:"
        ]
    );
    let job = block(&gates, "  gates:");
    assert!(job.contains(
        &"        run: packages/aethyme/rust/target/release/aethyme broker gates run --all"
    ));
    assert!(!job.iter().any(|line| line.trim_start().starts_with("if:")));
}

#[test]
fn duplicate_binary_suite_remains_available_manually() {
    let local = workflow("aethyme-local-tests.yml");
    assert_eq!(block(&local, "on:"), ["  workflow_dispatch:"]);
    let job = block(&local, "  binary-driven-tests:");
    assert!(job.contains(&"        run: cargo test --locked -p aethyme-cli -p aethyme-testkit"));
    assert!(job.contains(&"        run: cargo build --locked --quiet --bin aethyme --bin aethyme-engine-cli --bin aethyme-graph-index"));
    assert!(!job.iter().any(|line| line.trim_start().starts_with("if:")));
}

#[test]
fn distinct_product_proof_remains_automatic_for_both_events() {
    let oss = workflow("oss-ci.yml");
    let product = block(&oss, "  product-path-no-python:");
    assert!(
        !product
            .iter()
            .any(|line| line.trim_start().starts_with("if:"))
    );
    assert!(
        product
            .iter()
            .any(|line| line.contains("aethyme deploy verify --repo ."))
    );
    assert!(
        product
            .iter()
            .any(|line| line.contains("aethyme verify-targets --repo ."))
    );
}

#[test]
fn changed_workflows_have_read_only_permissions() {
    for name in ["oss-ci.yml", "aethyme-gates.yml", "aethyme-local-tests.yml"] {
        assert_eq!(block(&workflow(name), "permissions:"), ["  contents: read"]);
    }
}

#[test]
fn workflow_only_changes_run_this_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap();
    let policy = std::fs::read_to_string(root.join(".aethyme/gates.toml")).unwrap();
    let gate = policy
        .split("[[gate]]")
        .find(|gate| gate.contains("name = \"workflow-contract\""))
        .unwrap();
    assert!(gate.contains("--test ci_validation"));
    assert!(gate.contains("triggers = [\".github/workflows/**\"]"));
}
