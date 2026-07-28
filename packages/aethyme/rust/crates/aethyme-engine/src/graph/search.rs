use std::collections::HashMap;

use crate::map::RepositoryMap;
use crate::store::redb::graph_store::{
    GraphStoreError, ReadOnlyGraphStore, StoredNodeKind, SymbolCandidate, SymbolMatchOptions,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub score: i32,
    pub reason: String,
}

// ── Multi-signal scoring constants (2026-05-12) ──────────────────────────
//
// The previous scorer was per-token and substring-based:
//   exact name == query token              => 300
//   name.starts_with(query)                 => 200
//   name.contains(query)                    => 100
//
// That produced two failure modes the MediaWiki bug-fix-1 diagnostic
// surfaced:
//
//   1. Morphology mismatch — a query token "viewing" never matched a
//      symbol named "doViewUpdates" because "doviewupdates" doesn't
//      *contain* "viewing" as a substring. The two share the stem "view",
//      but substring matching couldn't see that. As a consequence,
//      `WikiPage::doViewUpdates` (the fix-site producer for T419918)
//      scored zero on every query token.
//
//   2. Single-token saturation — for query token "page", a function
//      literally named `page()` scored 300 (exact match) and beat
//      `showDiffPage` (substring, 100). The compound-name candidate
//      that obviously combined multiple query terms got out-ranked by
//      a generic short-name match.
//
// Multi-signal scorer fixes both by:
//
//   - Splitting symbol names into components on camelCase / snake_case /
//     dotted boundaries (acronym-aware: HTTPResponse -> [HTTP, Response]).
//   - Stem-matching components against query tokens via a shared-prefix
//     test (4 chars minimum, case-insensitive).
//   - Compound bonus: each ADDITIONAL distinct token that the name
//     matches adds NAME_COMPOUND_PER_EXTRA. So 1 matched token = 100,
//     2 = 250, 3 = 400 — strongly favors compound names like
//     `showDiffPage`.
//   - Cross-signal scoring: file path and area name also count toward
//     relevance. `doViewUpdates` in `includes/Page/WikiPage.php` scores
//     for the "page" token via the path match even when its symbol name
//     alone wouldn't.
//
// Weight values: chosen for "Balanced ~50/50" preset (selected
// interactively 2026-05-12). Symbol name still dominates but location
// matters meaningfully — adjust here if a future preset changes the
// balance.

/// Minimum prefix-length for a stem match to count. 4 was chosen because
/// it's the shortest length at which the common English/code root
/// fragments (`view`, `page`, `auth`, `user`, `file`, `data`) are
/// distinct. 3 produces noisy matches (`get` -> `getter`, `setting`,
/// anything with `get` substring). 5 misses common 4-char roots.
const MIN_STEM_LEN: usize = 4;

/// Per matched query token on the symbol name.
const NAME_BASE: i32 = 100;
/// Compound bonus per ADDITIONAL matched token beyond the first.
/// So 1 token = 100, 2 = 250, 3 = 400 — super-linear reward for names
/// that combine multiple query concepts.
const NAME_COMPOUND_PER_EXTRA: i32 = 150;
/// Per matched query token on the symbol's file path.
const PATH_PER_TOKEN: i32 = 60;
/// Per matched query token on the symbol's area name.
const AREA_PER_TOKEN: i32 = 40;
/// Bonus when the file's basename (last segment, without extension)
/// equals a query token exactly. Helps `Article.php` surface for the
/// token "article" even when the function name itself doesn't match.
const BASENAME_EXACT_BONUS: i32 = 80;

/// Legacy single-query entry point. Splits the query on whitespace and
/// delegates to `symbol_search_multi`. Preserves the existing API for
/// the daemon RPC handler and `aethyme-engine-cli search-symbol`
/// commands without churning callers.
pub fn symbol_search(map: &RepositoryMap, query: &str, limit: usize) -> Vec<SearchHit> {
    let tokens: Vec<String> = query.split_whitespace().map(|s| s.to_string()).collect();
    symbol_search_multi(map, &tokens, limit)
}

/// Redb-backed V2 symbol search. The store owns bounded candidate lookup and
/// multi-signal ranking; this adapter preserves the SearchHit JSON contract.
pub fn symbol_search_redb(
    store: &ReadOnlyGraphStore,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, GraphStoreError> {
    let tokens: Vec<String> = query.split_whitespace().map(|s| s.to_string()).collect();
    symbol_search_redb_multi(store, &tokens, limit)
}

/// Exact redb lookup for callers that already have tokenized task/query text.
pub fn symbol_search_redb_multi(
    store: &ReadOnlyGraphStore,
    tokens: &[String],
    limit: usize,
) -> Result<Vec<SearchHit>, GraphStoreError> {
    if tokens.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let query = tokens.join(" ");
    let candidates = store.symbols_matching_with(
        &query,
        SymbolMatchOptions {
            limit,
            ..SymbolMatchOptions::default()
        },
    )?;
    Ok(candidates
        .into_iter()
        .map(symbol_candidate_to_search_hit)
        .collect())
}

/// Multi-token symbol search. Use this directly when callers already
/// have a pre-tokenized query (e.g. anchors.rs's `candidate_queries`).
pub fn symbol_search_multi(map: &RepositoryMap, tokens: &[String], limit: usize) -> Vec<SearchHit> {
    if tokens.is_empty() {
        return Vec::new();
    }

    // Pre-compute lowercased tokens once. We use these everywhere the
    // matcher needs case-insensitive comparison; lowering inside the
    // hot loop would re-allocate per symbol-token pair (~19K * 5 =
    // 95K extra allocations on MediaWiki).
    let lowered_tokens: Vec<String> = tokens
        .iter()
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect();
    if lowered_tokens.is_empty() {
        return Vec::new();
    }

    // Pre-build area_id -> area_name lookup so the per-symbol area
    // scoring is O(1) rather than O(n_areas). Worth the up-front
    // allocation on MediaWiki where the area count is ~50 and the
    // symbol count is ~19K.
    let area_names: HashMap<String, String> = map
        .areas
        .iter()
        .map(|area| (area.id.to_string(), area.name.to_ascii_lowercase()))
        .collect();

    let mut hits = Vec::new();

    for class in &map.classes {
        let area_name = class
            .area_id
            .as_ref()
            .and_then(|id| area_names.get(id.as_str()).map(|s| s.as_str()));
        if let Some((score, matched)) =
            score_symbol(&class.name, &class.file_path, area_name, &lowered_tokens)
        {
            hits.push(SearchHit {
                id: class.id.to_string(),
                name: class.name.to_string(),
                kind: "class".to_string(),
                file: class.file_path.to_string(),
                line: class.line,
                score,
                reason: format!("class-name-match via {}", matched.join("+")),
            });
        }
    }

    for function in &map.functions {
        let area_name = function
            .area_id
            .as_ref()
            .and_then(|id| area_names.get(id.as_str()).map(|s| s.as_str()));
        if let Some((score, matched)) = score_symbol(
            &function.name,
            &function.file_path,
            area_name,
            &lowered_tokens,
        ) {
            hits.push(SearchHit {
                id: function.id.to_string(),
                name: function.name.to_string(),
                kind: "function".to_string(),
                file: function.file_path.to_string(),
                line: function.line,
                score,
                reason: format!("function-name-match via {}", matched.join("+")),
            });
        }
    }

    hits.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
    });
    hits.truncate(limit);
    hits
}

fn stored_symbol_kind(kind: StoredNodeKind) -> &'static str {
    match kind {
        StoredNodeKind::Repository => "repository",
        StoredNodeKind::Directory => "directory",
        StoredNodeKind::Function => "function",
        StoredNodeKind::Class => "class",
        StoredNodeKind::File => "file",
        StoredNodeKind::Area => "area",
        StoredNodeKind::Doc => "doc",
        StoredNodeKind::Config => "config",
        StoredNodeKind::BehaviorTestSurface => "behavior_test_surface",
        StoredNodeKind::CliSurface => "cli_surface",
        StoredNodeKind::CredentialOperation => "credential_operation",
        StoredNodeKind::JobSurface => "job_surface",
        StoredNodeKind::MiddlewareInstallation => "middleware_installation",
        StoredNodeKind::ProxySurface => "proxy_surface",
        StoredNodeKind::QueueSurface => "queue_surface",
        StoredNodeKind::RouteSurface => "route_surface",
        StoredNodeKind::WebhookSurface => "webhook_surface",
        StoredNodeKind::WorkerSurface => "worker_surface",
        StoredNodeKind::Unresolved => "unresolved",
    }
}

fn symbol_candidate_to_search_hit(candidate: SymbolCandidate) -> SearchHit {
    let reason = symbol_candidate_reason(&candidate);
    SearchHit {
        id: candidate.symbol.id,
        name: candidate.symbol.name,
        kind: stored_symbol_kind(candidate.symbol.kind).to_string(),
        file: candidate.symbol.path,
        line: candidate.symbol.line,
        score: candidate.rank,
        reason,
    }
}

fn symbol_candidate_reason(candidate: &SymbolCandidate) -> String {
    let mut signals = Vec::new();
    if candidate.signals.exact {
        signals.push("exact-name");
    }
    if candidate.signals.case_insensitive {
        signals.push("case-insensitive-name");
    }
    if candidate.signals.prefix {
        signals.push("prefix-name");
    }
    if candidate.signals.component {
        signals.push("component-name");
    }
    if candidate.signals.path {
        signals.push("path");
    }
    if candidate.signals.area {
        signals.push("area");
    }
    if candidate.signals.basename {
        signals.push("basename");
    }
    if signals.is_empty() {
        signals.push("candidate");
    }
    format!("redb-symbol-search:{}", signals.join("+"))
}

/// Score one symbol against the full token set. Returns `Some((score,
/// matched_tokens))` when the symbol has at least some signal; `None`
/// otherwise. The `matched_tokens` vec is used in the SearchHit's
/// reason field (joined by `+`) so consumers can see WHICH tokens led
/// to the match.
///
/// Signals (Balanced weights):
///
/// - Name (via component split): NAME_BASE per matched token +
///   NAME_COMPOUND_PER_EXTRA per ADDITIONAL token beyond the first.
///   1 matched token = 100, 2 = 250, 3 = 400, 4 = 550.
/// - File path: PATH_PER_TOKEN per distinct token whose 4-char stem
///   appears in the path string.
/// - Area name: AREA_PER_TOKEN per distinct token matched on area.
/// - Basename exact: BASENAME_EXACT_BONUS when any token equals the
///   filename without extension.
pub(crate) fn score_symbol(
    name: &str,
    file_path: &str,
    area_name: Option<&str>,
    lowered_tokens: &[String],
) -> Option<(i32, Vec<String>)> {
    // Name match via acronym-aware components.
    let components = split_components(name);
    let component_lowers: Vec<String> = components.iter().map(|c| c.to_ascii_lowercase()).collect();
    let mut name_matched_tokens: Vec<String> = Vec::new();
    let name_lower = name.to_ascii_lowercase();
    for token in lowered_tokens {
        if name_lower == *token
            || name_lower.contains(token.as_str())
            || component_lowers.iter().any(|c| shares_stem(c, token))
        {
            if !name_matched_tokens.contains(token) {
                name_matched_tokens.push(token.clone());
            }
        }
    }
    let name_score = match name_matched_tokens.len() {
        0 => 0,
        n => NAME_BASE + NAME_COMPOUND_PER_EXTRA * (n as i32 - 1),
    };

    // Path match: each token whose 4-char stem appears as a substring
    // of the file_path scores once. We use plain substring here
    // (rather than the component-split + stem-share path-name uses)
    // because file paths are heterogeneous (slashes, dots, mixed
    // case, ints) and component splitting would over-fragment them.
    let path_lower = file_path.to_ascii_lowercase();
    let mut path_matched_tokens: Vec<String> = Vec::new();
    for token in lowered_tokens {
        if token.len() >= MIN_STEM_LEN
            && path_lower.contains(token.as_str())
            && !path_matched_tokens.contains(token)
        {
            path_matched_tokens.push(token.clone());
        }
    }
    let path_score = path_matched_tokens.len() as i32 * PATH_PER_TOKEN;

    // Area match: same logic, against area name (already lowercased
    // in the lookup table).
    let area_score = area_name
        .map(|a| {
            lowered_tokens
                .iter()
                .filter(|t| t.len() >= MIN_STEM_LEN && a.contains(t.as_str()))
                .count() as i32
                * AREA_PER_TOKEN
        })
        .unwrap_or(0);

    // Basename exact match: filename without extension equals any
    // single token.
    let basename_lower = file_path
        .rsplit('/')
        .next()
        .unwrap_or(file_path)
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let basename_bonus =
        if !basename_lower.is_empty() && lowered_tokens.iter().any(|t| *t == basename_lower) {
            BASENAME_EXACT_BONUS
        } else {
            0
        };

    let total = name_score + path_score + area_score + basename_bonus;
    if total == 0 {
        None
    } else {
        // Surface the tokens that contributed to NAME signal in the
        // reason (highest-signal source). Path/area-only matches get
        // an empty token list, which renders as ""—still informative
        // because the score reflects the location signal.
        Some((total, name_matched_tokens))
    }
}

/// Split a symbol name into matchable components. Acronym-aware.
///
/// Boundaries:
///   - lowercase followed by uppercase: `doViewUpdates` -> `do | View | Updates`
///   - uppercase-run followed by uppercase-then-lowercase:
///     `HTTPResponse` -> `HTTP | Response`,
///     `XMLHttpRequest` -> `XML | Http | Request`
///   - underscores and dots: `parse_html_tag` -> `parse | html | tag`,
///     `Foo.Bar` -> `Foo | Bar`
///
/// Numbers are NOT treated as boundaries (Foo123 stays together).
/// Empty components (e.g. leading/trailing underscores) are dropped.
fn split_components(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    let chars: Vec<char> = name.chars().collect();
    let n = chars.len();

    for i in 0..n {
        let ch = chars[i];

        // Underscore / dot are hard boundaries, not included in any
        // component.
        if ch == '_' || ch == '.' {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            continue;
        }

        if current.is_empty() {
            current.push(ch);
            continue;
        }

        let prev = chars[i - 1];

        // lowercase-or-digit -> uppercase transition (camelCase
        // boundary). Treating digits like lowercase here means
        // `foo123Bar` splits at the `B` (boundary between digit `3`
        // and uppercase `B`), giving [foo123, Bar]. Without this,
        // `B` wouldn't split (prev `3` is neither lowercase nor
        // uppercase), and the symbol would stay glued. We do NOT
        // split before/after digits in other directions (`Foo123`
        // stays together — the "acronym-aware" preset doesn't split
        // on digit boundaries, only across them).
        let lc_uc = (prev.is_ascii_lowercase() || prev.is_ascii_digit()) && ch.is_ascii_uppercase();
        // uppercase-run ending: previous char was uppercase, current
        // is uppercase, AND the NEXT char is lowercase. That marks
        // the boundary inside an acronym followed by a CamelWord —
        // e.g. `HTTPResponse` at position `P` (prev `T`, ch `P`,
        // next `R` is uppercase → stay together) … wait, that's
        // wrong. Let me redo: we want to split BEFORE the `R` in
        // `HTTPResponse` (prev=`P`, ch=`R`, next=`e`). So the
        // condition is: prev uppercase + ch uppercase + next
        // lowercase.
        let acronym_break = prev.is_ascii_uppercase()
            && ch.is_ascii_uppercase()
            && i + 1 < n
            && chars[i + 1].is_ascii_lowercase();

        if lc_uc || acronym_break {
            out.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }

    if !current.is_empty() {
        out.push(current);
    }

    out
}

/// True iff `a` and `b` share a case-insensitive prefix of length
/// `MIN_STEM_LEN`. Both strings must be at least `MIN_STEM_LEN`
/// characters; otherwise the function returns false (we don't match
/// 1-3 char strings against anything — those are too generic).
///
/// Examples (MIN_STEM_LEN = 4):
///   shares_stem("view", "viewing")    -> true  (both >=4, share "view")
///   shares_stem("viewing", "viewed")  -> true  (share "view")
///   shares_stem("page", "pages")      -> true  (share "page")
///   shares_stem("vie",  "viewing")    -> false ("vie" < MIN_STEM_LEN)
///   shares_stem("view", "review")     -> false (different prefixes)
fn shares_stem(a: &str, b: &str) -> bool {
    if a.len() < MIN_STEM_LEN || b.len() < MIN_STEM_LEN {
        return false;
    }
    let a_prefix: String = a.chars().take(MIN_STEM_LEN).collect();
    let b_prefix: String = b.chars().take(MIN_STEM_LEN).collect();
    a_prefix.eq_ignore_ascii_case(&b_prefix)
}

#[cfg(test)]
mod component_splitter_tests {
    //! Tests for `split_components` (acronym-aware camelCase /
    //! snake_case / dotted name splitter). The splitter feeds the
    //! name-signal scoring: bad splits break stem matching for
    //! anything that isn't ASCII-letter-lowercase-only.
    use super::split_components;

    #[test]
    fn camel_case_basic() {
        assert_eq!(
            split_components("doViewUpdates"),
            vec!["do", "View", "Updates"]
        );
    }

    #[test]
    fn acronym_followed_by_camel_word() {
        // `HTTPResponse` — the acronym `HTTP` should stay together,
        // then split before `Response` (the uppercase-then-lowercase
        // boundary at P-R-e). Same shape as MediaWiki's
        // `MWException`, `XMLHttpRequest`, etc.
        assert_eq!(split_components("HTTPResponse"), vec!["HTTP", "Response"]);
        assert_eq!(split_components("MWException"), vec!["MW", "Exception"]);
        assert_eq!(
            split_components("XMLHttpRequest"),
            vec!["XML", "Http", "Request"]
        );
    }

    #[test]
    fn snake_case() {
        assert_eq!(
            split_components("parse_html_tag"),
            vec!["parse", "html", "tag"]
        );
    }

    #[test]
    fn dotted_names() {
        assert_eq!(split_components("Foo.Bar.baz"), vec!["Foo", "Bar", "baz"]);
    }

    #[test]
    fn single_lowercase_word() {
        assert_eq!(split_components("page"), vec!["page"]);
    }

    #[test]
    fn single_capital_word() {
        assert_eq!(split_components("Page"), vec!["Page"]);
    }

    #[test]
    fn empty_returns_empty() {
        assert_eq!(split_components(""), Vec::<String>::new());
    }

    #[test]
    fn underscores_at_edges_drop_empty_components() {
        // `__foo__` should yield `["foo"]`, not `["", "", "foo", "", ""]`.
        assert_eq!(split_components("__foo__"), vec!["foo"]);
    }

    #[test]
    fn numbers_stay_attached() {
        // We deliberately do NOT split on digit boundaries (Aggressive
        // strategy was an option but the user picked Acronym-aware).
        // `Foo123` stays together; `foo123Bar` splits at the camel
        // boundary only.
        assert_eq!(split_components("Foo123"), vec!["Foo123"]);
        assert_eq!(split_components("foo123Bar"), vec!["foo123", "Bar"]);
    }
}

#[cfg(test)]
mod stem_tests {
    //! Tests for `shares_stem`, the 4-char-prefix bidirectional match.
    use super::shares_stem;

    #[test]
    fn matching_root() {
        assert!(shares_stem("view", "viewing"));
        assert!(shares_stem("viewing", "view"));
        assert!(shares_stem("viewing", "viewed"));
        assert!(shares_stem("page", "pages"));
    }

    #[test]
    fn case_insensitive() {
        assert!(shares_stem("View", "viewing"));
        assert!(shares_stem("PAGE", "page"));
    }

    #[test]
    fn distinct_prefixes_do_not_match() {
        // 4-char prefixes "view" vs "revi" → no overlap.
        assert!(!shares_stem("view", "review"));
        assert!(!shares_stem("page", "wiki"));
    }

    #[test]
    fn strings_shorter_than_min_stem_never_match() {
        // 3 chars or fewer = too generic; always reject.
        assert!(!shares_stem("vie", "viewing"));
        assert!(!shares_stem("foo", "foo"));
        assert!(!shares_stem("get", "getter"));
    }

    #[test]
    fn exactly_min_stem_length_works() {
        // Boundary case: both strings exactly 4 chars, equal.
        assert!(shares_stem("view", "view"));
        assert!(shares_stem("page", "page"));
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{symbol_search, symbol_search_multi};
    use crate::map::RepositoryMap;

    #[test]
    fn symbol_search_prefers_exact_matches() {
        // Original test — preserved. The new scorer still prefers
        // exact matches because `main` is a single component, and
        // `main_helper` splits into `[main, helper]`. Both have
        // `main` as a component, both get name_score = 100 (1 token
        // matched). The tiebreaker is alphabetical: `main` sorts
        // before `main_helper`. Sufficient for this test's intent.
        let root = std::env::temp_dir().join("aethyme_engine_search_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(
            root.join("src/main.py"),
            "def main():\n    return 1\n\ndef main_helper():\n    return 2\n",
        )
        .expect("write source file");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let hits = symbol_search(&map, "main", 10);

        assert_eq!(hits.first().map(|item| item.name.as_str()), Some("main"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn multi_token_compound_beats_single_token_exact() {
        // Regression test for the MediaWiki bug-fix-1 anchor failure
        // (per the diagnostic that motivated this rewrite).
        //
        // Pre-fix: token "page" exact-matched a function literally
        // named `page()` for score 300, beating `showDiffPage` (which
        // only got 100 via substring `.contains("page")`).
        //
        // Post-fix: `showDiffPage` decomposes to [show, Diff, Page].
        // Tokens [show, diff, page] match 3 components → name_score
        // = 100 + 150*2 = 400. `page()` decomposes to [page] →
        // matches 1 token → name_score = 100. Compound name wins.
        let root = std::env::temp_dir().join("aethyme_engine_search_compound_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create dir");
        fs::write(
            root.join("src/lib.py"),
            "def page():\n    return 1\n\ndef showDiffPage():\n    return 2\n",
        )
        .expect("write source");

        let map = RepositoryMap::build(&root).expect("build map");
        let tokens = vec!["show".to_string(), "diff".to_string(), "page".to_string()];
        let hits = symbol_search_multi(&map, &tokens, 10);

        assert_eq!(
            hits.first().map(|h| h.name.as_str()),
            Some("showDiffPage"),
            "expected showDiffPage to outrank bare `page`; got: {:?}",
            hits.iter()
                .map(|h| (h.name.as_str(), h.score))
                .collect::<Vec<_>>()
        );
        let show_diff_page = hits.iter().find(|h| h.name == "showDiffPage").unwrap();
        let page = hits.iter().find(|h| h.name == "page").unwrap();
        assert!(
            show_diff_page.score > page.score,
            "showDiffPage score ({}) should exceed page score ({})",
            show_diff_page.score,
            page.score,
        );
    }

    #[test]
    fn morphology_aware_name_match() {
        // Pre-fix: token "viewing" never matched `doViewUpdates`
        // because substring matching can't see that they share the
        // root `view`. Post-fix: stem-match against the [View]
        // component succeeds.
        let root = std::env::temp_dir().join("aethyme_engine_search_morphology_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create dir");
        fs::write(
            root.join("src/lib.py"),
            "def doViewUpdates():\n    return 1\n",
        )
        .expect("write source");

        let map = RepositoryMap::build(&root).expect("build map");
        let tokens = vec!["viewing".to_string()];
        let hits = symbol_search_multi(&map, &tokens, 10);

        assert!(
            hits.iter().any(|h| h.name == "doViewUpdates"),
            "expected doViewUpdates to match token 'viewing' via shared stem 'view'; got: {:?}",
            hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn file_path_signal_surfaces_symbols_with_unrelated_names() {
        // A symbol whose NAME doesn't match any token but whose file
        // path does should still surface (via the path signal).
        // E.g. `Article` class in `includes/Page/Article.php` for a
        // query mentioning "page".
        let root = std::env::temp_dir().join("aethyme_engine_search_path_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("includes/Page")).expect("create dir");
        fs::write(
            root.join("includes/Page/Article.php"),
            "<?php\nclass Article {\n  public function read() {}\n}\n",
        )
        .expect("write source");

        let map = RepositoryMap::build(&root).expect("build map");
        // Query mentions "page" (matches path/area) but not "article"
        // (no token equals the class name).
        let tokens = vec!["page".to_string()];
        let hits = symbol_search_multi(&map, &tokens, 10);

        assert!(
            hits.iter().any(|h| h.name == "Article"),
            "Article should surface via file_path 'includes/Page/Article.php' \
             matching token 'page'; got: {:?}",
            hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn empty_tokens_returns_empty() {
        let root = std::env::temp_dir().join("aethyme_engine_search_empty_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create dir");
        fs::write(root.join("src/lib.py"), "def foo():\n    pass\n").expect("write");

        let map = RepositoryMap::build(&root).expect("build map");
        let hits = symbol_search_multi(&map, &[], 10);
        assert!(hits.is_empty());

        // Whitespace-only via the wrapper also produces no results.
        let hits2 = symbol_search(&map, "   ", 10);
        assert!(hits2.is_empty());
    }
}
