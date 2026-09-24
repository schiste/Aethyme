//! Request parsing: intents, intent selection, disclosure levels, Explore
//! parameters, and symbol-query extraction from the request text.

// ── intents ────────────────────────────────────────────────────────────

/// The two task_localization-shaped intents handled by the redb path.
///
/// `task_localization_query` is the default: bounded answer, compact
/// detail, conservative defaults. `behavior_localization_query` is for
/// change-tasks ("what would I edit to make X happen?") — same engine
/// call, wider params.
///
/// `usage_boundary_query` is dispatched separately because it has its
/// own hybrid analyzer path. This enum only covers the
/// task-localization-shaped intents; the third intent has its own entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    TaskLocalization,
    BehaviorLocalization,
}

impl Intent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Intent::TaskLocalization => "task_localization_query",
            Intent::BehaviorLocalization => "behavior_localization_query",
        }
    }

    /// Apply intent-specific overrides to params. behavior_localization
    /// widens the search: more text candidates, more callsite breadth.
    /// Mirrors Python's behavior_params dict in cli.py:432-436.
    pub fn apply_param_defaults(&self, params: &mut ExploreParams) {
        match self {
            Intent::TaskLocalization => {}
            Intent::BehaviorLocalization => {
                if params.max_text_files < 10 {
                    params.max_text_files = 10;
                }
                if params.max_filename_hints < 5 {
                    params.max_filename_hints = 5;
                }
                // Also expand symbol coverage — change tasks need to
                // see more candidate sites.
                if params.max_symbol_files < 12 {
                    params.max_symbol_files = 12;
                }
            }
        }
    }

    /// Heuristic intent selection from request text.
    ///
    /// Returns `BehaviorLocalization` when the request opens with a
    /// change-task verb ("add", "implement", "fix", etc.) within the
    /// first 10 tokens — that's where intent verbs front-load.
    /// Otherwise returns `TaskLocalization`.
    ///
    /// Cost asymmetry: this heuristic intentionally leans toward
    /// `BehaviorLocalization` when uncertain. Picking behavior when
    /// the user wanted task only costs a slightly wider search;
    /// picking task when the user wanted behavior costs missed
    /// candidate sites — a real quality regression. The signal-set
    /// is conservative (only confident change-verbs) but the bias
    /// is to surface change-shape evidence when it's plausible.
    ///
    /// Currently opt-in via `--intent auto` from the CLI; the
    /// default stays at `TaskLocalization` for back-compat. Once
    /// evals validate the heuristic's hit rate on real requests,
    /// this can become the default.
    pub fn auto_select(request: &str) -> Self {
        const CHANGE_VERBS: &[&str] = &[
            // Additive
            "add",
            "adds",
            "adding",
            "implement",
            "implements",
            "implementing",
            "introduce",
            "introduces",
            "introducing",
            "create",
            "creates",
            "creating",
            "build",
            "builds",
            "building",
            "wire",
            "wires",
            "wiring",
            // Modifying
            "modify",
            "modifies",
            "modifying",
            "edit",
            "edits",
            "editing",
            "change",
            "changes",
            "changing",
            "update",
            "updates",
            "updating",
            "tweak",
            "tweaks",
            // Restructuring
            "refactor",
            "refactors",
            "refactoring",
            "restructure",
            "restructures",
            "restructuring",
            "rewrite",
            "rewrites",
            "rewriting",
            "rename",
            "renames",
            "renaming",
            "extract",
            "extracts",
            "extracting",
            // Fixing
            "fix",
            "fixes",
            "fixing",
            "repair",
            "repairs",
            "repairing",
            "resolve",
            "resolves",
            "resolving",
            "patch",
            "patches",
            "patching",
            // Removing
            "remove",
            "removes",
            "removing",
            "delete",
            "deletes",
            "deleting",
            "drop",
            "drops",
            "dropping",
            "deprecate",
            "deprecates",
            "deprecating",
            "retire",
            "retires",
            "retiring",
            // Migrating
            "migrate",
            "migrates",
            "migrating",
            "port",
            "ports",
            "porting",
            "convert",
            "converts",
            "converting",
        ];
        let lower = request.to_ascii_lowercase();
        // Look only at the first ~10 tokens — verbs front-load.
        let token_iter = lower
            .split(|c: char| {
                c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'')
            })
            .filter(|s| !s.is_empty())
            .take(10);
        for token in token_iter {
            if CHANGE_VERBS.contains(&token) {
                return Intent::BehaviorLocalization;
            }
        }
        Intent::TaskLocalization
    }
}

// ── parameters ──────────────────────────────────────────────────────────

/// One rung of the progressive-disclosure ladder. The agent invokes
/// `aethyme-engine-cli explore --depth N` (0..=3) to dial in just enough
/// context to act, paying the cost only for what it asks about.
///
/// Constraints when editing this table:
///
/// 1. **Each rung must be meaningfully different from the one below.**
///    If depth=2 returns the same content as depth=1 plus 2 lines of
///    snippet, agents will skip 1 and go straight to 2 — the level
///    isn't doing real budget work.
/// 2. **`max_response_tokens` is a soft cap.** The response builder
///    truncates the answer list when serialized output approaches this
///    threshold. Setting it lets the agent treat each call as "buy at
///    most $X of context" rather than "buy whatever the engine
///    decides."
/// 3. **depth=0 must stay genuinely cheap.** This is the discovery
///    rung — agents call it first to map what's relevant. If it
///    bloats, agents stop using the ladder and fall back to bulk
///    loading.
/// 4. **depth=3 is the only rung with `include_call_graph: true`.**
///    Call-graph closure is O(graph) per call; gating it behind the
///    most-specific rung prevents accidental call-graph fan-out on
///    cheap discovery calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisclosureLevel {
    pub max_items: usize,
    pub include_signatures: bool,
    pub include_snippets: bool,
    /// Per-item snippet length cap (lines). 0 = no snippets;
    /// `usize::MAX` = full content (only at depth=3).
    pub snippet_lines: usize,
    pub include_call_graph: bool,
    /// Soft cap on serialized response size. The response builder
    /// truncates the answer list when JSON output approaches this
    /// threshold. Approximate — token counts vary by tokenizer.
    ///
    /// **Status (v1, 2026-07-29):** the response builder enforces
    /// this at the representation layer by capping agent-facing
    /// answer, navigation, subsystem, evidence, and observability
    /// arrays. It is still a soft cap because JSON overhead and
    /// request-specific strings vary, but depth=0 no longer emits
    /// the full debugging observability envelope.
    pub max_response_tokens: usize,
}

/// Progressive-disclosure budget table.
///
/// | depth | items | sigs | snippet | call_graph | ~tokens |
/// |-------|-------|------|---------|------------|---------|
/// | 0     | 15    | no   | —       | no         | ~600    |
/// | 1     | 8     | yes  | —       | no         | ~1500   |
/// | 2     | 3     | yes  | 20 ln   | no         | ~4000   |
/// | 3     | 1     | yes  | full    | yes        | ~8000   |
///
/// Adjusting these is allowed and expected — keep it a *single edit
/// here* rather than threading a new flag through the engine. The
/// constraint comment above lists the invariants any change must
/// preserve.
pub const DISCLOSURE_LEVELS: [DisclosureLevel; 4] = [
    // depth=0 — discovery: names + paths only, no signatures, no
    // snippets. Agents call this first to triage scope. The agent
    // pays ~600 tokens for a map of up to 15 candidates and decides
    // which one to escalate on.
    DisclosureLevel {
        max_items: 15,
        include_signatures: false,
        include_snippets: false,
        snippet_lines: 0,
        include_call_graph: false,
        max_response_tokens: 600,
    },
    // depth=1 — candidates: + signatures and per-item relevance hints.
    // Agents who escalated from depth=0 use this to disambiguate
    // between top candidates without yet paying for source.
    DisclosureLevel {
        max_items: 8,
        include_signatures: true,
        include_snippets: false,
        snippet_lines: 0,
        include_call_graph: false,
        max_response_tokens: 1500,
    },
    // depth=2 — snippets: + 20-line code excerpts for top 3.
    // Agents escalate here when they've narrowed to a small set and
    // need to see what each candidate actually does.
    DisclosureLevel {
        max_items: 3,
        include_signatures: true,
        include_snippets: true,
        snippet_lines: 20,
        include_call_graph: false,
        max_response_tokens: 4000,
    },
    // depth=3 — deep dive: full content + call-graph closure for one
    // anchor. The most expensive rung; intended for a final commit
    // before the agent acts.
    DisclosureLevel {
        max_items: 1,
        include_signatures: true,
        include_snippets: true,
        snippet_lines: usize::MAX,
        include_call_graph: true,
        max_response_tokens: 8000,
    },
];

#[derive(Debug, Clone)]
pub struct ExploreParams {
    pub max_answer_items: usize,
    /// Detail level: `compact`, `standard`, or `full`. Mirrors the Python
    /// `--detail` flag. Today only `compact` is fully implemented in the
    /// native path; standard/full fall back to Python at the call site.
    pub detail: Detail,
    /// Progressive-disclosure depth (0..=3). When `Some(N)`, applies
    /// `DISCLOSURE_LEVELS[N]` as caps over the existing fields —
    /// enforcing a budget-per-call rather than the bulk-load default.
    /// `None` (legacy default) preserves the pre-2026-05-09 behavior:
    /// caps come from `Detail` and explicit `--max-answer-items`.
    /// When both `--depth` and `--detail` are provided, depth wins
    /// (most-specific budget control). Call `apply_disclosure_level()`
    /// to materialize the table values into the existing param fields.
    pub depth: Option<u8>,
    /// Number of distinct symbol queries to derive from the request. The
    /// Python compact default is 5; matched here.
    pub max_symbol_queries: usize,
    /// Per-query result cap for symbol search.
    pub max_symbol_results: usize,
    /// Number of symbol-search-derived files to include in `answer[]`.
    /// Caps independently of `max_answer_items` so symbol evidence
    /// doesn't crowd out anchor evidence.
    pub max_symbol_files: usize,
    /// Maximum source-text candidate files emitted to `answer[]`.
    pub max_text_files: usize,
    /// Per-file cap on the `evidence.line_refs` excerpt list. Only the
    /// highest-scoring lines per file appear in the response — agents read
    /// 1-2; emitting all hits would be ~6,000 tokens of noise on a
    /// well-matching file.
    pub max_text_line_refs: usize,
    /// Number of filename-token matches to surface in
    /// `navigation_hints[]`. Filename-only matches aren't
    /// authoritative; this caps how many we suggest as "look here".
    pub max_filename_hints: usize,
    /// Number of strongest symbol-search hits to feed into the
    /// callsite expansion pass. Each hit can expand incoming redb
    /// adjacency; setting this too high inflates response time on
    /// queries that match many symbols. 4 is the Python compact default
    /// and a reasonable cap.
    pub max_callsite_symbols: usize,
    /// Per-symbol cap on the caller files surfaced as
    /// `call_site_file` answer items. Most agents read the top 3-4;
    /// emitting all callers (which can be hundreds for popular
    /// functions) inflates the response unnecessarily.
    pub max_callsite_results: usize,
    /// When true, emit agent-facing observability. Compact/standard output
    /// includes trust, freshness, Surface/Flow coverage, warnings, and ranking
    /// summaries; `--detail full --show-observability` emits the verbose
    /// debugging envelope.
    pub show_observability: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Compact,
    Standard,
    Full,
}

impl Detail {
    /// Apply detail-level overrides to params. Standard widens caps
    /// roughly 2x; Full widens 4x. Compact uses the user-provided
    /// (or default) values unchanged.
    ///
    /// We widen the *existing* evidence caps rather than emitting
    /// new fields (the way Python's `--detail standard` does with
    /// `output_adapters` etc.) because the agent flow rarely needs
    /// the verbose envelope — what helps is more candidates to
    /// triage. Output_adapters and observability stay compact-shaped.
    ///
    /// Why 2x (not Python's ~5x) at standard: the predecessor Python
    /// `_task_localization_detail_defaults` jumped `max_answer_items`
    /// from 5 (compact) to 24 (standard) — a 4.8x widening. We picked
    /// 2x because the 2026-05-07 evals (GRC + MediaWiki bug-fix-1)
    /// showed agents triage well with 10 candidates at standard;
    /// pushing to ~22 inflates response tokens without observable
    /// quality gain. Callers needing the wider pool can ask for
    /// `--detail full` (4x) or set `--max-answer-items` explicitly.
    /// This is a deliberate divergence from the Python predecessor,
    /// not an oversight (cleanup ladder #5 / project_native_explore_parity_2026_05_07.md).
    pub fn apply_param_widening(&self, params: &mut ExploreParams) {
        let factor: usize = match self {
            Detail::Compact => return,
            Detail::Standard => 2,
            Detail::Full => 4,
        };
        params.max_answer_items = params.max_answer_items.saturating_mul(factor);
        params.max_symbol_queries = params.max_symbol_queries.saturating_mul(factor);
        params.max_symbol_results = params.max_symbol_results.saturating_mul(factor);
        params.max_symbol_files = params.max_symbol_files.saturating_mul(factor);
        params.max_text_files = params.max_text_files.saturating_mul(factor);
        params.max_text_line_refs = params.max_text_line_refs.saturating_mul(factor);
        params.max_filename_hints = params.max_filename_hints.saturating_mul(factor);
    }
}

impl Default for ExploreParams {
    fn default() -> Self {
        Self {
            max_answer_items: 5, // matches Python compact default after f1e3da5
            detail: Detail::Compact,
            depth: None, // legacy detail-based path; --depth flips it
            max_symbol_queries: 5,
            max_symbol_results: 4,
            max_symbol_files: 8, // truncated when answer list fills
            max_text_files: 5,   // matches Python compact default
            max_text_line_refs: 2,
            max_filename_hints: 3,
            max_callsite_symbols: 4,   // Python compact default
            max_callsite_results: 4,   // Python compact default
            show_observability: false, // Python default; --show-observability flips it
        }
    }
}

impl ExploreParams {
    /// Serialize the resolved params as a JSON object for
    /// `resolved_parameters` echo. We don't auto-derive Serialize on
    /// the struct because some downstream consumers expect specific
    /// field naming and Detail's enum form needs a string (not a
    /// debug-formatted variant).
    pub fn to_json(&self) -> serde_json::Value {
        let detail = match self.detail {
            Detail::Compact => "compact",
            Detail::Standard => "standard",
            Detail::Full => "full",
        };
        serde_json::json!({
            "max_answer_items": self.max_answer_items,
            "detail": detail,
            "depth": self.depth,
            "max_symbol_queries": self.max_symbol_queries,
            "max_symbol_results": self.max_symbol_results,
            "max_symbol_files": self.max_symbol_files,
            "max_text_files": self.max_text_files,
            "max_text_line_refs": self.max_text_line_refs,
            "max_filename_hints": self.max_filename_hints,
            "max_callsite_symbols": self.max_callsite_symbols,
            "max_callsite_results": self.max_callsite_results,
            "show_observability": self.show_observability,
        })
    }

    /// Apply the disclosure-level table to the existing param fields.
    ///
    /// When `depth` is `Some(N)` (0..=3), this reads
    /// `DISCLOSURE_LEVELS[N]` and writes the corresponding caps into
    /// `max_answer_items`, `max_text_line_refs`, and the deeper
    /// per-knob fields. Existing non-cap fields (callsite, filename
    /// hints) are scaled proportionally so a depth=0 call doesn't
    /// pay for a wide callsite expansion that contradicts its budget.
    ///
    /// `depth` values outside 0..=3 are clamped to the nearest valid
    /// rung so callers can pass user-supplied integers without an
    /// explicit validation step. The clamping is silent because the
    /// CLI binary validates earlier; this method's robustness is
    /// belt-and-braces for embedded callers (Python via PyO3, future
    /// MCP wiring).
    pub fn apply_disclosure_level(&mut self) {
        let Some(raw_depth) = self.depth else {
            return;
        };
        let depth = (raw_depth as usize).min(DISCLOSURE_LEVELS.len() - 1);
        let level = DISCLOSURE_LEVELS[depth];

        self.max_answer_items = level.max_items;

        // Snippet inclusion gates `max_text_line_refs`. Levels without
        // snippets get 0 line_refs (no excerpts in evidence); levels
        // with snippets get a proportional cap (1 ref at depth=1
        // signature-only, more at higher rungs).
        if level.include_snippets {
            self.max_text_line_refs = match raw_depth {
                2 => 4,
                3 => 10,
                _ => 2,
            };
        } else {
            // depth=0 strips line_refs entirely (just paths/names).
            // depth=1 keeps 1 line_ref to surface the signature
            // line — that's the "+ signatures" promise.
            self.max_text_line_refs = if level.include_signatures { 1 } else { 0 };
        }

        // Cap downstream knobs so they don't crowd the budget.
        // Symbol files / filename hints / callsite expansion all
        // contribute to answer fan-out; scale them down at low depth.
        self.max_symbol_files = self.max_symbol_files.min(level.max_items);
        self.max_text_files = self.max_text_files.min(level.max_items);
        self.max_filename_hints = match raw_depth {
            0 => 0,
            1 => 2,
            _ => self.max_filename_hints,
        };

        // Callsite expansion is deeper than the depth 0/1 contract and
        // only meaningful when the agent is actively closing a loop.
        // Disable below depth=2.
        if raw_depth < 2 {
            self.max_callsite_symbols = 0;
            self.max_callsite_results = 0;
        }
    }
}

// ── intent source ───────────────────────────────────────────────────────

/// How the intent was selected. Reported back in the response so
/// consumers can attribute the choice (an agent that explicitly
/// requested behavior_localization should know its choice was honored,
/// vs the heuristic having picked it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentSource {
    /// No --intent flag — the default (TaskLocalization) was used.
    Default,
    /// Caller passed --intent <X> explicitly.
    Explicit,
    /// Caller passed --intent auto and the heuristic picked X.
    Auto,
}

impl IntentSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            IntentSource::Default => "default",
            IntentSource::Explicit => "explicit",
            IntentSource::Auto => "auto",
        }
    }
}

// ── symbol query extraction (Rust port of _request_symbol_queries) ──────
//
// Tokenizes `request`, drops English stop words and noisy single-letter
// tokens, builds the canonical query list. When a token contains an
// underscore we add the dropped-underscore variant too (so `add_watch`
// also queries `addwatch`). Order-preserving + de-duplicated lowercase.

pub(super) const STOP_WORDS: &[&str] = &[
    "about",
    "after",
    "against",
    "also",
    "and",
    "before",
    "being",
    "between",
    "bug",
    "code",
    "command",
    "could",
    "defined",
    "does",
    "done",
    "file",
    "files",
    "find",
    "fix",
    "for",
    "from",
    "have",
    "here",
    "how",
    "implement",
    "implemented",
    "implementation",
    "into",
    "issue",
    "json",
    "located",
    "make",
    "marked",
    "marks",
    "need",
    "object",
    "not",
    "only",
    "output",
    "path",
    "prose",
    "question",
    "relative",
    "report",
    "repo",
    "repository",
    "request",
    "rules",
    "shape",
    "the",
    "should",
    "specific",
    "that",
    "their",
    "there",
    "this",
    "ticket",
    "seen",
    "viewed",
    "viewing",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "with",
    "would",
    "you",
];

pub(crate) fn extract_symbol_queries(request: &str) -> Vec<String> {
    let normalized = request.replace('`', " ");
    let mut raw_terms: Vec<String> = Vec::new();
    for token in normalized.replace(['/', '-'], " ").split_whitespace() {
        let term: String = token
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if term.len() < 3 {
            continue;
        }
        let lowered = term.to_ascii_lowercase();
        if STOP_WORDS.contains(&lowered.as_str()) {
            continue;
        }
        raw_terms.push(term);
    }

    let mut queries: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for term in &raw_terms {
        let mut variants: Vec<String> = vec![term.clone()];
        if term.contains('_') {
            variants.push(term.replace('_', ""));
        }
        for variant in variants {
            let lowered = variant.to_ascii_lowercase();
            if seen.insert(lowered) {
                queries.push(variant);
            }
        }
    }
    queries
}
