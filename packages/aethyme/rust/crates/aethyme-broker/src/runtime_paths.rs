pub(crate) const MANAGED_GITIGNORE_BEGIN_MARKER: &str = "# aethyme-broker:begin";
const MANAGED_GITIGNORE_BEGIN_LINE: &str =
    "# aethyme-broker:begin (managed block — do not edit inside)";
pub(crate) const MANAGED_GITIGNORE_END_LINE: &str = "# aethyme-broker:end";

#[derive(Clone, Copy, Debug)]
enum RuntimePathKind {
    Exact,
    Directory,
    FilePrefix,
}

#[derive(Clone, Copy, Debug)]
struct RuntimePathRule {
    gitignore_pattern: &'static str,
    kind: RuntimePathKind,
}

impl RuntimePathRule {
    const fn exact(gitignore_pattern: &'static str) -> Self {
        Self {
            gitignore_pattern,
            kind: RuntimePathKind::Exact,
        }
    }

    const fn directory(gitignore_pattern: &'static str) -> Self {
        Self {
            gitignore_pattern,
            kind: RuntimePathKind::Directory,
        }
    }

    const fn file_prefix(gitignore_pattern: &'static str) -> Self {
        Self {
            gitignore_pattern,
            kind: RuntimePathKind::FilePrefix,
        }
    }

    fn matches(self, path: &str) -> bool {
        match self.kind {
            RuntimePathKind::Exact => {
                path == self.gitignore_pattern
                    || path
                        .strip_prefix(self.gitignore_pattern)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            }
            RuntimePathKind::Directory => path.starts_with(self.gitignore_pattern),
            RuntimePathKind::FilePrefix => {
                let prefix = self
                    .gitignore_pattern
                    .strip_suffix('*')
                    .expect("file-prefix rules end in *");
                path.starts_with(prefix)
            }
        }
    }
}

const BROKER_RUNTIME_PATH_RULES: &[RuntimePathRule] = &[
    RuntimePathRule::file_prefix(".aethyme/broker.db*"),
    RuntimePathRule::directory(".aethyme/logs/"),
    RuntimePathRule::directory(".aethyme/locks/"),
    RuntimePathRule::directory(".aethyme/reports/"),
    RuntimePathRule::directory(".aethyme/run/"),
    RuntimePathRule::directory(".aethyme/worktrees/"),
    RuntimePathRule::exact(".aethyme/broker-action-required.md"),
    RuntimePathRule::exact(".aethyme/broker-advisory.md"),
    RuntimePathRule::exact(".aethyme/graph_store.redb"),
    RuntimePathRule::exact(".aethyme/graph_store.redb.indexing"),
    RuntimePathRule::exact(".aethyme/generated/experience-status.json"),
    RuntimePathRule::exact(".aethyme/generated/experience-status.md"),
    RuntimePathRule::exact(".aethyme/generated/experience-telemetry.jsonl"),
    RuntimePathRule::directory(".aethyme/reviews/"),
    RuntimePathRule::exact(".aethyme/worktree-sizes.json"),
    RuntimePathRule::exact(".aethyme/gc-journal.json"),
    RuntimePathRule::exact(".aethyme/gc.lock"),
];

pub(crate) fn is_broker_runtime_path(path: &str) -> bool {
    BROKER_RUNTIME_PATH_RULES
        .iter()
        .any(|rule| rule.matches(path))
}

pub(crate) fn managed_gitignore_block() -> String {
    let mut block = String::from(MANAGED_GITIGNORE_BEGIN_LINE);
    block.push('\n');
    for rule in BROKER_RUNTIME_PATH_RULES {
        block.push_str(rule.gitignore_pattern);
        block.push('\n');
    }
    block.push_str(MANAGED_GITIGNORE_END_LINE);
    block.push('\n');
    block
}

#[cfg(test)]
mod tests {
    use super::{
        BROKER_RUNTIME_PATH_RULES, MANAGED_GITIGNORE_BEGIN_LINE, MANAGED_GITIGNORE_END_LINE,
        RuntimePathKind, is_broker_runtime_path, managed_gitignore_block,
    };

    #[test]
    fn directory_rules_do_not_classify_same_named_files_as_runtime() {
        for rule in BROKER_RUNTIME_PATH_RULES {
            if let RuntimePathKind::Directory = rule.kind {
                let file_path = rule
                    .gitignore_pattern
                    .strip_suffix('/')
                    .expect("directory rules end in /");
                assert!(
                    !is_broker_runtime_path(file_path),
                    "{} must not classify a same-named regular file as runtime",
                    file_path
                );
            }
        }
    }

    #[test]
    fn generated_ignore_rules_are_the_ship_runtime_path_catalog() {
        let expected = std::iter::once(MANAGED_GITIGNORE_BEGIN_LINE)
            .chain(
                BROKER_RUNTIME_PATH_RULES
                    .iter()
                    .map(|rule| rule.gitignore_pattern),
            )
            .chain(std::iter::once(MANAGED_GITIGNORE_END_LINE))
            .collect::<Vec<_>>();
        assert_eq!(
            managed_gitignore_block().lines().collect::<Vec<_>>(),
            expected
        );

        for rule in BROKER_RUNTIME_PATH_RULES {
            let sample = match rule.kind {
                RuntimePathKind::Exact => rule.gitignore_pattern.to_string(),
                RuntimePathKind::Directory => format!("{}probe", rule.gitignore_pattern),
                RuntimePathKind::FilePrefix => {
                    format!("{}probe", rule.gitignore_pattern.strip_suffix('*').unwrap())
                }
            };
            assert!(
                is_broker_runtime_path(&sample),
                "{} must be classified as runtime",
                rule.gitignore_pattern
            );
        }
        assert!(!is_broker_runtime_path(".aethyme/config.toml"));
        assert!(!is_broker_runtime_path(".aethyme/graph/example.rs.bin"));
        assert!(!is_broker_runtime_path("operator-note.txt"));
        assert!(is_broker_runtime_path(".aethyme/gc.lock/child"));
    }
}
