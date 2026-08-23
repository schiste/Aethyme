//! Homebrew formula rendering from the validated release manifest.

use crate::{REQUIRED_RELEASE_BINARIES, ReleaseManifest};

const MACOS_ARM_TARGET: &str = "aarch64-apple-darwin";
const MACOS_INTEL_TARGET: &str = "x86_64-apple-darwin";
const LINUX_INTEL_TARGET: &str = "x86_64-unknown-linux-gnu";

pub fn render_homebrew_formula(
    manifest: &ReleaseManifest,
    repository: &str,
) -> Result<String, String> {
    manifest.validate()?;
    if manifest.release_channel != "stable" {
        return Err("Homebrew formulae may only be rendered for stable releases".into());
    }
    validate_repository(repository)?;

    let macos_arm = artifact(manifest, MACOS_ARM_TARGET)?;
    let macos_intel = artifact(manifest, MACOS_INTEL_TARGET)?;
    let linux_intel = artifact(manifest, LINUX_INTEL_TARGET)?;
    let release_url = format!(
        "https://github.com/{repository}/releases/download/v{}",
        manifest.version
    );

    Ok(format!(
        r##"# frozen_string_literal: true

# The paired Aethyme router and engine daemon.
class Aethyme < Formula
  desc "Local-first flight control for concurrent AI coding agents"
  homepage "https://github.com/{repository}"
  version "{version}"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "{release_url}/{macos_arm_archive}"
      sha256 "{macos_arm_sha}"
    end

    on_intel do
      url "{release_url}/{macos_intel_archive}"
      sha256 "{macos_intel_sha}"
    end
  end

  on_linux do
    depends_on arch: :x86_64

    on_intel do
      url "{release_url}/{linux_intel_archive}"
      sha256 "{linux_intel_sha}"
    end
  end

  def install
    bin.install "aethyme", "aethyme-engine-cli"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/aethyme --version")
    assert_match version.to_s, shell_output("#{{bin}}/aethyme-engine-cli --version")
    system bin/"aethyme", "broker", "quick-test"
  end
end
"##,
        repository = repository,
        version = manifest.version,
        release_url = release_url,
        macos_arm_archive = macos_arm.archive,
        macos_arm_sha = macos_arm.sha256,
        macos_intel_archive = macos_intel.archive,
        macos_intel_sha = macos_intel.sha256,
        linux_intel_archive = linux_intel.archive,
        linux_intel_sha = linux_intel.sha256,
    ))
}

fn artifact<'a>(
    manifest: &'a ReleaseManifest,
    target: &str,
) -> Result<&'a crate::ReleaseArtifact, String> {
    manifest
        .artifact_for_target(target)
        .ok_or_else(|| format!("release manifest has no Homebrew artifact for {target}"))
}

fn validate_repository(repository: &str) -> Result<(), String> {
    let Some((owner, name)) = repository.split_once('/') else {
        return Err("repository must be owner/name".into());
    };
    if owner.is_empty()
        || name.is_empty()
        || name.contains('/')
        || !owner.bytes().all(repository_byte)
        || !name.bytes().all(repository_byte)
    {
        return Err("repository must be a safe GitHub owner/name".into());
    }
    Ok(())
}

fn repository_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

#[cfg(test)]
mod tests {
    use crate::{RELEASE_TARGETS, ReleaseArtifact, ReleaseInstaller, ReleaseManifest};

    use super::*;

    fn manifest(channel: &str) -> ReleaseManifest {
        ReleaseManifest::new(
            "0.2.0",
            "a".repeat(40),
            channel,
            RELEASE_TARGETS
                .iter()
                .enumerate()
                .map(|(index, target)| ReleaseArtifact {
                    archive: format!("aethyme-v0.2.0-{target}.tar.gz"),
                    binaries: REQUIRED_RELEASE_BINARIES
                        .iter()
                        .map(|binary| (*binary).to_string())
                        .collect(),
                    sha256: format!("{}", index + 1).repeat(64),
                    size_bytes: 100 + index as u64,
                    target: (*target).to_string(),
                })
                .collect(),
            ReleaseInstaller {
                filename: "install.sh".into(),
                sha256: "f".repeat(64),
                size_bytes: 42,
            },
        )
    }

    #[test]
    fn formula_installs_the_pair_from_each_platform_archive() {
        let formula = render_homebrew_formula(&manifest("stable"), "schiste/Aethyme").unwrap();

        assert!(formula.contains("bin.install \"aethyme\", \"aethyme-engine-cli\""));
        assert_eq!(formula.matches("url \"").count(), 3);
        assert_eq!(formula.matches("sha256 \"").count(), 3);
        for target in RELEASE_TARGETS {
            assert!(formula.contains(&format!("aethyme-v0.2.0-{target}.tar.gz")));
        }
        assert!(formula.contains("depends_on arch: :x86_64"));
        assert!(formula.contains("broker\", \"quick-test"));
    }

    #[test]
    fn formula_rejects_preview_releases_and_unsafe_repositories() {
        assert!(
            render_homebrew_formula(&manifest("beta"), "schiste/Aethyme")
                .unwrap_err()
                .contains("stable")
        );
        assert!(
            render_homebrew_formula(&manifest("stable"), "schiste/Aethyme\"; system(\"bad\")")
                .unwrap_err()
                .contains("safe GitHub")
        );
    }
}
