//! Linking a pull request back to the session that opened it.
//!
//! `broker gh --session <id> -- pr create` already records a coordinated
//! operation, but nothing tied the resulting pull request to the session, so
//! the common path -- an agent opens a PR and wants to hear about review --
//! produced no monitoring unless a human noticed the number and ran
//! `watch pr start` by hand (#150).
//!
//! Parsing is separated from starting the watch on purpose. Starting one polls
//! the provider, and the operation that created the PR is still holding the
//! repository write lock; issuing a network call there would reintroduce
//! exactly the head-of-line blocking #138 was filed about. So the number is
//! extracted from output already in hand, and the watch starts outside.

/// Whether a coordinated command creates a pull request.
///
/// Narrow by design: `pr create` only. `pr edit`, `pr comment` and friends act
/// on a pull request that already exists and has its own link, if any.
pub fn creates_pull_request(args: &[String]) -> bool {
    // Flags may precede the subcommand (`--repo o/n pr create`), and dropping
    // every `-` token would leave the flag's *value* looking positional. So
    // match the adjacent pair instead, and require that `pr` is not itself the
    // value of a preceding flag -- otherwise `--title pr create` would read as
    // a creation.
    args.windows(2).enumerate().any(|(index, pair)| {
        pair[0] == "pr" && pair[1] == "create" && (index == 0 || !args[index - 1].starts_with('-'))
    })
}

/// The pull request number from `gh pr create` output.
///
/// `gh` prints the browse URL on success. Matching the URL rather than a bare
/// integer keeps this from picking up a number out of a warning line.
pub fn pull_request_number_from_output(stdout: &str) -> Option<i64> {
    stdout.split_whitespace().find_map(|token| {
        let (_, tail) = token.split_once("/pull/")?;
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<i64>().ok().filter(|number| *number > 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn pr_create_is_recognised_through_its_flags() {
        assert!(creates_pull_request(&args(&[
            "pr", "create", "--title", "x"
        ])));
        assert!(creates_pull_request(&args(&[
            "--repo", "o/n", "pr", "create"
        ])));
    }

    /// A flag value must not be mistaken for the subcommand.
    #[test]
    fn a_title_that_reads_like_the_subcommand_is_not_a_creation() {
        assert!(!creates_pull_request(&args(&[
            "pr", "edit", "--title", "pr", "create"
        ])));
    }

    #[test]
    fn commands_that_do_not_create_a_pull_request_are_not_linked() {
        for command in [
            vec!["pr", "edit", "151"],
            vec!["pr", "comment", "151"],
            vec!["pr", "merge", "151"],
            vec!["issue", "create"],
            vec!["pr"],
        ] {
            assert!(
                !creates_pull_request(&args(&command)),
                "{command:?} must not be treated as PR creation"
            );
        }
    }

    #[test]
    fn the_number_comes_from_the_browse_url() {
        assert_eq!(
            pull_request_number_from_output("https://github.com/schiste/Aethyme/pull/151\n"),
            Some(151)
        );
    }

    /// The reason for matching a URL rather than any integer.
    #[test]
    fn a_stray_number_in_a_warning_is_not_mistaken_for_a_pull_request() {
        let noisy = "warning: 3 files changed\nremote: processed 42 objects\n";
        assert_eq!(pull_request_number_from_output(noisy), None);
    }

    #[test]
    fn a_url_with_a_trailing_path_or_fragment_still_yields_the_number() {
        assert_eq!(
            pull_request_number_from_output("https://github.com/o/n/pull/151/files"),
            Some(151)
        );
    }

    #[test]
    fn output_without_a_pull_request_url_links_nothing() {
        assert_eq!(pull_request_number_from_output(""), None);
        assert_eq!(
            pull_request_number_from_output("https://github.com/o/n"),
            None
        );
    }
}
