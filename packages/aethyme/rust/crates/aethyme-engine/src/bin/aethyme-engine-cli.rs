use std::env;
use std::path::PathBuf;

use aethyme_engine::map::RepositoryMap;
use aethyme_engine::neighborhood::{dependency_frontier, impact_frontier};
use aethyme_engine::pipeline::build_context_pack;
use aethyme_engine::search::symbol_search;
use aethyme_engine::task::TaskInput;

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

    let command = args.remove(0);
    match command.as_str() {
        "inspect" => {
            let repo = read_option(&args, "--repo")?;
            let map = RepositoryMap::build(&PathBuf::from(repo))?;
            println!("{}", aethyme_engine::json::repository_map(&map));
        }
        "symbol" => {
            let repo = read_option(&args, "--repo")?;
            let query = read_option(&args, "--query")?;
            let map = RepositoryMap::build(&PathBuf::from(repo))?;
            let hits = symbol_search(&map, &query, 20);
            println!("{}", aethyme_engine::json::search_hits(&hits));
        }
        "deps" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = RepositoryMap::build(&PathBuf::from(repo))?;
            let deps = dependency_frontier(&map, &target);
            println!("{}", aethyme_engine::json::string_list(&deps));
        }
        "impact" => {
            let repo = read_option(&args, "--repo")?;
            let target = read_option(&args, "--target")?;
            let map = RepositoryMap::build(&PathBuf::from(repo))?;
            let impact = impact_frontier(&map, &target);
            println!("{}", aethyme_engine::json::string_list(&impact));
        }
        "pack" => {
            let repo = read_option(&args, "--repo")?;
            let task_value = read_option(&args, "--task")?;
            let root = PathBuf::from(repo);
            let map = RepositoryMap::build(&root)?;
            let task = TaskInput::from_task_text(&task_value);
            let pack = build_context_pack(&root, &map, task);
            println!("{}", aethyme_engine::json::context_pack(&pack));
        }
        "explain" => {
            let repo = read_option(&args, "--repo")?;
            let task_value = read_option(&args, "--task").unwrap_or_else(|_| "Explain this repo".to_string());
            let root = PathBuf::from(repo);
            let map = RepositoryMap::build(&root)?;
            let task = TaskInput::from_task_text(&task_value);
            let pack = build_context_pack(&root, &map, task);
            print_explanation(&map, &pack);
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

fn print_explanation(map: &RepositoryMap, pack: &aethyme_engine::context_pack::ContextPack) {
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
