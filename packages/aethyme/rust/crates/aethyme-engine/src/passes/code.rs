use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;

use crate::cache::{sha256_hex, CacheEntry, CacheStats, ParseCache};
use crate::model::class::ClassNode;
use crate::model::edge::{Edge, EdgeKind};
use crate::model::file::{FileNode, FileRole};
use crate::model::function::FunctionNode;
use crate::indexer;
use crate::passes::structure::StructurePass;
use crate::model::symbol::{Symbol, SymbolKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodePass {
    pub classes: Vec<ClassNode>,
    pub functions: Vec<FunctionNode>,
    pub compatibility_symbols: Vec<Symbol>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeStageProfile {
    pub parse_files_ms: u128,
    pub normalize_symbols_ms: u128,
    pub resolve_imports_ms: u128,
    pub resolve_calls_ms: u128,
    pub resolve_references_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedFile {
    file: FileNode,
    contents: String,
    language: String,
    symbols: Vec<Symbol>,
    import_edges: Vec<Edge>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct InternId(u32);

#[derive(Debug, Default)]
struct StringInterner {
    ids: HashMap<String, InternId>,
    values: Vec<String>,
}

impl StringInterner {
    fn intern(&mut self, value: &str) -> InternId {
        if let Some(id) = self.ids.get(value) {
            return *id;
        }

        let id = InternId(self.values.len() as u32);
        let owned = value.to_string();
        self.values.push(owned.clone());
        self.ids.insert(owned, id);
        id
    }
}

#[derive(Debug, Default)]
struct GlobalIndexes {
    file_ids: HashMap<String, InternId>,
    functions_by_name: HashMap<InternId, Vec<usize>>,
    classes_by_name: HashMap<InternId, Vec<usize>>,
}

#[derive(Debug, Clone)]
struct FunctionBodyAnalysis {
    function_index: usize,
    tokens: BTreeSet<InternId>,
    call_names: BTreeSet<InternId>,
    qualified_call_names: BTreeSet<InternId>,
    same_file_call_targets: Vec<usize>,
}

pub fn build(root: &Path, structure: &StructurePass) -> CodePass {
    build_with_profile(root, structure, None, None).0
}

pub fn build_with_profile(
    root: &Path,
    structure: &StructurePass,
    cache: Option<&ParseCache>,
    grammar_registry: Option<&indexer::tree_sitter::GrammarRegistry>,
) -> (CodePass, CodeStageProfile, ParseCache, CacheStats) {
    let parse_started = Instant::now();
    let source_files: Vec<&FileNode> = structure
        .files
        .iter()
        .filter(|file| file.role == FileRole::Source)
        .collect();

    let parsed_results: Vec<(ParsedFile, bool)> = source_files
        .par_iter()
        .map(|file| parse_file_with_cache(root, file, cache, grammar_registry))
        .collect();

    let mut hits = 0usize;
    let mut misses = 0usize;
    let mut parsed_files = Vec::with_capacity(parsed_results.len());
    let mut new_cache = ParseCache::new();
    for (parsed, was_cached) in parsed_results {
        if was_cached {
            hits += 1;
        } else {
            misses += 1;
        }
        new_cache.insert(
            parsed.file.path.clone(),
            CacheEntry {
                content_hash: sha256_hex(&parsed.contents),
                symbols: parsed.symbols.clone(),
                import_edges: parsed.import_edges.clone(),
            },
        );
        parsed_files.push(parsed);
    }
    parsed_files.sort_by(|left, right| left.file.path.cmp(&right.file.path));
    let parse_files_ms = parse_started.elapsed().as_millis();

    let normalize_started = Instant::now();
    let (mut classes, mut functions, mut compatibility_symbols, mut edges) =
        normalize_symbols(structure, &parsed_files);
    let normalize_symbols_ms = normalize_started.elapsed().as_millis();

    let mut interner = StringInterner::default();
    let mut indexes = build_global_indexes(&functions, &classes, &mut interner);
    let file_function_indexes = build_file_function_index_map(&functions);
    let file_class_indexes = build_file_class_index_map(&classes);
    let files_by_path = structure
        .files
        .iter()
        .map(|file| (file.path.clone(), file.id.clone()))
        .collect::<HashMap<_, _>>();
    let files_by_last_segment = build_last_segment_file_map(&structure.files);

    let import_started = Instant::now();
    let mut resolved_imports_by_file = HashMap::new();
    for parsed in &parsed_files {
        let resolved = resolve_import_edges(&files_by_path, &files_by_last_segment, parsed);
        for edge in &resolved {
            if !edge.to.starts_with("import:") {
                let _ = intern_file_id(&mut indexes, &mut interner, &edge.to);
            }
        }
        edges.extend(resolved.iter().cloned());
        resolved_imports_by_file.insert(parsed.file.id.clone(), resolved);
    }
    let resolve_imports_ms = import_started.elapsed().as_millis();

    let calls_started = Instant::now();
    for parsed in &parsed_files {
        let resolved_imports = resolved_imports_by_file
            .get(&parsed.file.id)
            .cloned()
            .unwrap_or_default();
        let current_function_indexes = file_function_indexes
            .get(&parsed.file.id)
            .cloned()
            .unwrap_or_default();
        let imported_symbol_names =
            imported_symbol_names_for_language(&parsed.language, &parsed.contents, &mut interner);
        let analyses = analyze_file_functions(parsed, &current_function_indexes, &functions, &mut interner);
        edges.extend(resolve_cross_file_calls(
            &analyses,
            &functions,
            &indexes,
            &resolved_imports,
            &imported_symbol_names,
        ));
    }
    let resolve_calls_ms = calls_started.elapsed().as_millis();

    let refs_started = Instant::now();
    for parsed in &parsed_files {
        let resolved_imports = resolved_imports_by_file
            .get(&parsed.file.id)
            .cloned()
            .unwrap_or_default();
        let current_function_indexes = file_function_indexes
            .get(&parsed.file.id)
            .cloned()
            .unwrap_or_default();
        let current_class_indexes = file_class_indexes
            .get(&parsed.file.id)
            .cloned()
            .unwrap_or_default();
        let imported_symbol_names =
            imported_symbol_names_for_language(&parsed.language, &parsed.contents, &mut interner);
        let analyses = analyze_file_functions(parsed, &current_function_indexes, &functions, &mut interner);
        edges.extend(resolve_references(
            &analyses,
            &current_class_indexes,
            &functions,
            &classes,
            &indexes,
            &resolved_imports,
            &imported_symbol_names,
            &interner,
        ));
    }
    let resolve_references_ms = refs_started.elapsed().as_millis();

    classes.sort();
    functions.sort();
    compatibility_symbols.sort();
    edges.sort();
    edges.dedup();

    (
        CodePass {
            classes,
            functions,
            compatibility_symbols,
            edges,
        },
        CodeStageProfile {
            parse_files_ms,
            normalize_symbols_ms,
            resolve_imports_ms,
            resolve_calls_ms,
            resolve_references_ms,
        },
        new_cache,
        CacheStats { hits, misses },
    )
}

fn parse_file_with_cache(
    root: &Path,
    file: &FileNode,
    cache: Option<&ParseCache>,
    grammar_registry: Option<&indexer::tree_sitter::GrammarRegistry>,
) -> (ParsedFile, bool) {
    let absolute_path = root.join(&file.path);
    let contents = fs::read_to_string(&absolute_path).unwrap_or_default();
    let language = file.language.clone().unwrap_or_default();

    if let Some(cache) = cache {
        let hash = sha256_hex(&contents);
        if let Some(entry) = cache.lookup(&file.path, &hash) {
            let symbols = entry
                .symbols
                .iter()
                .map(|s| s.clone().with_context(Some(language.clone()), file.area_id.clone()))
                .collect();
            return (
                ParsedFile {
                    file: file.clone(),
                    contents,
                    language,
                    symbols,
                    import_edges: entry.import_edges.clone(),
                },
                true,
            );
        }
    }

    let symbols = indexer::extract_symbols(grammar_registry, &language, &file.path, &contents)
        .into_iter()
        .map(|symbol| symbol.with_context(Some(language.clone()), file.area_id.clone()))
        .collect::<Vec<_>>();
    let import_edges = indexer::extract_import_edges(grammar_registry, &language, &file.path, &contents);

    (
        ParsedFile {
            file: file.clone(),
            contents,
            language,
            symbols,
            import_edges,
        },
        false,
    )
}

fn normalize_symbols(
    structure: &StructurePass,
    parsed_files: &[ParsedFile],
) -> (Vec<ClassNode>, Vec<FunctionNode>, Vec<Symbol>, Vec<Edge>) {
    let mut classes = Vec::new();
    let mut functions = Vec::new();
    let mut compatibility_symbols = Vec::new();
    let mut edges = Vec::new();

    for parsed in parsed_files {
        for symbol in &parsed.symbols {
            match symbol.kind {
                SymbolKind::Class => {
                    let class = ClassNode::new(
                        &structure.repo_name,
                        &parsed.file.id,
                        &parsed.file.path,
                        parsed.file.area_id.clone(),
                        &parsed.language,
                        &symbol.name,
                        symbol.line,
                        &symbol.signature,
                    );
                    edges.push(Edge::new(
                        &parsed.file.id,
                        &class.id,
                        EdgeKind::Defines,
                        1000,
                        &parsed.language,
                    ));
                    classes.push(class);
                }
                SymbolKind::Function => {
                    let function = FunctionNode::new(
                        &structure.repo_name,
                        &parsed.file.id,
                        &parsed.file.path,
                        parsed.file.area_id.clone(),
                        None,
                        &parsed.language,
                        &symbol.name,
                        symbol.line,
                        &symbol.signature,
                    );
                    edges.push(Edge::new(
                        &parsed.file.id,
                        &function.id,
                        EdgeKind::Defines,
                        1000,
                        &parsed.language,
                    ));
                    functions.push(function);
                }
                SymbolKind::Constant => {}
            }
            compatibility_symbols.push(symbol.clone());
        }
    }

    (classes, functions, compatibility_symbols, edges)
}

fn build_global_indexes(
    functions: &[FunctionNode],
    classes: &[ClassNode],
    interner: &mut StringInterner,
) -> GlobalIndexes {
    let mut indexes = GlobalIndexes::default();

    for (index, function) in functions.iter().enumerate() {
        let name_id = interner.intern(&function.name);
        indexes
            .functions_by_name
            .entry(name_id)
            .or_default()
            .push(index);
        let _ = intern_file_id(&mut indexes, interner, &function.file_id);
    }

    for (index, class) in classes.iter().enumerate() {
        let name_id = interner.intern(&class.name);
        indexes.classes_by_name.entry(name_id).or_default().push(index);
        let _ = intern_file_id(&mut indexes, interner, &class.file_id);
    }

    indexes
}

fn intern_file_id(indexes: &mut GlobalIndexes, interner: &mut StringInterner, file_id: &str) -> InternId {
    if let Some(id) = indexes.file_ids.get(file_id) {
        return *id;
    }
    let id = interner.intern(file_id);
    indexes.file_ids.insert(file_id.to_string(), id);
    id
}

fn build_file_function_index_map(functions: &[FunctionNode]) -> HashMap<String, Vec<usize>> {
    let mut map = HashMap::new();
    for (index, function) in functions.iter().enumerate() {
        map.entry(function.file_id.clone())
            .or_insert_with(Vec::new)
            .push(index);
    }
    for indexes in map.values_mut() {
        indexes.sort_by_key(|index| functions[*index].line);
    }
    map
}

fn build_file_class_index_map(classes: &[ClassNode]) -> HashMap<String, Vec<usize>> {
    let mut map = HashMap::new();
    for (index, class) in classes.iter().enumerate() {
        map.entry(class.file_id.clone())
            .or_insert_with(Vec::new)
            .push(index);
    }
    for indexes in map.values_mut() {
        indexes.sort_by_key(|index| classes[*index].line);
    }
    map
}

fn build_last_segment_file_map(all_files: &[FileNode]) -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    for file in all_files {
        let last = last_target_segment(&file.path);
        map.entry(last).or_insert_with(Vec::new).push(file.id.clone());
    }
    map
}

fn resolve_import_edges(
    files_by_path: &HashMap<String, String>,
    files_by_last_segment: &HashMap<String, Vec<String>>,
    parsed: &ParsedFile,
) -> Vec<Edge> {
    let mut resolved = Vec::new();
    for edge in &parsed.import_edges {
        let resolved_target = resolve_import_target(
            files_by_path,
            files_by_last_segment,
            &parsed.file.path,
            &edge.to,
            &parsed.language,
        )
        .unwrap_or_else(|| format!("import:{}", edge.to));
        resolved.push(Edge::new(
            &parsed.file.id,
            resolved_target,
            EdgeKind::Imports,
            900,
            &edge.source,
        ));
    }
    resolved
}

fn resolve_import_target(
    files_by_path: &HashMap<String, String>,
    files_by_last_segment: &HashMap<String, Vec<String>>,
    source_file: &str,
    raw_target: &str,
    language: &str,
) -> Option<String> {
    indexer::resolve::resolve(language, files_by_path, files_by_last_segment, source_file, raw_target)
}

fn last_target_segment(raw_target: &str) -> String {
    raw_target
        .rsplit('/')
        .next()
        .unwrap_or(raw_target)
        .rsplit('.')
        .next()
        .unwrap_or(raw_target)
        .rsplit("::")
        .next()
        .unwrap_or(raw_target)
        .to_string()
}

fn resolve_cross_file_calls(
    analyses: &[FunctionBodyAnalysis],
    functions: &[FunctionNode],
    indexes: &GlobalIndexes,
    resolved_import_edges: &[Edge],
    imported_symbol_names: &BTreeSet<InternId>,
) -> Vec<Edge> {
    let imported_targets: HashSet<InternId> = resolved_import_edges
        .iter()
        .filter_map(|edge| indexes.file_ids.get(&edge.to).copied())
        .collect();

    let mut edges = Vec::new();
    for analysis in analyses {
        let function = &functions[analysis.function_index];

        for target_index in &analysis.same_file_call_targets {
            let target = &functions[*target_index];
            edges.push(Edge::new(&function.id, &target.id, EdgeKind::Calls, 900, &function.language));
        }

        for (token, target_index) in body_call_candidates(&analysis.call_names, &indexes.functions_by_name) {
            let target = &functions[target_index];
            if target.file_id == function.file_id || target.id == function.id {
                continue;
            }

            let target_file_id = indexes.file_ids.get(&target.file_id).copied();
            let directly_imported = imported_symbol_names.contains(&token);
            let imported_file = target_file_id.is_some_and(|file_id| imported_targets.contains(&file_id));
            if directly_imported || imported_file || analysis.qualified_call_names.contains(&token) {
                let confidence = if directly_imported {
                    920
                } else if imported_file {
                    860
                } else {
                    780
                };
                edges.push(Edge::new(
                    &function.id,
                    &target.id,
                    EdgeKind::Calls,
                    confidence,
                    &function.language,
                ));
            }
        }
    }

    edges.sort();
    edges.dedup();
    edges
}

fn resolve_references(
    analyses: &[FunctionBodyAnalysis],
    current_class_indexes: &[usize],
    functions: &[FunctionNode],
    classes: &[ClassNode],
    indexes: &GlobalIndexes,
    resolved_import_edges: &[Edge],
    imported_symbol_names: &BTreeSet<InternId>,
    interner: &StringInterner,
) -> Vec<Edge> {
    let imported_targets: HashSet<InternId> = resolved_import_edges
        .iter()
        .filter_map(|edge| indexes.file_ids.get(&edge.to).copied())
        .collect();
    let mut edges = Vec::new();
    let local_class_name_indexes = current_class_indexes.iter().copied().fold(
        HashMap::<InternId, Vec<usize>>::new(),
        |mut acc, class_index| {
            let class = &classes[class_index];
            if let Some(class_name_id) = interner.ids.get(&class.name).copied() {
                acc.entry(class_name_id).or_default().push(class_index);
            }
            acc
        },
    );

    for analysis in analyses {
        let function = &functions[analysis.function_index];

        for token in &analysis.tokens {
            if let Some(class_indexes) = local_class_name_indexes.get(token) {
                for class_index in class_indexes {
                    let class = &classes[*class_index];
                    edges.push(Edge::new(
                        &function.id,
                        &class.id,
                        EdgeKind::References,
                        850,
                        &function.language,
                    ));
                }
            }
        }

        for token in &analysis.tokens {
            if let Some(class_matches) = indexes.classes_by_name.get(token) {
                for class_index in class_matches {
                    let class = &classes[*class_index];
                    if class.file_id == function.file_id {
                        continue;
                    }
                    let target_file_id = indexes.file_ids.get(&class.file_id).copied();
                    let imported_file = target_file_id.is_some_and(|file_id| imported_targets.contains(&file_id));
                    let directly_imported = imported_symbol_names.contains(token);
                    if imported_file || directly_imported {
                        let confidence = if directly_imported { 900 } else { 800 };
                        edges.push(Edge::new(
                            &function.id,
                            &class.id,
                            EdgeKind::References,
                            confidence,
                            &function.language,
                        ));
                    }
                }
            }
        }

        for token in &analysis.tokens {
            if let Some(function_matches) = indexes.functions_by_name.get(token) {
                let is_call_name = analysis.call_names.contains(token);
                for target_index in function_matches {
                    let target = &functions[*target_index];
                    if target.id == function.id || target.file_id == function.file_id {
                        continue;
                    }

                    let target_file_id = indexes.file_ids.get(&target.file_id).copied();
                    let imported_file = target_file_id.is_some_and(|file_id| imported_targets.contains(&file_id));
                    let directly_imported = imported_symbol_names.contains(token);
                    if !is_call_name && (imported_file || directly_imported) {
                        let confidence = if directly_imported { 880 } else { 780 };
                        edges.push(Edge::new(
                            &function.id,
                            &target.id,
                            EdgeKind::References,
                            confidence,
                            &function.language,
                        ));
                    }
                }
            }
        }
    }

    edges.sort();
    edges.dedup();
    edges
}

fn imported_symbol_names_for_language(
    language: &str,
    contents: &str,
    interner: &mut StringInterner,
) -> BTreeSet<InternId> {
    let names = match language {
        "python" => python_imported_symbol_names(contents),
        "rust" => rust_imported_symbol_names(contents),
        "typescript" | "javascript" => typescript_imported_symbol_names(contents),
        _ => BTreeSet::new(),
    };
    names.into_iter().map(|name| interner.intern(&name)).collect()
}

fn analyze_file_functions(
    parsed: &ParsedFile,
    current_function_indexes: &[usize],
    functions: &[FunctionNode],
    interner: &mut StringInterner,
) -> Vec<FunctionBodyAnalysis> {
    let mut local_function_name_indexes = HashMap::<InternId, Vec<usize>>::new();
    for function_index in current_function_indexes.iter().copied() {
        let function = &functions[function_index];
        let name_id = interner.intern(&function.name);
        local_function_name_indexes
            .entry(name_id)
            .or_default()
            .push(function_index);
    }

    let mut analyses = Vec::with_capacity(current_function_indexes.len());
    for (offset, function_index) in current_function_indexes.iter().enumerate() {
        let function = &functions[*function_index];
        let next_line = current_function_indexes
            .get(offset + 1)
            .map(|index| functions[*index].line);
        let body = function_body(&parsed.contents, function.line, next_line);
        let tokens = body_identifier_tokens(&body, interner);
        let call_names = body_call_name_tokens(&body, interner);
        let qualified_call_names = body_qualified_call_name_tokens(&body, interner);
        let same_file_call_targets = call_names
            .iter()
            .filter_map(|name_id| local_function_name_indexes.get(name_id))
            .flat_map(|target_indexes| target_indexes.iter().copied())
            .filter(|target_index| functions[*target_index].id != function.id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        analyses.push(FunctionBodyAnalysis {
            function_index: *function_index,
            tokens,
            call_names,
            qualified_call_names,
            same_file_call_targets,
        });
    }
    analyses
}

fn python_imported_symbol_names(contents: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("from ")
            && let Some((_module, imported)) = rest.split_once(" import ")
        {
            for part in imported.split(',') {
                let token = part.trim();
                let imported_name = token
                    .split(" as ")
                    .nth(1)
                    .or_else(|| token.split_whitespace().next())
                    .unwrap_or_default()
                    .trim();
                if !imported_name.is_empty() && imported_name != "*" {
                    names.insert(imported_name.to_string());
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("import ") {
            for part in rest.split(',') {
                let token = part.trim();
                let imported_name = token
                    .split(" as ")
                    .nth(1)
                    .or_else(|| token.rsplit('.').next())
                    .unwrap_or_default()
                    .trim();
                if !imported_name.is_empty() {
                    names.insert(imported_name.to_string());
                }
            }
        }
    }
    names
}

fn rust_imported_symbol_names(contents: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("use ") {
            let use_target = rest.trim_end_matches(';').trim();
            let tail = use_target.rsplit("::").next().unwrap_or(use_target).trim();
            if tail.starts_with('{') && tail.ends_with('}') {
                for item in tail.trim_matches(|c| c == '{' || c == '}').split(',') {
                    let token = item.trim();
                    let imported_name = token
                        .split(" as ")
                        .nth(1)
                        .or_else(|| token.split_whitespace().next())
                        .unwrap_or_default()
                        .trim();
                    if !imported_name.is_empty() && imported_name != "self" {
                        names.insert(imported_name.to_string());
                    }
                }
            } else {
                let imported_name = tail
                    .split(" as ")
                    .nth(1)
                    .or_else(|| tail.split_whitespace().next())
                    .unwrap_or_default()
                    .trim();
                if !imported_name.is_empty() && imported_name != "self" {
                    names.insert(imported_name.to_string());
                }
            }
        }
    }
    names
}

fn typescript_imported_symbol_names(contents: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("import ") {
            if let Some((imports, _source)) = rest.split_once(" from ") {
                let imports = imports.trim();
                if imports.starts_with('{') && imports.ends_with('}') {
                    for item in imports.trim_matches(|c| c == '{' || c == '}').split(',') {
                        let token = item.trim();
                        let imported_name = token
                            .split(" as ")
                            .nth(1)
                            .or_else(|| token.split_whitespace().next())
                            .unwrap_or_default()
                            .trim();
                        if !imported_name.is_empty() {
                            names.insert(imported_name.to_string());
                        }
                    }
                } else if !imports.is_empty() {
                    let imported_name = imports
                        .split(" as ")
                        .nth(1)
                        .or_else(|| imports.split_whitespace().next())
                        .unwrap_or_default()
                        .trim();
                    if !imported_name.is_empty() {
                        names.insert(imported_name.to_string());
                    }
                }
            }
        }
    }
    names
}

fn body_call_candidates(
    call_names: &BTreeSet<InternId>,
    functions_by_name: &HashMap<InternId, Vec<usize>>,
) -> Vec<(InternId, usize)> {
    let mut matches = Vec::new();
    for token in call_names {
        let Some(candidates) = functions_by_name.get(token) else {
            continue;
        };
        matches.extend(candidates.iter().copied().map(|index| (*token, index)));
    }
    matches.sort_unstable();
    matches.dedup();
    matches
}

fn body_identifier_tokens(body: &str, interner: &mut StringInterner) -> BTreeSet<InternId> {
    body.split(|character: char| !character.is_alphanumeric() && character != '_')
        .map(str::trim)
        .filter(|token| token.len() >= 2)
        .map(|token| interner.intern(token))
        .collect()
}

fn body_call_name_tokens(body: &str, interner: &mut StringInterner) -> BTreeSet<InternId> {
    let mut names = BTreeSet::new();
    let mut chars = body.char_indices().peekable();

    while let Some((start, ch)) = chars.next() {
        if !(ch.is_ascii_alphabetic() || ch == '_') {
            continue;
        }

        let mut end = start + ch.len_utf8();
        while let Some((idx, next_ch)) = chars.peek().copied() {
            if next_ch.is_ascii_alphanumeric() || next_ch == '_' {
                end = idx + next_ch.len_utf8();
                let _ = chars.next();
            } else {
                break;
            }
        }

        let mut lookahead = chars.clone();
        while let Some((_, next_ch)) = lookahead.peek().copied() {
            if next_ch.is_whitespace() {
                let _ = lookahead.next();
            } else {
                break;
            }
        }

        if let Some((_, '(')) = lookahead.peek().copied() {
            let token = &body[start..end];
            if token.len() >= 2 {
                names.insert(interner.intern(token));
            }
        }
    }

    names
}

fn body_qualified_call_name_tokens(body: &str, interner: &mut StringInterner) -> BTreeSet<InternId> {
    let mut names = BTreeSet::new();
    let bytes = body.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        let ch = bytes[index] as char;
        if !(ch.is_ascii_alphabetic() || ch == '_') {
            index += 1;
            continue;
        }

        let start = index;
        index += 1;
        while index < bytes.len() {
            let next = bytes[index] as char;
            if next.is_ascii_alphanumeric() || next == '_' {
                index += 1;
            } else {
                break;
            }
        }
        let end = index;

        let mut lookahead = index;
        while lookahead < bytes.len() && (bytes[lookahead] as char).is_whitespace() {
            lookahead += 1;
        }
        if lookahead >= bytes.len() || bytes[lookahead] != b'(' {
            continue;
        }

        let is_qualified = if start >= 1 && bytes[start - 1] == b'.' {
            true
        } else {
            start >= 2 && bytes[start - 2] == b':' && bytes[start - 1] == b':'
        };
        if is_qualified {
            let token = &body[start..end];
            if token.len() >= 2 {
                names.insert(interner.intern(token));
            }
        }
    }

    names
}

fn function_body(contents: &str, start_line: usize, next_line: Option<usize>) -> String {
    let start = start_line.saturating_sub(1);
    let take = next_line
        .map(|line| line.saturating_sub(start_line).max(1))
        .unwrap_or(40)
        .min(80);
    contents
        .lines()
        .skip(start)
        .take(take)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::build;
    use crate::model::edge::EdgeKind;
    use crate::passes::structure;
    use crate::repo::discover_repo;

    #[test]
    fn rust_extraction_finds_structs_and_functions() {
        let root = std::env::temp_dir().join("aethyme_engine_rust_extract_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(
            root.join("src/lib.rs"),
            "pub struct Engine;\n\npub fn make_engine() -> Engine { Engine }\n",
        )
        .expect("write rust source");

        let snapshot = discover_repo(&root).expect("discover repo");
        let structure = structure::build(&snapshot);
        let code = build(&root, &structure);

        assert!(code.classes.iter().any(|class| class.name == "Engine"));
        assert!(code.functions.iter().any(|function| function.name == "make_engine"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cross_file_python_call_resolution_links_imported_function() {
        let root = std::env::temp_dir().join("aethyme_engine_cross_file_call_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(
            root.join("src/main.py"),
            "from auth import validate_token\n\ndef run():\n    return validate_token()\n",
        )
        .expect("write source");
        fs::write(root.join("src/auth.py"), "def validate_token():\n    return True\n").expect("write source");

        let snapshot = discover_repo(&root).expect("discover repo");
        let structure = structure::build(&snapshot);
        let code = build(&root, &structure);

        assert!(code.edges.iter().any(|edge| {
            matches!(edge.kind, EdgeKind::Calls) && edge.from.contains("run") && edge.to.contains("validate_token")
        }));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cross_file_rust_call_resolution_links_imported_function() {
        let root = std::env::temp_dir().join("aethyme_engine_cross_file_rust_call_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(
            root.join("src/lib.rs"),
            "mod auth;\nuse crate::auth::validate_token;\n\npub fn run() -> bool {\n    validate_token()\n}\n",
        )
        .expect("write source");
        fs::write(root.join("src/auth.rs"), "pub fn validate_token() -> bool {\n    true\n}\n")
            .expect("write source");

        let snapshot = discover_repo(&root).expect("discover repo");
        let structure = structure::build(&snapshot);
        let code = build(&root, &structure);

        assert!(code.edges.iter().any(|edge| {
            matches!(edge.kind, EdgeKind::Calls)
                && edge.from.contains("run")
                && edge.to.contains("validate_token")
                && edge.confidence >= 900
        }));

        let _ = fs::remove_dir_all(&root);
    }
}
