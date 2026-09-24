//! Write the GitHub release body for one tag.
//!
//! ```text
//! cargo run --locked -p aethyme-testkit --example release_notes -- \
//!     --repo <checkout> --tag vX.Y.Z --output <file>
//! ```
//!
//! The body is the tag's `CHANGELOG.md` entry followed by its `UPGRADING.md`
//! section when the release is breaking. Any contract violation, or a tag
//! with no CHANGELOG entry, exits 2 without writing the file.

use std::path::PathBuf;

use aethyme_testkit::release_notes::render;

fn main() {
    if let Err(error) = run(std::env::args().skip(1)) {
        eprintln!("release notes: {error}");
        std::process::exit(2);
    }
}

fn run(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let (mut repo, mut tag, mut output) = (None, None, None);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--repo" => repo = Some(PathBuf::from(value)),
            "--tag" => tag = Some(value),
            "--output" => output = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let repo = repo.ok_or("missing --repo")?;
    let tag = tag.ok_or("missing --tag")?;
    let output = output.ok_or("missing --output")?;
    let read = |name: &str| {
        std::fs::read_to_string(repo.join(name))
            .map_err(|error| format!("cannot read {}: {error}", repo.join(name).display()))
    };
    let body = render(&tag, &read("CHANGELOG.md")?, &read("UPGRADING.md")?)?;
    std::fs::write(&output, body)
        .map_err(|error| format!("cannot write {}: {error}", output.display()))
}
