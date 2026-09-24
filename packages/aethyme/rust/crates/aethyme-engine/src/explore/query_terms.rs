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
    "about",
    "after",
    "all",
    "also",
    "and",
    "any",
    "are",
    "can",
    "code",
    "codebase",
    "could",
    "defined",
    "did",
    "does",
    "doing",
    "done",
    "each",
    "for",
    "from",
    "function",
    "functions",
    "get",
    "gets",
    "happen",
    "happens",
    "has",
    "have",
    "here",
    "how",
    "implement",
    "implemented",
    "implementation",
    "implements",
    "into",
    "its",
    "live",
    "lives",
    "located",
    "logic",
    "look",
    "method",
    "methods",
    "need",
    "not",
    "one",
    "our",
    "out",
    "part",
    "place",
    "repo",
    "repository",
    "should",
    "show",
    "some",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "this",
    "those",
    "use",
    "used",
    "uses",
    "using",
    "via",
    "was",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
    "find",
    "file",
    "files",
    "happening",
    "handled",
    "decide",
    "decides",
    "decided",
    "responsible",
    "work",
    "works",
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

/// Light, language-agnostic suffix stripping. The stem is only ever used as
/// an identifier-boundary prefix, so over-stripping costs precision, never
/// recall; a stem is never shorter than three characters.
pub(super) fn stem(word: &str) -> String {
    const SUFFIXES: &[&str] = &[
        "ations", "ation", "ings", "ing", "ions", "ion", "ies", "ers", "er", "ed", "es", "s", "e",
    ];
    for suffix in SUFFIXES {
        if let Some(base) = word.strip_suffix(suffix)
            && base.len() >= 3
            && !(*suffix == "s" && base.ends_with('s'))
        {
            return base.to_string();
        }
    }
    word.to_string()
}

/// True when `stem` occurs in `line` starting at an identifier boundary:
/// the start of the line, after a non-alphanumeric byte, or at a camelCase
/// transition. `lower` must be `line.to_ascii_lowercase()`.
pub(super) fn occurs_at_boundary(line: &str, lower: &str, stem: &str) -> bool {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(offset) = lower[from..].find(stem) {
        let at = from + offset;
        let boundary = at == 0 || {
            let prev = bytes[at - 1];
            !prev.is_ascii_alphanumeric()
                || (bytes[at].is_ascii_uppercase() && prev.is_ascii_lowercase())
                || (prev.is_ascii_digit() && bytes[at].is_ascii_alphabetic())
        };
        if boundary {
            return true;
        }
        from = at + 1;
        while from < lower.len() && !lower.is_char_boundary(from) {
            from += 1;
        }
        if from >= lower.len() {
            break;
        }
    }
    false
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
        assert_eq!(stem("caching"), "cach");
        assert_eq!(stem("cache"), "cach");
        assert_eq!(stem("cached"), "cach");
        assert_eq!(stem("class"), "class");
        assert_eq!(stem("validation"), "valid");
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
    fn matching_requires_an_identifier_boundary() {
        let line = "let dialogBox = openLogin(fooLog);";
        let lower = line.to_ascii_lowercase();
        assert!(occurs_at_boundary(line, &lower, "login"));
        assert!(occurs_at_boundary(line, &lower, "log"));
        assert!(!occurs_at_boundary("dialog()", "dialog()", "log"));
        assert!(occurs_at_boundary("x.é_cache", "x.é_cache", "cach"));
    }
}
