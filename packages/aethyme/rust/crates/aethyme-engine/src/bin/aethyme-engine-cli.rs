use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use aethyme_engine::graph::activation::{hormone_profile, spread_activation, spread_from_seed};
use aethyme_engine::graph::analyzers::analyze_dead_code;
use aethyme_engine::graph::anchors::resolve_anchors;
use aethyme_engine::graph::facts::{function_usage_fact, public_function_facts};
use aethyme_engine::graph::navigation::{
    callees_view, callers_view, children_view, configs_view, docs_view, graph_expand_view,
    graph_overview_view, node_view, parents_view, task_anchors_view, task_expand_view,
    task_next_view, task_scope_view,
};
use aethyme_engine::graph::neighborhood::{dependency_frontier, impact_frontier};
use aethyme_engine::graph::overview::build_repo_overview;
use aethyme_engine::graph::search::symbol_search;
use aethyme_engine::graph::usage_boundary::analyze_usage_boundary_scope_first;
use aethyme_engine::map::RepositoryMap;
use aethyme_engine::model::task::TaskInput;
use aethyme_engine::pipeline::{build_context_pack, build_context_pack_with_content};
use aethyme_engine::workspace::{build_workspace_graph, cross_repo_blast_radius};

#[derive(Clone, Copy)]
enum FragmentBuildMode {
    /// Use committed fragments. The legacy pass pipeline was removed
    /// in 4.7.12, so missing fragments are a build error.
    Prefer,
    /// Legacy diagnostic spelling; equivalent to `Prefer` after 4.7.12.
    Force,
}

impl FragmentBuildMode {
    fn from_flags(no_fragments: bool, legacy_from_fragments: bool) -> Result<Self, String> {
        if no_fragments && legacy_from_fragments {
            return Err(legacy_pass_removed_error());
        }
        if no_fragments {
            Err(legacy_pass_removed_error())
        } else if legacy_from_fragments {
            Ok(Self::Force)
        } else {
            Ok(Self::Prefer)
        }
    }

    fn forces_fragments(self) -> bool {
        matches!(self, Self::Force)
    }
}

fn main() {
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();

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
    let no_fragments = has_flag(&args, "--no-fragments");
    let legacy_from_fragments = has_flag(&args, "--from-fragments");
    // Phase 4.7.12: the on-disk graph is the only build source.
    // `--from-fragments` remains a compatibility spelling;
    // `--no-fragments` errors because the rollback path is gone.
    let fragment_mode = FragmentBuildMode::from_flags(no_fragments, legacy_from_fragments)?;
    let command = args.remove(0);
    match command.as_str() {
        "daemon" => return run_daemon_subcommand(&args, no_cache, fragment_mode),
        "explore" => return run_explore_via_shared_cli(&args),
        "inspect" => {
            let repo = read_option(&args, "--repo")?;
            let mode = read_option(&args, "--mode").unwrap_or_else(|_| "full".to_string());
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let mut stdout = std::io::stdout().lock();
            match mode.as_str() {
                "brief" => writeln!(stdout, "{}", aethyme_engine::json::inspect_brief(&map))
                    .map_err(|e| e.to_string())?,
                "structure" => {
                    aethyme_engine::json::write_inspect_structure(&mut stdout, &map)
                        .map_err(|e| e.to_string())?;
                    writeln!(stdout).map_err(|e| e.to_string())?;
                }
                "full" => {
                    aethyme_engine::json::write_repository_map(&mut stdout, &map)
                        .map_err(|e| e.to_string())?;
                    writeln!(stdout).map_err(|e| e.to_string())?;
                }
                other => return Err(format!("unsupported inspect mode: {other}")),
            }
        }
        "build-profile" => {
            let repo = read_option(&args, "--repo")?;
            let (_map, profile) = match fragment_mode {
                FragmentBuildMode::Force => {
                    RepositoryMap::build_from_fragments(&PathBuf::from(&repo))?
                }
                FragmentBuildMode::Prefer => RepositoryMap::build_with_fragment_preference(
                    &PathBuf::from(&repo),
                    no_cache,
                    |stage| eprintln!("stage={} duration_ms={}", stage.name, stage.duration_ms),
                )?,
            };
            println!("{}", aethyme_engine::json::build_profile(&profile));
        }
        "symbol" => {
            let repo = read_option(&args, "--repo")?;
            let query = read_option(&args, "--query")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let hits = symbol_search(&map, &query, 20);
            println!("{}", aethyme_engine::json::search_hits(&hits));
        }
        "symbol-batch" => {
            let repo = read_option(&args, "--repo")?;
            let queries = read_options(&args, "--query");
            if queries.is_empty() {
                return Err("missing required option: --query".to_string());
            }
            let limit = read_option(&args, "--limit")
                .ok()
                .and_then(|raw| raw.parse::<usize>().ok())
                .unwrap_or(20);
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let results = queries
                .iter()
                .map(|query| (query.clone(), symbol_search(&map, query, limit)))
                .collect::<Vec<_>>();
            println!("{}", aethyme_engine::json::search_hits_by_query(&results));
        }
        "graph-node" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let view =
                node_view(&map, &target).ok_or_else(|| format!("node not found: {target}"))?;
            println!("{}", aethyme_engine::json::graph_node_view(&view));
        }
        "graph-children" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            println!(
                "{}",
                aethyme_engine::json::graph_relation(&children_view(&map, &target))
            );
        }
        "graph-parents" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            println!(
                "{}",
                aethyme_engine::json::graph_relation(&parents_view(&map, &target))
            );
        }
        "graph-callers" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            println!(
                "{}",
                aethyme_engine::json::graph_relation(&callers_view(&map, &target))
            );
        }
        "graph-callees" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            println!(
                "{}",
                aethyme_engine::json::graph_relation(&callees_view(&map, &target))
            );
        }
        "graph-docs" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            println!(
                "{}",
                aethyme_engine::json::graph_relation(&docs_view(&map, &target))
            );
        }
        "graph-configs" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            println!(
                "{}",
                aethyme_engine::json::graph_relation(&configs_view(&map, &target))
            );
        }
        "graph-expand" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let view = graph_expand_view(&map, &target)
                .ok_or_else(|| format!("node not found: {target}"))?;
            println!("{}", aethyme_engine::json::graph_expand_view(&view));
        }
        "graph-overview" => {
            let repo = read_option(&args, "--repo")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            println!(
                "{}",
                aethyme_engine::json::repo_overview_view(&graph_overview_view(&map))
            );
        }
        "graph-deps" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let deps = dependency_frontier(&map, &target);
            println!("{}", aethyme_engine::json::string_list(&deps));
        }
        "impact" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let impact = impact_frontier(&map, &target);
            println!("{}", aethyme_engine::json::string_list(&impact));
        }
        "pack" => {
            let repo = read_option(&args, "--repo")?;
            let task_value = read_option(&args, "--task")?;
            let root = PathBuf::from(&repo);
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let task = TaskInput::from_task_text(&task_value);
            let pack = build_context_pack(&root, &map, task);
            let mut stdout = std::io::stdout().lock();
            aethyme_engine::json::write_context_pack(&mut stdout, &pack)
                .map_err(|e| e.to_string())?;
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
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let task = TaskInput::from_task_text(&task_value);
            let pack = build_context_pack_with_content(&root, &map, task, content_budget);
            let mut stdout = std::io::stdout().lock();
            aethyme_engine::json::write_context_pack(&mut stdout, &pack)
                .map_err(|e| e.to_string())?;
            writeln!(stdout).map_err(|e| e.to_string())?;
        }
        "task-anchors" => {
            let repo = read_option(&args, "--repo")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let task_value = read_option(&args, "--task")?;
            let task = TaskInput::from_task_text(&task_value);
            println!(
                "{}",
                aethyme_engine::json::task_anchors_view(&task_anchors_view(&map, &task))
            );
        }
        "task-scope" => {
            let repo = read_option(&args, "--repo")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let task_value = read_option(&args, "--task")?;
            let task = TaskInput::from_task_text(&task_value);
            println!(
                "{}",
                aethyme_engine::json::task_scope_view(&task_scope_view(&map, &task))
            );
        }
        "task-next" => {
            let repo = read_option(&args, "--repo")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let task_value = read_option(&args, "--task")?;
            let task = TaskInput::from_task_text(&task_value);
            println!(
                "{}",
                aethyme_engine::json::graph_relation(&task_next_view(&map, &task))
            );
        }
        "task-localize" => {
            let repo = read_option(&args, "--repo")?;
            let task_value = read_option(&args, "--task")?;
            let profile = has_flag(&args, "--profile");
            let mut profiler = StageProfiler::new("task-localize", profile);

            // Use build_with_profile so we can fold the fragments build
            // timing into our profiler output. `RepositoryMap::build` only
            // returned the map, so previously we could only see total
            // map_build time.
            let (map, build_profile) = profiler.stage("map_build", || match fragment_mode {
                FragmentBuildMode::Force => {
                    RepositoryMap::build_from_fragments(&PathBuf::from(&repo))
                }
                FragmentBuildMode::Prefer => RepositoryMap::build_with_fragment_preference(
                    &PathBuf::from(&repo),
                    no_cache,
                    |_| {},
                ),
            })?;
            if profile {
                profiler.attach_substages("map_build", &build_profile);
            }
            let task = profiler.stage_pure("task_parse", || TaskInput::from_task_text(&task_value));
            let anchors = profiler.stage_pure("anchors", || task_anchors_view(&map, &task));
            let scope = profiler.stage_pure("scope", || task_scope_view(&map, &task));
            let next = profiler.stage_pure("next", || task_next_view(&map, &task));
            let rendered = profiler.stage_pure("json_render", || {
                aethyme_engine::json::task_localization_view(&anchors, &scope, &next)
            });
            println!("{rendered}");
            profiler.report();
        }
        "task-expand" => {
            let repo = read_option(&args, "--repo")?;
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let target = read_option(&args, "--target")?;
            println!(
                "{}",
                aethyme_engine::json::task_expand_view(&task_expand_view(&map, &target))
            );
        }
        "explain" => {
            let repo = read_option(&args, "--repo")?;
            let task_value =
                read_option(&args, "--task").unwrap_or_else(|_| "Explain this repo".to_string());
            let root = PathBuf::from(&repo);
            let map = build_map(&repo, no_cache, fragment_mode)?;
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
            let map = build_map(&repo, no_cache, fragment_mode)?;
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
            let hops = read_option(&args, "--hops")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(3);
            let map = build_map(&repo, no_cache, fragment_mode)?;
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
        "facts-public-functions" => {
            let repo = read_option(&args, "--repo")?;
            let scope = read_option(&args, "--scope")?;
            let include_methods = has_flag(&args, "--include-methods");
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let facts = public_function_facts(&map, &scope, include_methods);
            println!(
                "{}",
                serde_json::to_string_pretty(&facts).map_err(|e| e.to_string())?
            );
        }
        "facts-function-usage" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let boundary = read_option(&args, "--boundary")?;
            let roots = read_option(&args, "--roots")
                .ok()
                .map(|value| {
                    value
                        .split(',')
                        .map(|item| item.trim().to_string())
                        .filter(|item| !item.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let fact = public_function_facts(&map, &boundary, true)
                .into_iter()
                .find(|fact| {
                    fact.id == target || fact.name == target || fact.qualified_name == target
                })
                .ok_or_else(|| format!("function fact not found for target: {target}"))?;
            let usage = function_usage_fact(&map, &fact, &boundary, &roots);
            println!(
                "{}",
                serde_json::to_string_pretty(&usage).map_err(|e| e.to_string())?
            );
        }
        "analyze-dead-code" => {
            let repo = read_option(&args, "--repo")?;
            let scope = read_option(&args, "--scope")?;
            let include_methods = has_flag(&args, "--include-methods");
            let roots = read_option(&args, "--roots")
                .ok()
                .map(|value| {
                    value
                        .split(',')
                        .map(|item| item.trim().to_string())
                        .filter(|item| !item.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let answer = analyze_dead_code(&map, &scope, &roots, include_methods);
            println!(
                "{}",
                serde_json::to_string_pretty(&answer).map_err(|e| e.to_string())?
            );
        }
        "analyze-usage-boundary" => {
            let repo = read_option(&args, "--repo")?;
            let scope = read_option(&args, "--scope")?;
            let include_methods = has_flag(&args, "--include-methods");
            let roots = read_option(&args, "--roots")
                .ok()
                .map(|value| {
                    value
                        .split(',')
                        .map(|item| item.trim().to_string())
                        .filter(|item| !item.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let budget_ms = read_option(&args, "--budget-ms")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(10_000);
            let max_evidence_per_symbol = read_option(&args, "--max-evidence-per-symbol")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(5);
            let answer = analyze_usage_boundary_scope_first(
                &PathBuf::from(&repo),
                &scope,
                &roots,
                include_methods,
                Some(budget_ms),
                max_evidence_per_symbol,
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&answer).map_err(|e| e.to_string())?
            );
        }
        "warm" => {
            let repo = read_option(&args, "--repo")?;
            let _map = build_map(&repo, no_cache, fragment_mode)?;
            eprintln!("map cached");
        }
        "index" => {
            let repo = read_option(&args, "--repo")?;
            let profile = has_flag(&args, "--profile");
            let compact = has_flag(&args, "--compact");
            let disposable_fast = has_flag(&args, "--disposable-fast");
            let mut profiler = StageProfiler::new("index", profile);
            eprintln!("Building repository map...");
            let (map, build_profile) = profiler.stage("map_build", || {
                build_map_with_profile(&repo, no_cache, fragment_mode)
            })?;
            if profile {
                profiler.attach_substages("map_build", &build_profile);
            }
            eprintln!(
                "Map built: {} areas, {} files, {} functions, {} classes, {} edges",
                map.areas.len(),
                map.files.len(),
                map.functions.len(),
                map.classes.len(),
                map.edges.len(),
            );
            eprintln!("Writing to redb graph store...");
            index_to_store(
                &PathBuf::from(&repo),
                &map,
                &mut profiler,
                compact,
                disposable_fast,
            )?;
            eprintln!("Generating Chau7 snippets...");
            let canonical = PathBuf::from(&repo)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            profiler.stage("snippet_generation", || {
                aethyme_engine::context::snippets::generate_and_write(&canonical, &map)
                    .map_err(|e| e.to_string())
            })?;
            eprintln!(
                "Snippets written to {}/.chau7/snippets.json",
                canonical.display()
            );
            profiler.report();
        }
        "prompt" => {
            let repo = read_option(&args, "--repo")?;
            let task = read_option(&args, "--task")
                .unwrap_or_else(|_| "Explain this repository".to_string());
            let focus = read_option(&args, "--focus").ok();
            let subsystem = read_option(&args, "--subsystem").ok();
            let map = build_map(&repo, no_cache, fragment_mode)?;
            let canonical = PathBuf::from(&repo)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let prompt = aethyme_engine::context::prompt::generate_prompt(
                &canonical,
                &map,
                &task,
                focus.as_deref(),
            );
            println!("{prompt}");
            // If --subsystem is provided, append subsystem-specific context
            if let Some(ref sub) = subsystem {
                let sub_context = aethyme_engine::context::prompt::generate_subsystem_context(
                    &canonical, &map, sub,
                );
                println!("\n{sub_context}");
            }
        }
        "query-areas" => {
            let repo = read_option(&args, "--repo")?;
            let depth = read_option(&args, "--depth")
                .ok()
                .and_then(|v| v.parse::<u32>().ok());
            let canonical = PathBuf::from(&repo)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let store =
                aethyme_engine::store::redb::graph_store::GraphStore::open_read_only(&canonical)
                    .map_err(|e| e.to_string())?;
            let areas = store.list_areas(depth).map_err(|e| e.to_string())?;
            println!("{}", serde_json::to_string_pretty(&areas).unwrap());
        }
        "importers" => {
            let repo = read_option(&args, "--repo")?;
            let file = read_option(&args, "--file")?;
            let canonical = PathBuf::from(&repo)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let store =
                aethyme_engine::store::redb::graph_store::GraphStore::open_read_only(&canonical)
                    .map_err(|e| e.to_string())?;
            let edges = store.edges_to(&file).map_err(|e| e.to_string())?;
            for edge in &edges {
                if let Some(path) = file_path_from_id(edge.other.as_str()) {
                    println!("{path}");
                }
            }
        }
        "deps" => {
            let repo = read_option(&args, "--repo")?;
            let file = read_option(&args, "--file")?;
            let canonical = PathBuf::from(&repo)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let store =
                aethyme_engine::store::redb::graph_store::GraphStore::open_read_only(&canonical)
                    .map_err(|e| e.to_string())?;
            let edges = store.edges_from(&file).map_err(|e| e.to_string())?;
            for edge in &edges {
                if let Some(path) = file_path_from_id(edge.other.as_str()) {
                    println!("{path}");
                }
            }
        }
        "callers" => {
            let repo = read_option(&args, "--repo")?;
            let symbol_name = read_option(&args, "--symbol")?;
            let canonical = PathBuf::from(&repo)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let store =
                aethyme_engine::store::redb::graph_store::GraphStore::open_read_only(&canonical)
                    .map_err(|e| e.to_string())?;

            // Step 1: grep -rl to find files containing the symbol name
            let grep_output = std::process::Command::new("grep")
                .args([
                    "-rl",
                    "--include=*",
                    "--exclude-dir=.git",
                    "--exclude-dir=node_modules",
                    "--exclude-dir=vendor",
                    "--exclude-dir=.aethyme",
                    &symbol_name,
                ])
                .arg(canonical.as_os_str())
                .output()
                .map_err(|e| format!("grep failed: {}", e))?;

            let grep_stdout = String::from_utf8_lossy(&grep_output.stdout);
            let repo_prefix = format!("{}/", canonical.display());
            let found_files: Vec<String> = grep_stdout
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.strip_prefix(&repo_prefix).unwrap_or(l).to_string())
                .collect();

            if !found_files.is_empty() {
                // Step 2: For each file, query the graph for files that import it
                let mut search_set: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                for f in &found_files {
                    search_set.insert(f.clone());
                    if let Ok(edges) = store.edges_to(f) {
                        for edge in &edges {
                            if let Some(p) = file_path_from_id(edge.other.as_str()) {
                                search_set.insert(p);
                            }
                        }
                    }
                }

                // Step 3: grep -n the symbol name in all search set files
                let abs_files: Vec<String> = search_set
                    .iter()
                    .map(|f| format!("{}{}", repo_prefix, f))
                    .filter(|p| std::path::Path::new(p).exists())
                    .collect();

                if !abs_files.is_empty() {
                    let mut grep_cmd = std::process::Command::new("grep");
                    grep_cmd.args(["-n", &symbol_name]);
                    for f in &abs_files {
                        grep_cmd.arg(f);
                    }
                    let result = grep_cmd
                        .output()
                        .map_err(|e| format!("grep failed: {}", e))?;
                    let result_stdout = String::from_utf8_lossy(&result.stdout);
                    for line in result_stdout.lines() {
                        let relative = line.strip_prefix(&repo_prefix).unwrap_or(line);
                        println!("{}", relative);
                    }
                }
            }
        }
        "query-overview" => {
            let repo = read_option(&args, "--repo")?;
            let canonical = PathBuf::from(&repo)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let store =
                aethyme_engine::store::redb::graph_store::GraphStore::open_read_only(&canonical)
                    .map_err(|e| e.to_string())?;
            let overview = store.overview(20, 10, 20).map_err(|e| e.to_string())?;
            // Hand-roll JSON to keep the output stable: native Overview isn't
            // serde::Serialize on the AreaNode/RiskFlag side either, so we
            // build a serde_json::Value here.
            let json = serde_json::json!({
                "repo": overview.repo,
                "areas": overview.areas,
                "entrypoints": overview.entrypoint_paths,
                "risks": overview.risks,
            });
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
        }
        "unused" => {
            let repo = read_option(&args, "--repo")?;
            let scope = read_option(&args, "--scope")?;
            let canonical = PathBuf::from(&repo)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let scope = scope.trim_end_matches('/').to_string();
            let scope_abs = canonical.join(&scope);

            // Step 1: Find all public function definitions in the scope
            let grep_defs = std::process::Command::new("grep")
                .args(["-rn", "public function ", "--include=*.php"])
                .arg(&scope_abs)
                .output()
                .map_err(|e| format!("grep failed: {}", e))?;
            if !grep_defs.status.success() && grep_defs.status.code() != Some(1) {
                return Err(format!(
                    "grep failed while reading definitions in {}",
                    scope_abs.display()
                ));
            }

            let defs_stdout = String::from_utf8_lossy(&grep_defs.stdout);
            let repo_prefix = format!("{}/", canonical.display());

            struct FuncDef {
                name: String,
                file: String,
                line: u32,
            }

            let mut functions: Vec<FuncDef> = Vec::new();
            for line in defs_stdout.lines() {
                if line.is_empty() {
                    continue;
                }
                // Parse: /abs/path/to/file:line:    public function name(
                // Convert to repo-relative path
                let relative = line.strip_prefix(&repo_prefix).unwrap_or(line);
                let parts: Vec<&str> = relative.splitn(3, ':').collect();
                if parts.len() < 3 {
                    continue;
                }
                let file = parts[0].to_string();
                let line_num: u32 = parts[1].parse().unwrap_or(0);
                let code = parts[2].trim();
                // Extract function name
                if let Some(name_start) = code.find("function ") {
                    let after = &code[name_start + 9..];
                    let name: String = after
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if name.is_empty() || name.starts_with("__") {
                        continue;
                    }
                    functions.push(FuncDef {
                        name,
                        file,
                        line: line_num,
                    });
                }
            }

            eprintln!("Found {} public functions in {}", functions.len(), scope);

            // Step 2: For each function, grep for callers outside the scope
            let mut unused = Vec::new();
            for func in &functions {
                let call_pattern = format!(r"(->|::)\s*{}\s*\(", func.name);
                let grep_callers = std::process::Command::new("grep")
                    .args([
                        "-REl",
                        "--include=*.php",
                        "--exclude-dir=.git",
                        "--exclude-dir=tests",
                        "--exclude-dir=vendor",
                        "--exclude-dir=.aethyme",
                        &call_pattern,
                    ])
                    .arg(&canonical)
                    .output()
                    .map_err(|e| format!("grep failed for {}: {}", func.name, e))?;
                if !grep_callers.status.success() && grep_callers.status.code() != Some(1) {
                    return Err(format!(
                        "grep failed while reading callers for {}",
                        func.name
                    ));
                }

                let callers = String::from_utf8_lossy(&grep_callers.stdout);
                let external_callers: Vec<&str> = callers
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| l.strip_prefix(&repo_prefix).unwrap_or(l))
                    .filter(|l| !l.starts_with(&scope))
                    .filter(|l| !l.contains("/tests/") && !l.contains("/vendor/"))
                    .collect();

                if external_callers.is_empty() {
                    unused.push((&func.name, &func.file, func.line));
                }
            }

            // Output results
            eprintln!("{} unused (no external callers)", unused.len());
            println!("# Unused public functions in {}", scope);
            println!(
                "# {} total public functions, {} unused\n",
                functions.len(),
                unused.len()
            );
            for (name, file, line) in &unused {
                println!("{file}:{line}  {name}()");
            }
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

fn read_options(args: &[String], flag: &str) -> Vec<String> {
    args.windows(2)
        .filter(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .collect()
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

// explore subcommand — thin adapter over the shared front end in
// aethyme_engine::explore_cli (also used by the top-level `aethyme`
// router). This binary keeps the documented exit-code contract:
// exit 2 with a recognizable message when the engine daemon isn't
// running, so out-of-repo callers can detect the condition.

fn run_explore_via_shared_cli(args: &[String]) -> Result<(), String> {
    use aethyme_engine::explore_cli::{ExploreCliOutcome, run};
    match run(args) {
        ExploreCliOutcome::Done => Ok(()),
        ExploreCliOutcome::DaemonNotRunning { repo } => {
            eprintln!(
                "explore: engine daemon not running; \
                 start one with `aethyme-engine-cli daemon start --repo {}`",
                repo.display()
            );
            std::process::exit(2);
        }
        ExploreCliOutcome::BadUsage(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
        ExploreCliOutcome::Failed(msg) => Err(msg),
    }
}

// daemon subcommand — start/stop/status/serve. Dispatches to the library
// in crate::daemon. Lifecycle helpers (fork via Command + setsid pre_exec,
// pidfile management, kill via libc) live here because they're shell-tied
// concerns rather than core engine concerns.

fn run_daemon_subcommand(
    args: &[String],
    no_cache: bool,
    fragment_mode: FragmentBuildMode,
) -> Result<(), String> {
    if args.is_empty() {
        return Err(
            "usage: aethyme-engine-cli daemon <serve|start|stop|status> --repo <path>".to_string(),
        );
    }
    let action = &args[0];
    let repo_str = read_option(args, "--repo")?;
    let repo = PathBuf::from(&repo_str);
    if !repo.is_dir() {
        return Err(format!("--repo path is not a directory: {repo_str}"));
    }

    match action.as_str() {
        "serve" => daemon_serve_action(&repo, no_cache, fragment_mode, args),
        "start" => daemon_start_action(&repo, no_cache, fragment_mode, args),
        "stop" => daemon_stop_action(&repo),
        "status" => daemon_status_action(&repo),
        other => Err(format!("daemon: unknown action {other:?}")),
    }
}

fn parse_daemon_idle_timeout(args: &[String]) -> Result<std::time::Duration, String> {
    if let Ok(value) = read_option(args, "--idle-timeout") {
        let secs: u64 = value
            .trim()
            .parse()
            .map_err(|e| format!("--idle-timeout: {e}"))?;
        return Ok(std::time::Duration::from_secs(secs));
    }
    Ok(std::time::Duration::from_secs(
        aethyme_engine::daemon::DEFAULT_IDLE_TIMEOUT_SECONDS,
    ))
}

fn daemon_serve_action(
    repo: &Path,
    no_cache: bool,
    fragment_mode: FragmentBuildMode,
    args: &[String],
) -> Result<(), String> {
    let idle_timeout = parse_daemon_idle_timeout(args)?;
    let config = aethyme_engine::daemon::DaemonConfig::new(repo.to_path_buf())
        .with_idle_timeout(idle_timeout)
        .with_no_cache(no_cache)
        .with_force_fragments(fragment_mode.forces_fragments());
    aethyme_engine::daemon::serve_forever(config)
}

fn daemon_start_action(
    repo: &Path,
    no_cache: bool,
    fragment_mode: FragmentBuildMode,
    args: &[String],
) -> Result<(), String> {
    let self_path = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let opts = aethyme_engine::daemon::StartOptions {
        no_cache,
        force_fragments: fragment_mode.forces_fragments(),
        idle_timeout: read_option(args, "--idle-timeout").ok(),
    };
    match aethyme_engine::daemon::start_detached(repo, &self_path, &opts)? {
        aethyme_engine::daemon::StartOutcome::AlreadyRunning(pid) => {
            eprintln!("engine daemon already running (pid {pid})");
        }
        aethyme_engine::daemon::StartOutcome::Spawned(child) => {
            let log_path = aethyme_engine::daemon::logfile_path_for(repo);
            eprintln!(
                "engine daemon spawned (pid {}, log {})\n  building map will take ~70s on a 12K-file repo",
                child.id(),
                log_path.display()
            );
        }
    }
    Ok(())
}

fn daemon_stop_action(repo: &Path) -> Result<(), String> {
    let pidfile = aethyme_engine::daemon::pidfile_path_for(repo);
    let Ok(pid_str) = std::fs::read_to_string(&pidfile) else {
        eprintln!("engine daemon: not running");
        return Ok(());
    };
    let Ok(pid) = pid_str.trim().parse::<i32>() else {
        let _ = std::fs::remove_file(&pidfile);
        eprintln!("engine daemon: stale pidfile cleaned");
        return Ok(());
    };
    let result = unsafe { libc::kill(pid, libc::SIGTERM) };
    if result == 0 {
        let _ = std::fs::remove_file(&pidfile);
        eprintln!("engine daemon: SIGTERM sent to pid {pid}");
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            let _ = std::fs::remove_file(&pidfile);
            eprintln!("engine daemon: pid {pid} already gone, pidfile cleaned");
        } else {
            return Err(format!("kill pid {pid}: {err}"));
        }
    }
    Ok(())
}

fn daemon_status_action(repo: &Path) -> Result<(), String> {
    let socket = aethyme_engine::daemon::socket_path_for(repo);
    if !socket.exists() {
        eprintln!(
            "engine daemon: not running ({} does not exist)",
            socket.display()
        );
        std::process::exit(1);
    }
    match aethyme_engine::daemon::send_request(&socket, &serde_json::json!({"command":"ping"})) {
        Ok(response) => {
            let trimmed = response.trim();
            println!("engine daemon: {trimmed}");
            Ok(())
        }
        Err(e) => {
            eprintln!("engine daemon: socket exists but ping failed: {e}");
            std::process::exit(1);
        }
    }
}

fn build_map(
    repo: &str,
    no_cache: bool,
    fragment_mode: FragmentBuildMode,
) -> Result<RepositoryMap, String> {
    build_map_with_profile(repo, no_cache, fragment_mode).map(|(map, _)| map)
}

fn build_map_with_profile(
    repo: &str,
    no_cache: bool,
    fragment_mode: FragmentBuildMode,
) -> Result<(RepositoryMap, aethyme_engine::map::RepositoryBuildProfile), String> {
    match fragment_mode {
        FragmentBuildMode::Force => RepositoryMap::build_from_fragments(&PathBuf::from(repo)),
        FragmentBuildMode::Prefer => {
            // Default path as of Phase 4.7.12: committed fragments are the
            // only map source. Missing `.aethyme/graph` is a build error.
            RepositoryMap::build_with_fragment_preference(&PathBuf::from(repo), no_cache, |_| {})
        }
    }
}

fn legacy_pass_removed_error() -> String {
    "--no-fragments is no longer supported: the legacy pass pipeline was deleted in 4.7.12; remove the flag and ensure .aethyme/graph is indexed".to_string()
}

/// Per-stage wall-time profiler for one engine command.
///
/// Diagnostic for "where does the time actually go?" The profiler reports
/// stage timings to stderr so command-specific work can be separated from
/// shared `RepositoryMap` construction. `task-localize --profile` uses it for
/// navigation latency, while `index --profile` uses it for Redb materialization.
///
/// Output line example (only when `--profile` is set):
///   [profile] task-localize: map_build=12480ms task_parse=0ms anchors=210ms \
///             scope=830ms next=305ms json_render=12ms total=13837ms
enum StageEntry {
    Top {
        name: String,
        ms: u128,
    },
    Sub {
        parent: String,
        name: String,
        ms: u128,
    },
}

struct StageProfiler {
    command: &'static str,
    enabled: bool,
    stages: Vec<StageEntry>,
    started_at: std::time::Instant,
}

impl StageProfiler {
    fn new(command: &'static str, enabled: bool) -> Self {
        Self {
            command,
            enabled,
            stages: Vec::new(),
            started_at: std::time::Instant::now(),
        }
    }

    fn stage<T, E, F>(&mut self, name: &'static str, f: F) -> Result<T, E>
    where
        F: FnOnce() -> Result<T, E>,
    {
        if !self.enabled {
            return f();
        }
        let start = std::time::Instant::now();
        let out = f();
        self.stages.push(StageEntry::Top {
            name: name.to_string(),
            ms: start.elapsed().as_millis(),
        });
        out
    }

    fn stage_pure<T, F>(&mut self, name: &'static str, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        if !self.enabled {
            return f();
        }
        let start = std::time::Instant::now();
        let out = f();
        self.stages.push(StageEntry::Top {
            name: name.to_string(),
            ms: start.elapsed().as_millis(),
        });
        out
    }

    /// Fold an existing `RepositoryBuildProfile`'s per-stage timings into the
    /// profiler output as substages of `parent`. Lets us see what's inside
    /// the otherwise-opaque map_build phase without re-instrumenting the
    /// build path.
    fn attach_substages(
        &mut self,
        parent: &'static str,
        build_profile: &aethyme_engine::map::RepositoryBuildProfile,
    ) {
        if !self.enabled {
            return;
        }
        for stage in &build_profile.stages {
            self.stages.push(StageEntry::Sub {
                parent: parent.to_string(),
                name: stage.name.clone(),
                ms: stage.duration_ms,
            });
        }
    }

    fn report(&self) {
        if !self.enabled {
            return;
        }
        let total = self.started_at.elapsed();
        let mut top_line = format!("[profile] {}: ", self.command);
        let mut first_top = true;
        let mut subs_by_parent: std::collections::BTreeMap<String, Vec<(String, u128)>> =
            std::collections::BTreeMap::new();

        for entry in &self.stages {
            match entry {
                StageEntry::Top { name, ms } => {
                    if !first_top {
                        top_line.push(' ');
                    }
                    first_top = false;
                    top_line.push_str(name);
                    top_line.push('=');
                    top_line.push_str(&format!("{ms}ms"));
                }
                StageEntry::Sub { parent, name, ms } => {
                    subs_by_parent
                        .entry(parent.clone())
                        .or_default()
                        .push((name.clone(), *ms));
                }
            }
        }
        top_line.push_str(&format!(" total={}ms", total.as_millis()));
        eprintln!("{top_line}");

        for (parent, subs) in subs_by_parent {
            let mut sub_line = format!("[profile]   {parent}.* :");
            for (name, ms) in subs {
                sub_line.push(' ');
                sub_line.push_str(&name);
                sub_line.push('=');
                sub_line.push_str(&format!("{ms}ms"));
            }
            eprintln!("{sub_line}");
        }
    }
}

fn print_explanation(map: &RepositoryMap, pack: &aethyme_engine::context_pack::ContextPack) {
    let overview = build_repo_overview(map, &pack.navigation_order);
    println!("Task: {}", pack.task.raw);
    println!("Languages: {}", map.snapshot.languages.join(", "));
    println!(
        "Top-level directories: {}",
        map.snapshot.top_level_dirs.join(", ")
    );
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
        println!(
            "Representative docs: {}",
            overview.representative_docs.join(", ")
        );
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

/// Resolve a structured node id like `file:Repo:src/lib.rs` to its repo-relative
/// path. Returns `None` for non-file kinds (areas, symbols, unresolved imports).
fn file_path_from_id(id: &str) -> Option<String> {
    let rest = id.strip_prefix("file:")?;
    let after_repo = rest.split_once(':')?.1;
    Some(after_repo.to_string())
}

fn index_to_store(
    repo_root: &std::path::Path,
    map: &RepositoryMap,
    profiler: &mut StageProfiler,
    compact: bool,
    disposable_fast: bool,
) -> Result<(), String> {
    use aethyme_engine::store::redb::graph_store::{
        self as gs, GraphStore, IndexDurability, RepoMetadata,
    };

    let canonical = repo_root.canonicalize().map_err(|e| e.to_string())?;
    if let Some(incompatible) = GraphStore::detect_incompatible_file_format(&canonical) {
        eprintln!(
            "Existing Redb graph store uses old redb file format v{}: {}",
            incompatible.found_redb_format,
            incompatible.path.display()
        );
        eprintln!(
            "Deleting graph_store.redb and regenerating it from .aethyme/graph fragments; fragments are untouched."
        );
    }
    // reset() = delete file + recreate. Mirrors what surreal's REMOVE DATABASE
    // gave us: every index pass starts from a clean slate.
    if disposable_fast {
        eprintln!(
            "Using disposable-fast Redb index mode: bulk commits are non-durable; \
             graph_store.redb is replaced only after the final durable metadata commit."
        );
    }
    let mut store = profiler.stage("redb_reset_open", || {
        if disposable_fast {
            GraphStore::reset_staging(&canonical).map_err(|e| e.to_string())
        } else {
            GraphStore::reset(&canonical).map_err(|e| e.to_string())
        }
    })?;

    let durability = if disposable_fast {
        IndexDurability::None
    } else {
        IndexDurability::Immediate
    };
    let mut session = profiler.stage("redb_begin_index", || {
        store
            .begin_index_with_durability(durability)
            .map_err(|e| e.to_string())
    })?;

    let mut file_ok = 0usize;
    let mut file_errors = 0usize;
    profiler.stage("redb_node_writes", || -> Result<(), String> {
        // Areas
        for area in &map.areas {
            gs::insert_area(&mut session, area).map_err(|e| e.to_string())?;
        }
        eprintln!("  areas: {}", map.areas.len());

        // Files
        for file in &map.files {
            if let Err(e) = gs::insert_file(&mut session, file) {
                if file_errors < 3 {
                    eprintln!(
                        "  file error: {} (area={:?}): {}",
                        &file.path, &file.area_id, e
                    );
                }
                file_errors += 1;
            } else {
                file_ok += 1;
            }
        }
        eprintln!(
            "  files: {} ok, {} errors (of {} total)",
            file_ok,
            file_errors,
            map.files.len()
        );
        Ok(())
    })?;

    // Edges — skip unresolved imports and any symbol-level endpoints. With
    // raw structured IDs we just inspect the prefix; redb keys are not
    // sanitized so what the engine produced is what we filter on.
    let mut edge_errors = 0usize;
    let mut edge_ok = 0usize;
    let mut edge_skipped = 0usize;
    profiler.stage("redb_edge_writes", || -> Result<(), String> {
        for edge in &map.edges {
            let from = edge.from.as_str();
            let to = edge.to.as_str();
            if to.starts_with("import:")
                || from.starts_with("fn:")
                || from.starts_with("class:")
                || to.starts_with("fn:")
                || to.starts_with("class:")
            {
                edge_skipped += 1;
                continue;
            }
            if let Err(e) = gs::insert_edge(&mut session, edge) {
                if edge_errors < 5 {
                    eprintln!(
                        "  edge error: {} -> {} ({:?}): {}",
                        &from[..from.len().min(50)],
                        &to[..to.len().min(50)],
                        edge.kind,
                        e
                    );
                }
                edge_errors += 1;
            } else {
                edge_ok += 1;
            }
        }
        eprintln!(
            "  edges: {} ok, {} errors, {} skipped (symbol-level) (of {} total)",
            edge_ok,
            edge_errors,
            edge_skipped,
            map.edges.len()
        );
        Ok(())
    })?;

    profiler.stage("redb_risk_writes", || -> Result<(), String> {
        for risk in &map.risk_flags {
            gs::insert_risk(&mut session, risk).map_err(|e| e.to_string())?;
        }
        eprintln!("  risks: {}", map.risk_flags.len());
        Ok(())
    })?;

    profiler.stage("redb_commit", || {
        session.commit().map_err(|e| e.to_string())
    })?;

    // Repo metadata — outside the IndexSession because it's a one-shot
    // META write and we want it persisted only after the bulk load succeeded.
    profiler.stage("metadata_write", || {
        let commit = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&canonical)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string());
        let indexed_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        store
            .set_repo_metadata(&RepoMetadata {
                root_path: canonical.to_string_lossy().to_string(),
                commit_hash: commit,
                indexed_at_unix,
                file_count: map.files.len() as u64,
                languages: map.snapshot.languages.clone(),
            })
            .map_err(|e| e.to_string())
    })?;

    let db_path = GraphStore::final_path(&canonical);
    let active_db_path = store.path().to_path_buf();
    if compact {
        let before = file_size_bytes(&active_db_path)?;
        eprintln!("Compacting redb graph store...");
        let compacted = profiler.stage("redb_compact", || {
            store.compact().map_err(|e| e.to_string())
        })?;
        // Drop before stat so the reported size is what later processes see.
        drop(store);
        let after = file_size_bytes(&active_db_path)?;
        let delta = after as i128 - before as i128;
        let delta_pct = if before == 0 {
            0.0
        } else {
            (delta as f64 / before as f64) * 100.0
        };
        let (size_word, size_delta) = if delta <= 0 {
            ("saved", (-delta) as u128)
        } else {
            ("grew", delta as u128)
        };
        eprintln!(
            "  compacted: {compacted}; size: {before} -> {after} bytes \
             ({size_word} {size_delta}, delta {delta_pct:+.1}%)"
        );
    } else {
        drop(store);
    }
    if disposable_fast {
        profiler.stage("redb_publish", || {
            GraphStore::publish_staging(&canonical).map_err(|e| e.to_string())
        })?;
    }
    eprintln!("Store written to: {}", db_path.display());
    Ok(())
}

fn file_size_bytes(path: &Path) -> Result<u64, String> {
    std::fs::metadata(path)
        .map(|meta| meta.len())
        .map_err(|e| format!("stat {}: {e}", path.display()))
}
