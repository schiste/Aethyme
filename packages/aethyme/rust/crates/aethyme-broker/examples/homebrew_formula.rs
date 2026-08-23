//! Render a Homebrew formula from a validated Aethyme release manifest.

use std::fs;
use std::path::PathBuf;

use aethyme_broker::{ReleaseManifest, render_homebrew_formula};

fn main() {
    if let Err(error) = run(std::env::args().skip(1)) {
        eprintln!("Homebrew formula: {error}");
        std::process::exit(2);
    }
}

fn run(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut manifest_path = None;
    let mut repository = None;
    let mut output = None;
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--manifest" => manifest_path = Some(PathBuf::from(value)),
            "--repo" => repository = Some(value),
            "--output" => output = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let manifest_path = manifest_path.ok_or("missing --manifest")?;
    let repository = repository.ok_or("missing --repo")?;
    let output = output.ok_or("missing --output")?;
    let bytes = fs::read(&manifest_path)
        .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
    let manifest: ReleaseManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", manifest_path.display()))?;
    let formula = render_homebrew_formula(&manifest, &repository)?;
    fs::write(&output, formula).map_err(|error| format!("write {}: {error}", output.display()))
}
