use std::env;
use std::io::Write;
use std::path::PathBuf;

use aethyme_engine::graph::activation::{hormone_profile, spread_activation, spread_from_seed};
use aethyme_engine::graph::anchors::resolve_anchors;
use aethyme_engine::map::RepositoryMap;
use aethyme_engine::graph::neighborhood::{dependency_frontier, impact_frontier};
use aethyme_engine::graph::navigation::{
    callers_view, callees_view, children_view, configs_view, docs_view, graph_expand_view,
    graph_overview_view, node_view, parents_view, task_anchors_view, task_expand_view, task_next_view,
    task_scope_view,
};
use aethyme_engine::graph::overview::build_repo_overview;
use aethyme_engine::pipeline::{build_context_pack, build_context_pack_with_content};
use aethyme_engine::graph::search::symbol_search;
use aethyme_engine::model::task::TaskInput;
use aethyme_engine::workspace::{build_workspace_graph, cross_repo_blast_radius};

fn main() {
    if let Err(message) = run() {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        return Err("missing command".to_string());
    }

    let no_cache = has_flag(&args, "--no-cache");
    let command = args.remove(0);
    match command.as_str() {
        "inspect" => {
            let repo = read_option(&args, "--repo")?;
            let mode = read_option(&args, "--mode").unwrap_or_else(|_| "full".to_string());
            let map = build_map(&repo, no_cache)?;
            let mut stdout = std::io::stdout().lock();
            match mode.as_str() {
                "brief" => writeln!(stdout, "{}", aethyme_engine::json::inspect_brief(&map)).map_err(|e| e.to_string())?,
                "structure" => {
                    aethyme_engine::json::write_inspect_structure(&mut stdout, &map).map_err(|e| e.to_string())?;
                    writeln!(stdout).map_err(|e| e.to_string())?;
                }
                "full" => {
                    aethyme_engine::json::write_repository_map(&mut stdout, &map).map_err(|e| e.to_string())?;
                    writeln!(stdout).map_err(|e| e.to_string())?;
                }
                other => return Err(format!("unsupported inspect mode: {other}")),
            }
        }
        "build-profile" => {
            let repo = read_option(&args, "--repo")?;
            let (_map, profile) = if no_cache {
                RepositoryMap::build_no_cache(&PathBuf::from(&repo))?
            } else {
                RepositoryMap::build_with_profile_and_progress(
                    &PathBuf::from(&repo),
                    |stage| eprintln!("stage={} duration_ms={}", stage.name, stage.duration_ms),
                )?
            };
            println!("{}", aethyme_engine::json::build_profile(&profile));
        }
        "symbol" => {
            let repo = read_option(&args, "--repo")?;
            let query = read_option(&args, "--query")?;
            let map = build_map(&repo, no_cache)?;
            let hits = symbol_search(&map, &query, 20);
            println!("{}", aethyme_engine::json::search_hits(&hits));
        }
        "graph-node" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache)?;
            let view = node_view(&map, &target).ok_or_else(|| format!("node not found: {target}"))?;
            println!("{}", aethyme_engine::json::graph_node_view(&view));
        }
        "graph-children" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache)?;
            println!("{}", aethyme_engine::json::graph_relation(&children_view(&map, &target)));
        }
        "graph-parents" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache)?;
            println!("{}", aethyme_engine::json::graph_relation(&parents_view(&map, &target)));
        }
        "graph-callers" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache)?;
            println!("{}", aethyme_engine::json::graph_relation(&callers_view(&map, &target)));
        }
        "graph-callees" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache)?;
            println!("{}", aethyme_engine::json::graph_relation(&callees_view(&map, &target)));
        }
        "graph-docs" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache)?;
            println!("{}", aethyme_engine::json::graph_relation(&docs_view(&map, &target)));
        }
        "graph-configs" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache)?;
            println!("{}", aethyme_engine::json::graph_relation(&configs_view(&map, &target)));
        }
        "graph-expand" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache)?;
            let view = graph_expand_view(&map, &target).ok_or_else(|| format!("node not found: {target}"))?;
            println!("{}", aethyme_engine::json::graph_expand_view(&view));
        }
        "graph-overview" => {
            let repo = read_option(&args, "--repo")?;
            let map = build_map(&repo, no_cache)?;
            println!("{}", aethyme_engine::json::repo_overview_view(&graph_overview_view(&map)));
        }
        "deps" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache)?;
            let deps = dependency_frontier(&map, &target);
            println!("{}", aethyme_engine::json::string_list(&deps));
        }
        "impact" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache)?;
            let impact = impact_frontier(&map, &target);
            println!("{}", aethyme_engine::json::string_list(&impact));
        }
        "pack" => {
            let repo = read_option(&args, "--repo")?;
            let task_value = read_option(&args, "--task")?;
            let root = PathBuf::from(&repo);
            let map = build_map(&repo, no_cache)?;
            let task = TaskInput::from_task_text(&task_value);
            let pack = build_context_pack(&root, &map, task);
            let mut stdout = std::io::stdout().lock();
            aethyme_engine::json::write_context_pack(&mut stdout, &pack).map_err(|e| e.to_string())?;
            writeln!(stdout).map_err(|e| e.to_string())?;
        }
        "context" => {
            let repo = read_option(&args, "--repo")?;
            let task_value = read_option(&args, "--task")?;
            let content_budget: usize = read_option(&args, "--content-budget")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(80_000);
            let root = PathBuf::from(&repo);
            let map = build_map(&repo, no_cache)?;
            let task = TaskInput::from_task_text(&task_value);
            let pack = build_context_pack_with_content(&root, &map, task, content_budget);
            let mut stdout = std::io::stdout().lock();
            aethyme_engine::json::write_context_pack(&mut stdout, &pack).map_err(|e| e.to_string())?;
            writeln!(stdout).map_err(|e| e.to_string())?;
        }
        "task-anchors" => {
            let repo = read_option(&args, "--repo")?;
            let map = build_map(&repo, no_cache)?;
            let task_value = read_option(&args, "--task")?;
            let task = TaskInput::from_task_text(&task_value);
            println!("{}", aethyme_engine::json::task_anchors_view(&task_anchors_view(&map, &task)));
        }
        "task-scope" => {
            let repo = read_option(&args, "--repo")?;
            let map = build_map(&repo, no_cache)?;
            let task_value = read_option(&args, "--task")?;
            let task = TaskInput::from_task_text(&task_value);
            println!("{}", aethyme_engine::json::task_scope_view(&task_scope_view(&map, &task)));
        }
        "task-next" => {
            let repo = read_option(&args, "--repo")?;
            let map = build_map(&repo, no_cache)?;
            let task_value = read_option(&args, "--task")?;
            let task = TaskInput::from_task_text(&task_value);
            println!("{}", aethyme_engine::json::graph_relation(&task_next_view(&map, &task)));
        }
        "task-expand" => {
            let repo = read_option(&args, "--repo")?;
            let map = build_map(&repo, no_cache)?;
            let target = read_option(&args, "--target")?;
            println!("{}", aethyme_engine::json::task_expand_view(&task_expand_view(&map, &target)));
        }
        "explain" => {
            let repo = read_option(&args, "--repo")?;
            let task_value = read_option(&args, "--task").unwrap_or_else(|_| "Explain this repo".to_string());
            let root = PathBuf::from(&repo);
            let map = build_map(&repo, no_cache)?;
            let task = TaskInput::from_task_text(&task_value);
            let pack = build_context_pack(&root, &map, task);
            print_explanation(&map, &pack);
        }
        "workspace-inspect" => {
            let repos_str = read_option(&args, "--repos")?;
            let repo_paths: Vec<PathBuf> = repos_str.split(',').map(PathBuf::from).collect();
            let path_refs: Vec<&std::path::Path> = repo_paths.iter().map(|p| p.as_path()).collect();
            let graph = build_workspace_graph(&path_refs)?;
            println!("{}", aethyme_engine::json::workspace_graph(&graph));
        }
        "activate" => {
            let repo = read_option(&args, "--repo")?;
            let task_value = read_option(&args, "--task")?;
            let map = build_map(&repo, no_cache)?;
            let task = TaskInput::from_task_text(&task_value);
            let anchor_limit = if task.kind.is_explain_repo() { 5 } else { 3 };
            let anchors = resolve_anchors(&map, &task, anchor_limit);
            let profile = hormone_profile(&task.kind);
            let activation = spread_activation(&map, &anchors, &profile);
            println!("{}", aethyme_engine::json::activation_map(&activation));
        }
        "activate-from" => {
            let repo = read_option(&args, "--repo")?;
            let seed = read_option(&args, "--seed")?;
            let hops = read_option(&args, "--hops").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(3);
            let map = build_map(&repo, no_cache)?;
            let activation = spread_from_seed(&map, &seed, hops);
            println!("{}", aethyme_engine::json::activation_map(&activation));
        }
        "workspace-blast-radius" => {
            let repos_str = read_option(&args, "--repos")?;
            let target_repo = read_option(&args, "--repo")?;
            let file = read_option(&args, "--file")?;
            let repo_paths: Vec<PathBuf> = repos_str.split(',').map(PathBuf::from).collect();
            let path_refs: Vec<&std::path::Path> = repo_paths.iter().map(|p| p.as_path()).collect();
            let graph = build_workspace_graph(&path_refs)?;
            let items = cross_repo_blast_radius(&graph, &target_repo, &file);
            println!("{}", aethyme_engine::json::blast_radius(&items));
        }
        "warm" => {
            let repo = read_option(&args, "--repo")?;
            let _map = build_map(&repo, no_cache)?;
            eprintln!("map cached");
        }
        "index" => {
            let repo = read_option(&args, "--repo")?;
            eprintln!("Building repository map...");
            let map = build_map(&repo, no_cache)?;
            eprintln!(
                "Map built: {} areas, {} files, {} functions, {} classes, {} edges",
                map.areas.len(),
                map.files.len(),
                map.functions.len(),
                map.classes.len(),
                map.edges.len(),
            );
            eprintln!("Writing to SurrealDB store...");
            let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            rt.block_on(async {
                index_to_store(&PathBuf::from(&repo), &map).await
            })?;
            eprintln!("Generating Chau7 snippets...");
            let canonical = PathBuf::from(&repo).canonicalize().map_err(|e| e.to_string())?;
            aethyme_engine::store::snippets::generate_and_write(&canonical, &map)?;
            eprintln!("Snippets written to {}/.chau7/snippets.json", canonical.display());
        }
        "prompt" => {
            let repo = read_option(&args, "--repo")?;
            let task = read_option(&args, "--task").unwrap_or_else(|_| "Explain this repository".to_string());
            let focus = read_option(&args, "--focus").ok();
            let map = build_map(&repo, no_cache)?;
            let canonical = PathBuf::from(&repo).canonicalize().map_err(|e| e.to_string())?;
            let prompt = aethyme_engine::store::prompt::generate_prompt(
                &canonical,
                &map,
                &task,
                focus.as_deref(),
            );
            println!("{prompt}");
        }
        "query-areas" => {
            let repo = read_option(&args, "--repo")?;
            let depth = read_option(&args, "--depth").ok().and_then(|v| v.parse::<u32>().ok());
            let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            rt.block_on(async {
                let store = aethyme_engine::store::GraphStore::open(&PathBuf::from(&repo).canonicalize().unwrap())
                    .await.map_err(|e| e.to_string())?;
                let areas = aethyme_engine::store::read::list_areas(store.db(), depth)
                    .await.map_err(|e| e.to_string())?;
                println!("{}", serde_json::to_string_pretty(&areas).unwrap());
                Ok::<(), String>(())
            })?;
        }
        "query-file" => {
            let repo = read_option(&args, "--repo")?;
            let file = read_option(&args, "--file")?;
            let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            rt.block_on(async {
                let store = aethyme_engine::store::GraphStore::open(&PathBuf::from(&repo).canonicalize().unwrap())
                    .await.map_err(|e| e.to_string())?;
                let info = aethyme_engine::store::read::file_info(store.db(), &file)
                    .await.map_err(|e| e.to_string())?;
                match info {
                    Some(f) => println!("{}", serde_json::to_string_pretty(&f).unwrap()),
                    None => println!("null"),
                }
                Ok::<(), String>(())
            })?;
        }
        "query-edges" => {
            let repo = read_option(&args, "--repo")?;
            let from = read_option(&args, "--from")?;
            let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            rt.block_on(async {
                let store = aethyme_engine::store::GraphStore::open(&PathBuf::from(&repo).canonicalize().unwrap())
                    .await.map_err(|e| e.to_string())?;
                let edges = aethyme_engine::store::read::edges_from(store.db(), &from)
                    .await.map_err(|e| e.to_string())?;
                println!("{}", serde_json::to_string_pretty(&edges).unwrap());
                Ok::<(), String>(())
            })?;
        }
        "query-overview" => {
            let repo = read_option(&args, "--repo")?;
            let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            rt.block_on(async {
                let store = aethyme_engine::store::GraphStore::open(&PathBuf::from(&repo).canonicalize().unwrap())
                    .await.map_err(|e| e.to_string())?;
                let overview = aethyme_engine::store::read::overview(store.db())
                    .await.map_err(|e| e.to_string())?;
                println!("{}", serde_json::to_string_pretty(&overview).unwrap());
                Ok::<(), String>(())
            })?;
        }
        other => return Err(format!("unsupported command: {other}")),
    }
    Ok(())
}

fn read_option(args: &[String], flag: &str) -> Result<String, String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .ok_or_else(|| format!("missing required option: {flag}"))
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn build_map(repo: &str, no_cache: bool) -> Result<RepositoryMap, String> {
    if no_cache {
        RepositoryMap::build_no_cache(&PathBuf::from(repo)).map(|(map, _)| map)
    } else {
        RepositoryMap::build(&PathBuf::from(repo))
    }
}

fn print_explanation(map: &RepositoryMap, pack: &aethyme_engine::context_pack::ContextPack) {
    let overview = build_repo_overview(map, &pack.navigation_order);
    println!("Task: {}", pack.task.raw);
    println!("Languages: {}", map.snapshot.languages.join(", "));
    println!("Top-level directories: {}", map.snapshot.top_level_dirs.join(", "));
    println!("Files indexed: {}", map.snapshot.files.len());
    println!("Functions indexed: {}", map.functions.len());
    println!("Classes indexed: {}", map.classes.len());
    println!("Docs indexed: {}", map.docs.len());
    println!("Configs indexed: {}", map.configs.len());
    if let Some(readme) = &map.snapshot.readme_path {
        println!("README: {readme}");
    }
    if !overview.code_areas.is_empty() {
        println!("Code areas: {}", overview.code_areas.join(", "));
    }
    if !overview.reference_areas.is_empty() {
        println!("Reference areas: {}", overview.reference_areas.join(", "));
    }
    if !overview.subareas.is_empty() {
        println!("Key subareas: {}", overview.subareas.join(", "));
    }
    if !overview.key_configs.is_empty() {
        println!("Key configs: {}", overview.key_configs.join(", "));
    }
    if !overview.entrypoints.is_empty() {
        println!("Entrypoints: {}", overview.entrypoints.join(", "));
    }
    if !overview.representative_code_files.is_empty() {
        println!(
            "Representative code: {}",
            overview.representative_code_files.join(", ")
        );
    }
    if !overview.representative_docs.is_empty() {
        println!("Representative docs: {}", overview.representative_docs.join(", "));
    }
    println!("\nNavigation order:");
    for step in &pack.navigation_order {
        println!("- {step}");
    }
    if !pack.risk_flags.is_empty() {
        println!("\nHigh-risk areas:");
        for risk in &pack.risk_flags {
            println!("- {} ({:?}): {}", risk.scope, risk.area, risk.reason);
        }
    }
    if !pack.out_of_scope.areas.is_empty() {
        println!("\nOut of scope:");
        for area in &pack.out_of_scope.areas {
            println!("- {}: {}", area.value, area.reason);
        }
    }
}

async fn index_to_store(repo_root: &std::path::Path, map: &RepositoryMap) -> Result<(), String> {
    use aethyme_engine::store;

    let canonical = repo_root.canonicalize().map_err(|e| e.to_string())?;
    let store = store::GraphStore::open(&canonical).await.map_err(|e| e.to_string())?;
    store.ensure_schema().await.map_err(|e| e.to_string())?;
    store.reset().await.map_err(|e| e.to_string())?;

    // Areas
    for area in &map.areas {
        store::write::insert_area(store.db(), area).await.map_err(|e| e.to_string())?;
    }
    eprintln!("  areas: {}", map.areas.len());

    // Files
    for file in &map.files {
        store::write::insert_file(store.db(), file).await.map_err(|e| e.to_string())?;
    }
    eprintln!("  files: {}", map.files.len());

    // Symbols
    for symbol in &map.symbols {
        store::write::insert_symbol(store.db(), symbol).await.map_err(|e| e.to_string())?;
    }
    eprintln!("  symbols: {}", map.symbols.len());

    // Edges
    for edge in &map.edges {
        let _ = store::write::insert_edge(store.db(), edge).await;
        // Ignore individual edge insert failures (some edges may reference unknown nodes)
    }
    eprintln!("  edges: {}", map.edges.len());

    // Risk flags
    for risk in &map.risk_flags {
        store::write::insert_risk(store.db(), risk).await.map_err(|e| e.to_string())?;
    }
    eprintln!("  risks: {}", map.risk_flags.len());

    // Repo metadata
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&canonical)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());

    store.db().query(
        "CREATE repo SET root_path = $root, commit_hash = $commit, \
         indexed_at = time::now(), file_count = $fc, languages = $langs"
    )
        .bind(("root", canonical.to_string_lossy().to_string()))
        .bind(("commit", commit))
        .bind(("fc", map.files.len() as i64))
        .bind(("langs", Some(map.snapshot.languages.clone())))
        .await
        .map_err(|e| e.to_string())?;

    let db_path = canonical.join(".aethyme").join("graph.db");
    eprintln!("Store written to: {}", db_path.display());
    Ok(())
}
