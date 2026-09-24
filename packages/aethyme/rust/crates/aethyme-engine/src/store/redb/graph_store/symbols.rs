//! Symbol search: exact, prefix, component and path-component lookups,
//! candidate scoring, task anchors and usage-boundary seeds.

use super::*;

pub(super) fn find_symbols_from<D: ReadableDatabase>(
    db: &D,
    name: &str,
    kind: Option<StoredNodeKind>,
) -> Result<Vec<SymbolLookup>, GraphStoreError> {
    if matches!(
        kind,
        Some(
            StoredNodeKind::File
                | StoredNodeKind::Repository
                | StoredNodeKind::Directory
                | StoredNodeKind::Area
                | StoredNodeKind::Doc
                | StoredNodeKind::Config
                | StoredNodeKind::Unresolved
        )
    ) {
        return Ok(Vec::new());
    }

    let key = symbol_index_key(name);
    let ids = {
        let txn = db.begin_read()?;
        let t = txn.open_multimap_table(SYMBOL_BY_NAME)?;
        let mut ids = BTreeSet::new();
        for row in t.get(key.as_str())? {
            ids.insert(row?.value().to_string());
        }
        ids
    };

    let txn = db.begin_read()?;
    let mut out = Vec::new();
    for id in ids {
        let Some(node) = get_node_in_txn(&txn, &id)? else {
            continue;
        };
        if let Some(expected) = kind
            && node.kind() != expected
        {
            continue;
        }
        if let Some(symbol) = symbol_lookup_from_node(node) {
            out.push(symbol);
        }
    }
    out.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(out)
}

pub(super) fn merge_symbol_candidate(
    candidates: &mut BTreeMap<String, SymbolCandidate>,
    symbol: SymbolLookup,
    signals: SymbolMatchSignals,
) {
    candidates
        .entry(symbol.id.clone())
        .and_modify(|existing| existing.signals.merge(signals))
        .or_insert(SymbolCandidate {
            symbol,
            signals,
            rank: 0,
        });
}

pub(super) fn symbol_query_tokens(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for component in split_symbol_components(query) {
        let token = component.to_ascii_lowercase();
        if !token.is_empty() && !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens
}

pub(super) fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !value.is_empty() && !values.contains(&value) {
        values.push(value);
    }
}

pub(super) fn symbol_query_exact_variants(query: &str) -> Vec<String> {
    let trimmed = query.trim();
    let mut variants = Vec::new();
    push_unique_string(&mut variants, trimmed.to_string());

    let components = split_symbol_components(trimmed);
    if !components.is_empty() {
        push_unique_string(&mut variants, components.join("_"));
        push_unique_string(&mut variants, components.join(""));
        for component in components {
            push_unique_string(&mut variants, component);
        }
    }
    variants
}

pub(super) fn symbol_query_index_variants(query: &str, tokens: &[String]) -> Vec<String> {
    let mut variants = Vec::new();
    push_unique_string(&mut variants, symbol_index_key(query.trim()));
    if !tokens.is_empty() {
        push_unique_string(&mut variants, tokens.join("_"));
        push_unique_string(&mut variants, tokens.join(""));
        for token in tokens {
            push_unique_string(&mut variants, token.clone());
        }
    }
    variants
}

pub(super) fn shares_stem(left: &str, right: &str) -> bool {
    if left.len() < MIN_STEM_LEN || right.len() < MIN_STEM_LEN {
        return false;
    }
    let left_prefix = left.chars().take(MIN_STEM_LEN).collect::<String>();
    let right_prefix = right.chars().take(MIN_STEM_LEN).collect::<String>();
    left_prefix.eq_ignore_ascii_case(&right_prefix)
}

pub(super) fn basename_without_extension(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub(super) fn score_symbol_candidate(
    symbol: &SymbolLookup,
    query: &str,
    tokens: &[String],
    area_name: Option<&str>,
    mut signals: SymbolMatchSignals,
) -> (i32, SymbolMatchSignals) {
    let exact_variants = symbol_query_exact_variants(query);
    let index_variants = symbol_query_index_variants(query, tokens);
    let name_lower = symbol.name.to_ascii_lowercase();
    signals.exact |= exact_variants.contains(&symbol.name);
    signals.case_insensitive |= index_variants.contains(&name_lower);

    let component_lowers = split_symbol_components(symbol.name.as_str())
        .into_iter()
        .map(|component| component.to_ascii_lowercase())
        .collect::<Vec<_>>();

    let mut name_matched_tokens = Vec::new();
    for token in tokens {
        let component_match = component_lowers
            .iter()
            .any(|component| component == token || shares_stem(component, token));
        if component_match {
            signals.component = true;
        }
        if name_lower.starts_with(token)
            || component_lowers
                .iter()
                .any(|component| component.starts_with(token))
        {
            signals.prefix = true;
        }
        if (name_lower == *token || name_lower.contains(token.as_str()) || component_match)
            && !name_matched_tokens.contains(token)
        {
            name_matched_tokens.push(token.clone());
        }
    }

    let name_score = match name_matched_tokens.len() {
        0 => 0,
        matched => NAME_BASE + NAME_COMPOUND_PER_EXTRA * (matched as i32 - 1),
    };

    let path_lower = symbol.path.to_ascii_lowercase();
    let path_matched = tokens
        .iter()
        .filter(|token| token.len() >= MIN_STEM_LEN && path_lower.contains(token.as_str()))
        .count();
    if path_matched > 0 {
        signals.path = true;
    }
    let path_score = path_matched as i32 * PATH_PER_TOKEN;

    let area_matched = area_name
        .map(|area| {
            tokens
                .iter()
                .filter(|token| token.len() >= MIN_STEM_LEN && area.contains(token.as_str()))
                .count()
        })
        .unwrap_or(0);
    if area_matched > 0 {
        signals.area = true;
    }
    let area_score = area_matched as i32 * AREA_PER_TOKEN;

    let basename_lower = basename_without_extension(symbol.path.as_str());
    if !basename_lower.is_empty() && tokens.contains(&basename_lower) {
        signals.basename = true;
    }
    let basename_score = if signals.basename {
        BASENAME_EXACT_BONUS
    } else {
        0
    };

    let rank = name_score + path_score + area_score + basename_score;
    let fallback_rank = if rank == 0 {
        i32::from(signals.signal_count()) * 10
    } else {
        0
    };
    (rank + fallback_rank, signals)
}

pub(super) fn symbol_candidate_allowed(
    symbol: &SymbolLookup,
    options: &SymbolMatchOptions,
) -> bool {
    if let Some(kind) = options.kind
        && symbol.kind != kind
    {
        return false;
    }
    if let Some(prefix) = &options.path_prefix
        && !symbol.path.starts_with(prefix)
    {
        return false;
    }
    if let Some(area_id) = &options.area_id
        && symbol.area_id.as_deref() != Some(area_id.as_str())
    {
        return false;
    }
    true
}

pub(super) fn add_symbol_candidates_for_ids(
    txn: &ReadTransaction,
    ids: BTreeSet<String>,
    candidates: &mut BTreeMap<String, SymbolCandidate>,
    signals: SymbolMatchSignals,
    options: &SymbolMatchOptions,
) -> Result<(), GraphStoreError> {
    for id in ids {
        let Some(node) = get_node_in_txn(txn, &id)? else {
            continue;
        };
        let Some(symbol) = symbol_lookup_from_node(node) else {
            continue;
        };
        if symbol_candidate_allowed(&symbol, options) {
            merge_symbol_candidate(candidates, symbol, signals);
        }
        if candidates.len() >= options.limit.saturating_mul(4).max(options.limit) {
            break;
        }
    }
    Ok(())
}

pub(super) fn collect_symbol_ids_for_exact_name<D: ReadableDatabase>(
    db: &D,
    key: &str,
) -> Result<BTreeSet<String>, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(SYMBOL_BY_NAME)?;
    let mut ids = BTreeSet::new();
    for row in t.get(key)? {
        ids.insert(row?.value().to_string());
    }
    Ok(ids)
}

pub(super) fn collect_symbol_ids_for_name_prefix<D: ReadableDatabase>(
    db: &D,
    prefix: &str,
    limit: usize,
) -> Result<BTreeSet<String>, GraphStoreError> {
    if limit == 0 {
        return Ok(BTreeSet::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(SYMBOL_BY_NAME)?;
    let end = prefix_end(prefix);
    let mut ids = BTreeSet::new();
    for entry in t.range(prefix..end.as_str())? {
        let (key, values) = entry?;
        if !key.value().starts_with(prefix) {
            continue;
        }
        for value in values {
            ids.insert(value?.value().to_string());
            if ids.len() >= limit {
                return Ok(ids);
            }
        }
    }
    Ok(ids)
}

pub(super) fn collect_symbol_ids_for_component<D: ReadableDatabase>(
    db: &D,
    component: &str,
    limit: usize,
) -> Result<BTreeSet<String>, GraphStoreError> {
    if limit == 0 {
        return Ok(BTreeSet::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(SYMBOL_BY_COMPONENT)?;
    let mut ids = BTreeSet::new();
    for row in t.get(component)? {
        ids.insert(row?.value().to_string());
        if ids.len() >= limit {
            break;
        }
    }
    Ok(ids)
}

pub(super) fn collect_symbol_ids_for_component_prefix<D: ReadableDatabase>(
    db: &D,
    prefix: &str,
    limit: usize,
) -> Result<BTreeSet<String>, GraphStoreError> {
    if limit == 0 {
        return Ok(BTreeSet::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(SYMBOL_BY_COMPONENT)?;
    let end = prefix_end(prefix);
    let mut ids = BTreeSet::new();
    for entry in t.range(prefix..end.as_str())? {
        let (key, values) = entry?;
        if !key.value().starts_with(prefix) {
            continue;
        }
        for value in values {
            ids.insert(value?.value().to_string());
            if ids.len() >= limit {
                return Ok(ids);
            }
        }
    }
    Ok(ids)
}

pub(super) fn collect_symbol_ids_for_path_component<D: ReadableDatabase>(
    db: &D,
    component: &str,
    limit: usize,
) -> Result<BTreeSet<String>, GraphStoreError> {
    if limit == 0 {
        return Ok(BTreeSet::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(SYMBOL_BY_PATH_COMPONENT)?;
    let mut ids = BTreeSet::new();
    for row in t.get(component)? {
        ids.insert(row?.value().to_string());
        if ids.len() >= limit {
            break;
        }
    }
    Ok(ids)
}

pub(super) fn collect_symbol_ids_for_path_component_prefix<D: ReadableDatabase>(
    db: &D,
    prefix: &str,
    limit: usize,
) -> Result<BTreeSet<String>, GraphStoreError> {
    if limit == 0 {
        return Ok(BTreeSet::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(SYMBOL_BY_PATH_COMPONENT)?;
    let end = prefix_end(prefix);
    let mut ids = BTreeSet::new();
    for entry in t.range(prefix..end.as_str())? {
        let (key, values) = entry?;
        if !key.value().starts_with(prefix) {
            continue;
        }
        for value in values {
            ids.insert(value?.value().to_string());
            if ids.len() >= limit {
                return Ok(ids);
            }
        }
    }
    Ok(ids)
}

pub(super) fn stem_prefix(token: &str) -> Option<String> {
    if token.len() < MIN_STEM_LEN {
        return None;
    }
    Some(token.chars().take(MIN_STEM_LEN).collect())
}

pub(super) fn symbols_matching_from<D: ReadableDatabase>(
    db: &D,
    query: &str,
    options: SymbolMatchOptions,
) -> Result<Vec<SymbolCandidate>, GraphStoreError> {
    let trimmed = query.trim();
    let normalized = symbol_index_key(trimmed);
    let tokens = symbol_query_tokens(trimmed);
    let index_variants = symbol_query_index_variants(trimmed, &tokens);
    if normalized.is_empty() || options.limit == 0 {
        return Ok(Vec::new());
    }

    let mut candidates = BTreeMap::new();
    let txn = db.begin_read()?;

    for variant in &index_variants {
        add_symbol_candidates_for_ids(
            &txn,
            collect_symbol_ids_for_exact_name(db, variant)?,
            &mut candidates,
            SymbolMatchSignals {
                case_insensitive: true,
                ..SymbolMatchSignals::default()
            },
            &options,
        )?;
        add_symbol_candidates_for_ids(
            &txn,
            collect_symbol_ids_for_name_prefix(db, variant, options.limit.saturating_mul(2))?,
            &mut candidates,
            SymbolMatchSignals {
                prefix: true,
                ..SymbolMatchSignals::default()
            },
            &options,
        )?;
    }

    for token in &tokens {
        add_symbol_candidates_for_ids(
            &txn,
            collect_symbol_ids_for_component(db, token, options.limit.saturating_mul(2))?,
            &mut candidates,
            SymbolMatchSignals {
                component: true,
                ..SymbolMatchSignals::default()
            },
            &options,
        )?;
        if let Some(stem) = stem_prefix(token) {
            add_symbol_candidates_for_ids(
                &txn,
                collect_symbol_ids_for_component_prefix(
                    db,
                    &stem,
                    options.limit.saturating_mul(3),
                )?,
                &mut candidates,
                SymbolMatchSignals {
                    component: true,
                    ..SymbolMatchSignals::default()
                },
                &options,
            )?;
        }
        add_symbol_candidates_for_ids(
            &txn,
            collect_symbol_ids_for_path_component(db, token, options.limit.saturating_mul(3))?,
            &mut candidates,
            SymbolMatchSignals {
                path: true,
                ..SymbolMatchSignals::default()
            },
            &options,
        )?;
        if let Some(stem) = stem_prefix(token) {
            add_symbol_candidates_for_ids(
                &txn,
                collect_symbol_ids_for_path_component_prefix(
                    db,
                    &stem,
                    options.limit.saturating_mul(3),
                )?,
                &mut candidates,
                SymbolMatchSignals {
                    path: true,
                    ..SymbolMatchSignals::default()
                },
                &options,
            )?;
        }
    }

    let path_ids =
        ids_under_path_limited_from(db, NODES_BY_PATH, trimmed, options.limit.saturating_mul(3))?
            .into_iter()
            .collect::<BTreeSet<_>>();
    add_symbol_candidates_for_ids(
        &txn,
        path_ids,
        &mut candidates,
        SymbolMatchSignals {
            path: true,
            ..SymbolMatchSignals::default()
        },
        &options,
    )?;

    let areas = list_areas_from(db, None)?;
    for area in &areas {
        let area_name = symbol_index_key(&area.name);
        let area_path = symbol_index_key(&area.path_prefix);
        let area_matches = index_variants.iter().any(|variant| {
            area_name == *variant
                || area_name.starts_with(variant)
                || area_path == *variant
                || area_path.starts_with(variant)
                || area_path.contains(variant)
        }) || tokens.iter().any(|token| {
            token.len() >= MIN_STEM_LEN && (area_name.contains(token) || area_path.contains(token))
        }) || area.id.eq_ignore_ascii_case(trimmed);

        if area_matches {
            let area_ids = ids_under_path_limited_from(
                db,
                NODES_BY_PATH,
                &area.path_prefix,
                options.limit.saturating_mul(3),
            )?
            .into_iter()
            .collect::<BTreeSet<_>>();
            add_symbol_candidates_for_ids(
                &txn,
                area_ids,
                &mut candidates,
                SymbolMatchSignals {
                    area: true,
                    ..SymbolMatchSignals::default()
                },
                &options,
            )?;
        }
    }

    let area_names = areas
        .into_iter()
        .map(|area| (area.id, area.name.to_ascii_lowercase()))
        .collect::<BTreeMap<_, _>>();
    let mut out = candidates.into_values().collect::<Vec<_>>();
    for candidate in &mut out {
        let area_name = candidate
            .symbol
            .area_id
            .as_ref()
            .and_then(|area_id| area_names.get(area_id).map(String::as_str));
        let (rank, signals) = score_symbol_candidate(
            &candidate.symbol,
            trimmed,
            &tokens,
            area_name,
            candidate.signals,
        );
        candidate.rank = rank;
        candidate.signals = signals;
    }
    out.sort_by(|left, right| {
        right
            .rank
            .cmp(&left.rank)
            .then_with(|| right.signals.exact.cmp(&left.signals.exact))
            .then_with(|| {
                right
                    .signals
                    .case_insensitive
                    .cmp(&left.signals.case_insensitive)
            })
            .then_with(|| right.signals.prefix.cmp(&left.signals.prefix))
            .then_with(|| right.signals.component.cmp(&left.signals.component))
            .then_with(|| right.signals.path.cmp(&left.signals.path))
            .then_with(|| right.signals.area.cmp(&left.signals.area))
            .then_with(|| right.signals.basename.cmp(&left.signals.basename))
            .then_with(|| {
                right
                    .signals
                    .signal_count()
                    .cmp(&left.signals.signal_count())
            })
            .then_with(|| left.symbol.path.cmp(&right.symbol.path))
            .then_with(|| left.symbol.line.cmp(&right.symbol.line))
            .then_with(|| left.symbol.name.cmp(&right.symbol.name))
            .then_with(|| left.symbol.id.cmp(&right.symbol.id))
    });
    out.truncate(options.limit);
    Ok(out)
}

pub(super) fn merge_task_anchor_candidate(
    candidates: &mut BTreeMap<String, TaskAnchorCandidate>,
    node: NodeDisplay,
    token: &str,
    signals: SymbolMatchSignals,
) {
    candidates
        .entry(node.id.clone())
        .and_modify(|existing| {
            existing.signals.merge(signals);
            if !existing.matched_tokens.iter().any(|value| value == token) {
                existing.matched_tokens.push(token.to_string());
                existing.matched_tokens.sort();
            }
        })
        .or_insert_with(|| TaskAnchorCandidate {
            node,
            signals,
            matched_tokens: vec![token.to_string()],
        });
}

pub(super) fn task_anchor_candidates_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    task_tokens: &[S],
    limit: usize,
) -> Result<Vec<TaskAnchorCandidate>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut candidates = BTreeMap::new();
    for token in task_tokens {
        let token = token.as_ref().trim();
        if token.len() < 2 {
            continue;
        }
        for symbol in symbols_matching_from(
            db,
            token,
            SymbolMatchOptions {
                limit,
                ..SymbolMatchOptions::default()
            },
        )? {
            let node = NodeDisplay {
                id: symbol.symbol.id.clone(),
                kind: symbol.symbol.kind,
                display: format!("{}::{}", symbol.symbol.path, symbol.symbol.name),
                name: symbol.symbol.name,
                path: Some(symbol.symbol.path),
                language: Some(symbol.symbol.language),
                area_id: symbol.symbol.area_id,
            };
            merge_task_anchor_candidate(&mut candidates, node, token, symbol.signals);
        }

        let path_ids = ids_under_path_limited_from(db, NODES_BY_PATH, token, limit)?;
        let txn = db.begin_read()?;
        for id in path_ids {
            let Some(node) = display_from_id_in_txn(&txn, &id)? else {
                continue;
            };
            merge_task_anchor_candidate(
                &mut candidates,
                node,
                token,
                SymbolMatchSignals {
                    path: true,
                    ..SymbolMatchSignals::default()
                },
            );
        }

        for area in list_areas_from(db, None)? {
            if area.name.eq_ignore_ascii_case(token)
                || area.path_prefix.eq_ignore_ascii_case(token)
                || area.path_prefix.starts_with(token)
            {
                merge_task_anchor_candidate(
                    &mut candidates,
                    NodeDisplay {
                        id: area.id.clone(),
                        kind: StoredNodeKind::Area,
                        display: area.path_prefix.clone(),
                        name: area.name.clone(),
                        path: Some(area.path_prefix.clone()),
                        language: None,
                        area_id: Some(area.id),
                    },
                    token,
                    SymbolMatchSignals {
                        area: true,
                        ..SymbolMatchSignals::default()
                    },
                );
            }
        }
    }

    let mut out = candidates.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        right
            .matched_tokens
            .len()
            .cmp(&left.matched_tokens.len())
            .then_with(|| {
                right
                    .signals
                    .signal_count()
                    .cmp(&left.signals.signal_count())
            })
            .then_with(|| left.node.display.cmp(&right.node.display))
            .then_with(|| left.node.id.cmp(&right.node.id))
    });
    out.truncate(limit);
    Ok(out)
}

pub(super) fn usage_boundary_candidates_from<D: ReadableDatabase>(
    db: &D,
    scope: &str,
    symbol_kind: Option<StoredNodeKind>,
    limit: usize,
) -> Result<Vec<UsageBoundaryCandidate>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let ids = ids_under_path_limited_from(db, NODES_BY_PATH, scope, limit.saturating_mul(4))?;
    let txn = db.begin_read()?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for id in ids {
        let Some(node) = get_node_in_txn(&txn, &id)? else {
            continue;
        };
        let kind = node.kind();
        let include = match symbol_kind {
            Some(expected) => kind == expected,
            None => {
                matches!(
                    kind,
                    StoredNodeKind::Function | StoredNodeKind::Class | StoredNodeKind::File
                ) || is_surface_stored_kind(kind)
            }
        };
        if !include {
            continue;
        }
        let symbol = symbol_lookup_from_node(node.clone());
        let display = node_display_from_node(node);
        if seen.insert(display.id.clone()) {
            out.push(UsageBoundaryCandidate {
                node: display,
                symbol,
            });
        }
        if out.len() >= limit {
            break;
        }
    }
    out.sort_by(|left, right| {
        left.node
            .path
            .cmp(&right.node.path)
            .then_with(|| left.node.kind.cmp(&right.node.kind))
            .then_with(|| left.node.display.cmp(&right.node.display))
            .then_with(|| left.node.id.cmp(&right.node.id))
    });
    Ok(out)
}
