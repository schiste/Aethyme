//! Render a Homebrew formula from a release manifest bound to its exact tag and source commit.

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
    let mut tag = None;
    let mut source_sha = None;
    let mut repository = None;
    let mut output = None;
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--manifest" => manifest_path = Some(PathBuf::from(value)),
            "--tag" => tag = Some(value),
            "--source-sha" => source_sha = Some(value),
            "--repo" => repository = Some(value),
            "--output" => output = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let manifest_path = manifest_path.ok_or("missing --manifest")?;
    let tag = tag.ok_or("missing --tag")?;
    let source_sha = source_sha.ok_or("missing --source-sha")?;
    let repository = repository.ok_or("missing --repo")?;
    let output = output.ok_or("missing --output")?;
    let bytes = fs::read(&manifest_path)
        .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
    let manifest: ReleaseManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", manifest_path.display()))?;
    validate_release_binding(&manifest.version, &manifest.source_sha, &tag, &source_sha)?;
    let formula = render_homebrew_formula(&manifest, &repository)?;
    fs::write(&output, formula).map_err(|error| format!("write {}: {error}", output.display()))
}

fn validate_release_binding(
    manifest_version: &str,
    manifest_source_sha: &str,
    tag: &str,
    expected_source_sha: &str,
) -> Result<(), String> {
    let tag_version = tag
        .strip_prefix('v')
        .ok_or_else(|| format!("release tag {tag:?} must begin with 'v'"))?;
    if tag_version != manifest_version {
        return Err(format!(
            "release tag {tag:?} does not match manifest version {manifest_version}"
        ));
    }
    if expected_source_sha != manifest_source_sha {
        return Err(format!(
            "release manifest source SHA {manifest_source_sha} does not match expected {expected_source_sha}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_release_binding;

    const SOURCE_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn release_binding_accepts_the_exact_tag_and_source_commit() {
        assert_eq!(
            validate_release_binding("0.8.21", SOURCE_SHA, "v0.8.21", SOURCE_SHA),
            Ok(())
        );
    }

    #[test]
    fn release_binding_rejects_a_tag_for_another_manifest_version() {
        let error =
            validate_release_binding("0.8.21", SOURCE_SHA, "v0.8.20", SOURCE_SHA).unwrap_err();
        assert!(error.contains("does not match manifest version"));
    }

    #[test]
    fn release_binding_rejects_a_different_source_commit() {
        let error = validate_release_binding(
            "0.8.21",
            SOURCE_SHA,
            "v0.8.21",
            "fedcba9876543210fedcba9876543210fedcba98",
        )
        .unwrap_err();
        assert!(error.contains("does not match expected"));
    }

    #[test]
    fn release_binding_rejects_tags_without_the_release_prefix() {
        let error =
            validate_release_binding("0.8.21", SOURCE_SHA, "0.8.21", SOURCE_SHA).unwrap_err();
        assert!(error.contains("must begin with 'v'"));
    }
}
