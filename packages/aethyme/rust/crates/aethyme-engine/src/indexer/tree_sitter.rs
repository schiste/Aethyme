use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use tree_sitter::{
    Language, Parser, Query, QueryCursor, StreamingIterator, WasmStore, wasmtime::Engine,
};

use crate::model::edge::{Edge, EdgeKind};
use crate::model::symbol::{Symbol, SymbolKind};

pub struct LoadedGrammar {
    pub language: Language,
    pub query: Query,
}

pub struct GrammarRegistry {
    engine: Engine,
    grammars: HashMap<String, LoadedGrammar>,
}

unsafe impl Send for GrammarRegistry {}
unsafe impl Sync for GrammarRegistry {}

impl GrammarRegistry {
    pub fn load(grammars_dir: &Path) -> Self {
        let engine = Engine::default();
        let mut grammars = HashMap::new();

        if !grammars_dir.is_dir() {
            return Self { engine, grammars };
        }

        let entries = match fs::read_dir(grammars_dir) {
            Ok(entries) => entries,
            Err(_) => return Self { engine, grammars },
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let lang_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };

            let wasm_path = path.join("grammar.wasm");
            let query_path = path.join("queries.scm");

            if !wasm_path.exists() || !query_path.exists() {
                continue;
            }

            let wasm_bytes = match fs::read(&wasm_path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    eprintln!("warning: failed to read {}: {e}", wasm_path.display());
                    continue;
                }
            };

            let query_source = match fs::read_to_string(&query_path) {
                Ok(source) => source,
                Err(e) => {
                    eprintln!("warning: failed to read {}: {e}", query_path.display());
                    continue;
                }
            };

            let mut store = match WasmStore::new(&engine) {
                Ok(store) => store,
                Err(e) => {
                    eprintln!("warning: failed to create WasmStore for {lang_name}: {e}");
                    continue;
                }
            };

            let language = match store.load_language(&lang_name, &wasm_bytes) {
                Ok(lang) => lang,
                Err(e) => {
                    eprintln!("warning: failed to load grammar for {lang_name}: {e}");
                    continue;
                }
            };

            let query = match Query::new(&language, &query_source) {
                Ok(query) => query,
                Err(e) => {
                    eprintln!("warning: failed to compile query for {lang_name}: {e}");
                    continue;
                }
            };

            grammars.insert(lang_name, LoadedGrammar { language, query });
        }

        Self { engine, grammars }
    }

    pub fn has_grammar(&self, language: &str) -> bool {
        self.grammars.contains_key(language)
    }

    pub fn languages(&self) -> Vec<&str> {
        self.grammars.keys().map(|k| k.as_str()).collect()
    }
}

thread_local! {
    static THREAD_PARSER: RefCell<Option<Parser>> = RefCell::new(None);
}

fn with_parser<F, R>(engine: &Engine, f: F) -> R
where
    F: FnOnce(&mut Parser) -> R,
{
    THREAD_PARSER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let mut parser = Parser::new();
            let store = WasmStore::new(engine).expect("WasmStore creation failed");
            parser.set_wasm_store(store).expect("set_wasm_store failed");
            *slot = Some(parser);
        }
        f(slot.as_mut().unwrap())
    })
}

pub fn extract(
    registry: &GrammarRegistry,
    language: &str,
    path: &str,
    contents: &str,
) -> Option<(Vec<Symbol>, Vec<Edge>)> {
    let grammar = registry.grammars.get(language)?;

    with_parser(&registry.engine, |parser| {
        if parser.set_language(&grammar.language).is_err() {
            return None;
        }
        let tree = parser.parse(contents, None)?;
        let mut cursor = QueryCursor::new();
        let mut symbols = Vec::new();
        let mut edges = Vec::new();

        let mut matches = cursor.matches(&grammar.query, tree.root_node(), contents.as_bytes());
        while let Some(m) = matches.next() {
            process_match(&grammar.query, m, path, contents, &mut symbols, &mut edges);
        }

        Some((symbols, edges))
    })
}

fn process_match(
    query: &Query,
    m: &tree_sitter::QueryMatch,
    path: &str,
    source: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let mut name_text: Option<&str> = None;
    let mut kind: Option<SymbolKind> = None;
    let mut line: usize = 0;
    let mut signature = String::new();

    for capture in m.captures {
        let capture_name = query.capture_names()[capture.index as usize];
        let text = capture
            .node
            .utf8_text(source.as_bytes())
            .unwrap_or_default();

        if capture_name == "symbol.function.name" {
            name_text = Some(text);
            kind = Some(SymbolKind::Function);
            line = capture.node.start_position().row + 1;
        } else if capture_name == "symbol.class.name" {
            name_text = Some(text);
            kind = Some(SymbolKind::Class);
            line = capture.node.start_position().row + 1;
        } else if capture_name == "symbol.constant.name" {
            name_text = Some(text);
            kind = Some(SymbolKind::Constant);
            line = capture.node.start_position().row + 1;
        } else if capture_name == "symbol.function" || capture_name == "symbol.class" {
            let sig_line = source
                .lines()
                .nth(capture.node.start_position().row)
                .unwrap_or_default();
            signature = sig_line.trim_start().to_string();
            if line == 0 {
                line = capture.node.start_position().row + 1;
            }
        } else if capture_name == "import.source" {
            edges.push(Edge::new(path, text, EdgeKind::Imports, 900, "tree-sitter"));
        }
    }

    if let (Some(name), Some(symbol_kind)) = (name_text, kind) {
        if !name.is_empty() {
            if signature.is_empty() {
                signature = source
                    .lines()
                    .nth(line.saturating_sub(1))
                    .unwrap_or_default()
                    .trim_start()
                    .to_string();
            }
            symbols.push(Symbol::new(name, symbol_kind, path, line, signature.as_str()));
        }
    }
}

pub fn default_grammars_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    // From target/debug/ or target/release/ → ../../grammars/
    let candidate = exe_dir.join("../../grammars");
    if candidate.is_dir() {
        return Some(candidate.canonicalize().unwrap_or(candidate));
    }
    // Also try next to the binary
    let candidate = exe_dir.join("grammars");
    if candidate.is_dir() {
        return Some(candidate);
    }
    None
}
