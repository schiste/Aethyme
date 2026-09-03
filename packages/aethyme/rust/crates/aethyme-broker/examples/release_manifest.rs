//! Build the signed-release payload from completed platform archives.
//!
//! Signing remains a workflow concern; this tool creates the deterministic
//! manifest and standalone checksum assets that are reviewed and signed.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use aethyme_broker::{
    RELEASE_TARGETS, REQUIRED_RELEASE_BINARIES, ReleaseArtifact, ReleaseInstaller, ReleaseManifest,
};
use sha2::{Digest, Sha256};

#[derive(Debug, PartialEq)]
struct Options {
    dist: PathBuf,
    tag: String,
    source_sha: String,
    channel: String,
    output: PathBuf,
}

fn main() {
    if let Err(error) = run(std::env::args().skip(1)) {
        eprintln!("release manifest: {error}");
        std::process::exit(2);
    }
}

fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let options = parse_options(args)?;
    let manifest = build_manifest(&options)?;
    write_outputs(&options, &manifest)
}

fn parse_options(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut dist = None;
    let mut tag = None;
    let mut source_sha = None;
    let mut channel = None;
    let mut output = None;
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--dist" => dist = Some(PathBuf::from(value)),
            "--tag" => tag = Some(value),
            "--source-sha" => source_sha = Some(value),
            "--channel" => channel = Some(value),
            "--output" => output = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    Ok(Options {
        dist: dist.ok_or("missing --dist")?,
        tag: tag.ok_or("missing --tag")?,
        source_sha: source_sha.ok_or("missing --source-sha")?,
        channel: channel.ok_or("missing --channel")?,
        output: output.ok_or("missing --output")?,
    })
}

fn build_manifest(options: &Options) -> Result<ReleaseManifest, String> {
    let version = env!("CARGO_PKG_VERSION");
    if options.tag != format!("v{version}") {
        return Err(format!(
            "release tag {} does not match workspace version {version}",
            options.tag
        ));
    }
    if !matches!(options.source_sha.len(), 40 | 64)
        || !options
            .source_sha
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("--source-sha must be a full 40- or 64-character Git object id".into());
    }
    if !matches!(options.channel.as_str(), "stable" | "preview") {
        return Err("--channel must be stable or preview".into());
    }

    let mut artifacts = Vec::with_capacity(RELEASE_TARGETS.len());
    for target in RELEASE_TARGETS {
        let archive = format!("aethyme-{}-{target}.tar.gz", options.tag);
        let path = options.dist.join(&archive);
        let (sha256, size_bytes) = hash_file(&path)?;
        artifacts.push(ReleaseArtifact {
            archive,
            binaries: REQUIRED_RELEASE_BINARIES
                .iter()
                .map(|binary| (*binary).to_string())
                .collect(),
            sha256,
            size_bytes,
            target: (*target).to_string(),
        });
    }
    let installer_name = "install.sh";
    let (installer_sha256, installer_size_bytes) = hash_file(&options.dist.join(installer_name))?;

    let manifest = ReleaseManifest::new(
        version,
        options.source_sha.clone(),
        options.channel.clone(),
        artifacts,
        ReleaseInstaller {
            filename: installer_name.to_string(),
            sha256: installer_sha256,
            size_bytes: installer_size_bytes,
        },
    );
    manifest.validate()?;
    Ok(manifest)
}

fn write_outputs(options: &Options, manifest: &ReleaseManifest) -> Result<(), String> {
    let mut aggregate = String::new();
    for artifact in &manifest.artifacts {
        let archive = &artifact.archive;
        let digest = &artifact.sha256;
        let line = format!("{digest}  {archive}\n");
        fs::write(options.dist.join(format!("{archive}.sha256")), &line)
            .map_err(|error| format!("write checksum for {archive}: {error}"))?;
        aggregate.push_str(&line);
    }
    let installer_name = &manifest.installer.filename;
    let installer_digest = &manifest.installer.sha256;
    let installer_line = format!("{installer_digest}  {installer_name}\n");
    fs::write(
        options.dist.join(format!("{installer_name}.sha256")),
        &installer_line,
    )
    .map_err(|error| format!("write checksum for {installer_name}: {error}"))?;
    aggregate.push_str(&installer_line);
    fs::write(options.dist.join("SHA256SUMS"), aggregate)
        .map_err(|error| format!("write SHA256SUMS: {error}"))?;
    let mut encoded =
        serde_json::to_vec_pretty(manifest).map_err(|error| format!("encode manifest: {error}"))?;
    encoded.push(b'\n');
    fs::write(&options.output, encoded)
        .map_err(|error| format!("write {}: {error}", options.output.display()))
}

fn hash_file(path: &Path) -> Result<(String, u64), String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let size = file
        .metadata()
        .map_err(|error| format!("stat {}: {error}", path.display()))?
        .len();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let encoded = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((encoded, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Options) {
        let temp = tempfile::tempdir().unwrap();
        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        for target in RELEASE_TARGETS {
            fs::write(
                temp.path().join(format!("aethyme-{tag}-{target}.tar.gz")),
                format!("archive for {target}"),
            )
            .unwrap();
        }
        fs::write(temp.path().join("install.sh"), "#!/bin/sh\n").unwrap();
        let options = Options {
            dist: temp.path().to_path_buf(),
            tag,
            source_sha: "a".repeat(40),
            channel: "stable".into(),
            output: temp.path().join("release-manifest.json"),
        };
        (temp, options)
    }

    #[test]
    fn manifest_covers_every_supported_archive_and_compatibility_boundary() {
        let (_temp, options) = fixture();

        let manifest = build_manifest(&options).unwrap();

        assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest.source_sha, "a".repeat(40));
        assert_eq!(manifest.release_channel, "stable");
        assert_eq!(manifest.artifacts.len(), 3);
        assert_eq!(
            manifest.required_binaries,
            REQUIRED_RELEASE_BINARIES
                .iter()
                .map(|binary| (*binary).to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(manifest.installer.filename, "install.sh");
        assert_eq!(manifest.installer.sha256.len(), 64);
        assert_eq!(
            manifest.compatibility.engine_protocol,
            aethyme_broker::ENGINE_PROTOCOL_VERSION
        );
        assert_eq!(manifest.compatibility.graph_refresh_plan_schema, 1);
        assert_eq!(
            manifest.compatibility.broker_storage.current_schema,
            aethyme_broker::BROKER_STORAGE_CURRENT_SCHEMA
        );
        for artifact in &manifest.artifacts {
            assert_eq!(artifact.sha256.len(), 64);
            assert_eq!(artifact.binaries, manifest.required_binaries);
        }
    }

    #[test]
    fn outputs_include_manifest_and_standalone_checksums() {
        let (_temp, options) = fixture();
        let manifest = build_manifest(&options).unwrap();

        write_outputs(&options, &manifest).unwrap();

        assert!(options.output.is_file());
        let sums = fs::read_to_string(options.dist.join("SHA256SUMS")).unwrap();
        assert_eq!(sums.lines().count(), RELEASE_TARGETS.len() + 1);
        for target in RELEASE_TARGETS {
            let archive = format!("aethyme-{}-{target}.tar.gz", options.tag);
            assert!(sums.contains(&archive));
            assert!(options.dist.join(format!("{archive}.sha256")).is_file());
        }
        assert!(options.dist.join("install.sh.sha256").is_file());
    }

    #[test]
    fn manifest_rejects_tag_sha_channel_and_archive_drift() {
        let (_temp, mut options) = fixture();
        options.tag = "v9.9.9".into();
        assert!(
            build_manifest(&options)
                .unwrap_err()
                .contains("workspace version")
        );

        let (_temp, mut options) = fixture();
        options.source_sha = "short".into();
        assert!(
            build_manifest(&options)
                .unwrap_err()
                .contains("full 40- or 64")
        );

        let (_temp, mut options) = fixture();
        options.channel = "production".into();
        assert!(
            build_manifest(&options)
                .unwrap_err()
                .contains("stable or preview")
        );

        let (temp, options) = fixture();
        fs::remove_file(temp.path().join(format!(
            "aethyme-{}-x86_64-unknown-linux-gnu.tar.gz",
            options.tag
        )))
        .unwrap();
        assert!(build_manifest(&options).unwrap_err().contains("read"));
    }
}
