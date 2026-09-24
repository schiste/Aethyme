//! Request tokenization for the graph-free source search.
//!
//! A request is reduced to its meaningful terms: English function words and
//! generic question vocabulary are dropped, identifiers are split on
//! camelCase / snake_case / kebab-case boundaries, and every term carries a
//! light suffix-stripped stem so `caching`, `cached` and `cache` meet on
//! `cach`. Matching is prefix-at-an-identifier-boundary, never a raw
//! substring, so a short stem such as `log` does not match inside `dialog`.

/// Words that carry no localization signal in a navigation request.
/// Generic English and question vocabulary only; domain words stay terms.
const STOP_WORDS: &[&str] = &[
    "able",
    "about",
    "above",
    "across",
    "actually",
    "adjust",
    "after",
    "again",
    "against",
    "all",
    "along",
    "already",
    "also",
    "although",
    "always",
    "among",
    "and",
    "another",
    "any",
    "anything",
    "anywhere",
    "are",
    "around",
    "because",
    "been",
    "before",
    "behind",
    "being",
    "below",
    "between",
    "both",
    "but",
    "call",
    "called",
    "caller",
    "callers",
    "calls",
    "can",
    "change",
    "changing",
    "code",
    "codebase",
    "come",
    "comes",
    "could",
    "currently",
    "decide",
    "decided",
    "decides",
    "define",
    "defined",
    "defines",
    "determine",
    "determined",
    "determines",
    "did",
    "does",
    "doing",
    "done",
    "each",
    "either",
    "else",
    "ever",
    "every",
    "everything",
    "exactly",
    "exist",
    "exists",
    "file",
    "files",
    "find",
    "first",
    "for",
    "from",
    "function",
    "functions",
    "get",
    "gets",
    "given",
    "goes",
    "going",
    "handled",
    "handles",
    "happen",
    "happening",
    "happens",
    "has",
    "have",
    "here",
    "how",
    "implement",
    "implementation",
    "implemented",
    "implements",
    "into",
    "invoke",
    "invoked",
    "invokes",
    "its",
    "just",
    "kind",
    "know",
    "like",
    "live",
    "lives",
    "located",
    "logic",
    "look",
    "made",
    "make",
    "makes",
    "many",
    "may",
    "mean",
    "means",
    "method",
    "methods",
    "might",
    "modify",
    "more",
    "most",
    "much",
    "must",
    "need",
    "never",
    "new",
    "not",
    "now",
    "one",
    "only",
    "other",
    "others",
    "our",
    "out",
    "over",
    "own",
    "part",
    "per",
    "place",
    "purpose",
    "really",
    "repo",
    "repository",
    "responsible",
    "same",
    "see",
    "seen",
    "should",
    "show",
    "since",
    "some",
    "such",
    "sure",
    "take",
    "takes",
    "tell",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "thing",
    "things",
    "this",
    "those",
    "though",
    "through",
    "too",
    "tweak",
    "under",
    "until",
    "upon",
    "use",
    "used",
    "uses",
    "using",
    "very",
    "via",
    "want",
    "wants",
    "was",
    "way",
    "ways",
    "well",
    "were",
    "what",
    "when",
    "whenever",
    "where",
    "wherever",
    "whether",
    "which",
    "while",
    "who",
    "whom",
    "whose",
    "why",
    "will",
    "with",
    "within",
    "without",
    "work",
    "works",
    "would",
    "yet",
    "you",
    "your",
];

const MAX_TERMS: usize = 16;
/// Longest run of adjacent request words joined into a compound.
const MAX_COMPOUND_PARTS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Term {
    /// Lowercase surface form as written in the request.
    pub text: String,
    /// Suffix-stripped prefix used for matching.
    pub stem: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct QueryTerms {
    pub terms: Vec<Term>,
    /// Lowercase identifiers with separators removed: identifiers written in
    /// the request (`load_token` -> `loadtoken`) and adjacent word pairs
    /// (`load token` -> `loadtoken`). Used for exact symbol-name matching.
    pub compounds: Vec<String>,
}

impl QueryTerms {
    pub fn parse(request: &str) -> Self {
        let mut out = QueryTerms::default();
        // Consecutive meaningful parts; a stopword breaks the run.
        let mut run: Vec<String> = Vec::new();
        for word in request
            .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
            .filter(|word| !word.is_empty())
        {
            let parts = split_identifier(word);
            if parts.len() >= 2 {
                push_unique(&mut out.compounds, parts.concat());
            }
            let meaningful = parts
                .into_iter()
                .filter(|part| part.len() >= 3 && !part.chars().all(|ch| ch.is_ascii_digit()))
                .filter(|part| !STOP_WORDS.contains(&part.as_str()))
                .collect::<Vec<_>>();
            if meaningful.is_empty() {
                run.clear();
                continue;
            }
            for part in meaningful {
                run.push(part.clone());
                for length in 2..=MAX_COMPOUND_PARTS.min(run.len()) {
                    push_unique(&mut out.compounds, run[run.len() - length..].concat());
                }
                if out.terms.len() < MAX_TERMS && !out.terms.iter().any(|term| term.text == part) {
                    out.terms.push(Term {
                        stem: stem(&part),
                        text: part,
                    });
                }
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn mentions(&self, words: &[&str]) -> bool {
        self.terms
            .iter()
            .any(|term| words.contains(&term.text.as_str()))
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if value.len() >= 6 && !values.contains(&value) {
        values.push(value);
    }
}

/// Split one identifier-ish word into lowercase parts on `_`, digits-to-
/// letters, and camelCase boundaries (`HTTPServer` -> `http`, `server`).
pub(super) fn split_identifier(word: &str) -> Vec<String> {
    let mut parts = Vec::new();
    for chunk in word.split(['_', '-']).filter(|chunk| !chunk.is_empty()) {
        let chars = chunk.chars().collect::<Vec<_>>();
        let mut current = String::new();
        for (index, &ch) in chars.iter().enumerate() {
            let boundary = index > 0 && {
                let prev = chars[index - 1];
                let next_lower = chars.get(index + 1).is_some_and(|c| c.is_lowercase());
                (ch.is_uppercase() && (prev.is_lowercase() || prev.is_ascii_digit()))
                    || (ch.is_uppercase() && prev.is_uppercase() && next_lower)
            };
            if boundary && !current.is_empty() {
                parts.push(std::mem::take(&mut current).to_lowercase());
            }
            current.push(ch);
        }
        if !current.is_empty() {
            parts.push(current.to_lowercase());
        }
    }
    parts
}

/// A light, conservative English stemmer (Porter-style suffix stripping),
/// applied alike to request terms and to the identifier sub-tokens they are
/// matched against, so `retries`, `retried` and `retrying` all meet `retry`,
/// and `validation`, `validates` and `validated` meet `validate`.
///
/// Steps: plurals (`-ies`, `-sses`, `-es` after a sibilant, `-s`), then one
/// of `-ied`, `-ation`, `-ing`, `-ed`, `-able`, `-er` (undoubling a final
/// double consonant: `running` -> `run`), then a final `-e`. No stem is
/// shorter than three characters; a step that would go shorter is skipped.
pub(super) fn stem(word: &str) -> String {
    const MIN: usize = 3;
    let mut word = word.to_string();
    // Plurals.
    if let Some(base) = word
        .strip_suffix("ies")
        .filter(|base| base.len() + 1 >= MIN)
    {
        word = format!("{base}y");
    } else if let Some(base) = word.strip_suffix("sses") {
        word = format!("{base}ss");
    } else if let Some(base) = word.strip_suffix("es").filter(|base| {
        base.len() >= MIN
            && ["s", "x", "z", "ch", "sh"]
                .iter()
                .any(|end| base.ends_with(end))
    }) {
        word = base.to_string();
    } else if let Some(base) = word
        .strip_suffix('s')
        .filter(|base| base.len() >= MIN && !["s", "u", "i"].iter().any(|end| base.ends_with(end)))
    {
        word = base.to_string();
    }
    // One derivational or inflectional suffix.
    if let Some(base) = word
        .strip_suffix("ied")
        .filter(|base| base.len() + 1 >= MIN)
    {
        word = format!("{base}y");
    } else if let Some(base) = word.strip_suffix("ation").filter(|base| base.len() >= MIN) {
        word = format!("{base}ate");
    } else if let Some(base) = ["ing", "ed", "able"]
        .iter()
        .find_map(|suffix| word.strip_suffix(suffix))
        .filter(|base| base.len() >= MIN && base.bytes().any(is_vowel))
    {
        word = undouble(base);
    } else if let Some(base) = word.strip_suffix("er").filter(|base| base.len() > MIN) {
        word = undouble(base);
    }
    if let Some(base) = word.strip_suffix('e').filter(|base| base.len() >= MIN) {
        word = base.to_string();
    }
    word
}

fn is_vowel(byte: u8) -> bool {
    matches!(byte, b'a' | b'e' | b'i' | b'o' | b'u' | b'y')
}

/// `runn` -> `run`, but `pass`, `fall` and `buzz` keep their double letter.
fn undouble(base: &str) -> String {
    let bytes = base.as_bytes();
    let n = bytes.len();
    if n >= 4
        && bytes[n - 1] == bytes[n - 2]
        && !is_vowel(bytes[n - 1])
        && !matches!(bytes[n - 1], b'l' | b's' | b'z')
    {
        return base[..n - 1].to_string();
    }
    base.to_string()
}

/// Whether an identifier sub-token (lowercase) matches a request term's
/// stem: the token starts with the stem, or stems to it.
pub(super) fn token_matches(token: &str, term_stem: &str) -> bool {
    token.starts_with(term_stem) || stem(token) == term_stem
}

/// The part of a stem every surface form it comes from starts with: a
/// restored `y` (`retries` -> `retry`) is not in `retri...`.
fn probe(term_stem: &str) -> &str {
    term_stem.strip_suffix('y').unwrap_or(term_stem)
}

/// Whether `lower` (lowercase text) could hold a match for the stem at all.
pub(super) fn may_contain(lower: &str, term_stem: &str) -> bool {
    lower.contains(probe(term_stem))
}

/// True when `stem` occurs in `line` starting at an identifier boundary
/// (see [`boundary_matches`]). `lower` must be `line.to_ascii_lowercase()`.
#[cfg(test)]
pub(super) fn occurs_at_boundary(line: &str, lower: &str, stem: &str) -> bool {
    boundary_matches(line, lower, stem).next().is_some()
}

/// Byte offsets of identifier sub-tokens in `text` that match `term_stem`
/// (see [`token_matches`]). A sub-token starts at the start of the text,
/// after a non-alphanumeric byte, or at a camelCase or digit-to-letter
/// transition. The stem may also run across a camelCase boundary
/// (`backoff` in `backOff`). `lower` must be `text.to_ascii_lowercase()`.
pub(super) fn boundary_matches<'a>(
    text: &'a str,
    lower: &'a str,
    term_stem: &'a str,
) -> impl Iterator<Item = usize> + 'a {
    let bytes = text.as_bytes();
    lower
        .match_indices(probe(term_stem))
        .map(|(at, _)| at)
        .filter(move |&at| {
            let starts_token = at == 0 || {
                let prev = bytes[at - 1];
                !prev.is_ascii_alphanumeric()
                    || (bytes[at].is_ascii_uppercase() && prev.is_ascii_lowercase())
                    || (prev.is_ascii_digit() && bytes[at].is_ascii_alphabetic())
            };
            starts_token
                && (lower[at..].starts_with(term_stem)
                    || stem(&lower[at..sub_token_end(bytes, at)]) == term_stem)
        })
}

/// End of the identifier sub-token that starts at `at`.
fn sub_token_end(bytes: &[u8], at: usize) -> usize {
    let mut end = at + 1;
    while end < bytes.len() {
        let (prev, ch) = (bytes[end - 1], bytes[end]);
        let next_lower = bytes.get(end + 1).is_some_and(u8::is_ascii_lowercase);
        if !ch.is_ascii_alphanumeric()
            || (ch.is_ascii_uppercase() && prev.is_ascii_lowercase())
            || (ch.is_ascii_uppercase() && prev.is_ascii_uppercase() && next_lower)
            || (ch.is_ascii_alphabetic() && prev.is_ascii_digit())
        {
            break;
        }
        end += 1;
    }
    end
}

/// Identifier-ish tokens (runs of ASCII alphanumerics and `_`) in `line`:
/// the length unit of the code and comment fields.
pub(super) fn token_count(line: &str) -> u32 {
    let mut count = 0;
    let mut inside = false;
    for byte in line.bytes() {
        let word = byte.is_ascii_alphanumeric() || byte == b'_';
        if word && !inside {
            count += 1;
        }
        inside = word;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_split_on_case_and_separators() {
        assert_eq!(split_identifier("loadToken"), ["load", "token"]);
        assert_eq!(split_identifier("HTTPServer"), ["http", "server"]);
        assert_eq!(split_identifier("retry_backoff"), ["retry", "backoff"]);
        assert_eq!(split_identifier("v2Parser"), ["v2", "parser"]);
    }

    #[test]
    fn stems_converge_on_inflections() {
        for group in [
            &["cache", "caches", "cached", "caching"][..],
            &["retry", "retries", "retried", "retrying"],
            &[
                "validate",
                "validates",
                "validated",
                "validating",
                "validation",
            ],
            &["parse", "parser", "parses", "parsing"],
            &["run", "running"],
            &["stop", "stopped"],
            &["class", "classes"],
            &["match", "matches", "matching"],
            &["configure", "configurable"],
        ] {
            let stems = group.iter().map(|word| stem(word)).collect::<Vec<_>>();
            assert!(
                stems.iter().all(|s| *s == stems[0]),
                "{group:?} -> {stems:?}"
            );
        }
        for kept in [
            "class", "status", "analysis", "pass", "fall", "user", "order", "need",
        ] {
            assert!(stem(kept).len() >= 3, "{kept}");
        }
        assert_eq!(stem("status"), "status");
        assert_eq!(stem("pass"), "pass");
        assert_eq!(stem("run"), "run");
    }

    #[test]
    fn requests_drop_stopwords_and_keep_compounds() {
        let query = QueryTerms::parse("Where is the retry backoff for loadToken decided?");
        let texts = query
            .terms
            .iter()
            .map(|term| term.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(texts, ["retry", "backoff", "load", "token"]);
        assert!(query.compounds.contains(&"loadtoken".to_string()));
        assert!(query.compounds.contains(&"retrybackoff".to_string()));
        let query = QueryTerms::parse("the retry throttle window");
        for compound in ["retrythrottle", "throttlewindow", "retrythrottlewindow"] {
            assert!(
                query.compounds.contains(&compound.to_string()),
                "{compound}"
            );
        }
    }

    #[test]
    fn inflected_sub_tokens_match_the_request_stem() {
        let line = "fn retriesLeft() { retried += 1; retryCount; backOff }";
        let lower = line.to_ascii_lowercase();
        assert_eq!(boundary_matches(line, &lower, &stem("retries")).count(), 3);
        assert_eq!(boundary_matches(line, &lower, "backoff").count(), 1);
        assert_eq!(
            boundary_matches("entries", "entries", &stem("retries")).count(),
            0
        );
        assert!(token_matches("validation", &stem("validate")));
        assert!(token_matches("config", &stem("config")));
        assert!(token_matches("configuration", &stem("config")));
        assert!(may_contain("the retries", &stem("retry")));
    }

    #[test]
    fn matching_requires_an_identifier_boundary() {
        let line = "let dialogBox = openLogin(fooLog);";
        let lower = line.to_ascii_lowercase();
        assert!(occurs_at_boundary(line, &lower, "login"));
        assert!(occurs_at_boundary(line, &lower, "log"));
        assert!(!occurs_at_boundary("dialog()", "dialog()", "log"));
        assert!(occurs_at_boundary("x.é_cache", "x.é_cache", "cach"));
    }

    #[test]
    fn every_boundary_match_is_reported() {
        let line = "cacheKey = cache.get(cachedKey) + recache";
        let lower = line.to_ascii_lowercase();
        assert_eq!(boundary_matches(line, &lower, "cach").count(), 3);
        assert_eq!(
            boundary_matches(line, &lower, "key").collect::<Vec<_>>(),
            [5, 27]
        );
        assert_eq!(token_count("let x_y = foo(1, bar);"), 5);
    }

    #[test]
    fn question_phrasing_is_not_a_term() {
        let query = QueryTerms::parse(
            "Which file handles the code that is implemented to decide how retries back off?",
        );
        let texts = query
            .terms
            .iter()
            .map(|term| term.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(texts, ["retries", "back", "off"]);
        let query = QueryTerms::parse("Where would I change what calls the session cache?");
        let texts = query
            .terms
            .iter()
            .map(|term| term.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(texts, ["session", "cache"]);
    }
}
