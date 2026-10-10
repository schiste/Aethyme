//! Cache identity for the environment a broker gate runs under.
//!
//! A Git tree and gate definition do not identify the toolchain or the
//! relevant build environment. This module fingerprints those inputs without
//! persisting their values, and refuses cache reuse when it cannot observe a
//! toolchain probe within its bound.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::process::Command;
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::GateCacheProvenance;

pub(crate) const EXECUTION_PROFILE_SCOPE: &str = "v1 hashes OS/architecture, effective PATH, selected Cargo/Rust/compiler/Python/locale environment variables, and version output for sh, git, rustc, cargo, cargo-nextest, python, pytest, node and go. It excludes arbitrary environment variables, ignored/untracked/generated inputs (including virtualenv contents), managed-cache contents, per-run gate variables and allocated host resources, executable bytes beyond reported versions, and OS library state.";

const PROBE_BUDGET: Duration = Duration::from_secs(2);

const ENVIRONMENT_KEYS: &[&str] = &[
    "HOME",
    "PATH",
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_INCREMENTAL",
    "CARGO_NET_OFFLINE",
    "CARGO_PROFILE_DEV_DEBUG",
    "CARGO_PROFILE_TEST_DEBUG",
    "RUSTFLAGS",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "RUSTC_WRAPPER",
    "CC",
    "CXX",
    "CPPFLAGS",
    "CFLAGS",
    "CXXFLAGS",
    "AR",
    "RANLIB",
    "PKG_CONFIG_PATH",
    "PKG_CONFIG_LIBDIR",
    "PYTHONHOME",
    "PYTHONPATH",
    "VIRTUAL_ENV",
    "NODE_OPTIONS",
    "GOENV",
    "GOTOOLCHAIN",
    "LANG",
    "LC_ALL",
    "TZ",
];

struct ToolProbe {
    name: &'static str,
    program: &'static str,
    args: &'static [&'static str],
}

const TOOL_PROBES: &[ToolProbe] = &[
    ToolProbe {
        name: "sh",
        program: "sh",
        args: &["--version"],
    },
    ToolProbe {
        name: "git",
        program: "git",
        args: &["--version"],
    },
    ToolProbe {
        name: "rustc",
        program: "rustc",
        args: &["--version", "--verbose"],
    },
    ToolProbe {
        name: "cargo",
        program: "cargo",
        args: &["--version"],
    },
    ToolProbe {
        name: "cargo-nextest",
        program: "cargo",
        args: &["nextest", "--version"],
    },
    ToolProbe {
        name: "python",
        program: "python",
        args: &["--version"],
    },
    ToolProbe {
        name: "python3",
        program: "python3",
        args: &["--version"],
    },
    ToolProbe {
        name: "pytest",
        program: "pytest",
        args: &["--version"],
    },
    ToolProbe {
        name: "node",
        program: "node",
        args: &["--version"],
    },
    ToolProbe {
        name: "go",
        program: "go",
        args: &["version"],
    },
];

#[derive(Debug, Clone)]
pub(crate) struct ExecutionProfile {
    digest: Option<String>,
}

impl ExecutionProfile {
    pub(crate) fn capture(subprocess_path: Option<&OsStr>) -> Self {
        let mut environment = BTreeMap::new();
        for key in ENVIRONMENT_KEYS {
            let value = if *key == "PATH" {
                subprocess_path
                    .map(|path| path.as_encoded_bytes().to_vec())
                    .or_else(|| {
                        std::env::var_os("PATH")
                            .map(|path| path.as_os_str().as_encoded_bytes().to_vec())
                    })
            } else {
                std::env::var_os(key).map(|value| value.as_os_str().as_encoded_bytes().to_vec())
            };
            environment.insert((*key).to_string(), value);
        }

        // Version probes are independent and bounded. Running them together
        // keeps profile capture from adding the sum of their deadlines to a
        // gate run. An absent optional tool is a stable part of the profile;
        // a timeout or other spawn error makes this run ineligible for reuse.
        let tool_results = std::thread::scope(|scope| {
            let handles = TOOL_PROBES
                .iter()
                .map(|probe| {
                    scope.spawn(move || {
                        (
                            probe.name.to_string(),
                            probe_version(probe, subprocess_path),
                        )
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| {
                    handle.join().unwrap_or_else(|_| {
                        (
                            "probe-panic".to_string(),
                            Err("version probe panicked".to_string()),
                        )
                    })
                })
                .collect::<Vec<_>>()
        });

        let mut tools = BTreeMap::new();
        let mut complete = true;
        for (name, result) in tool_results {
            match result {
                Ok(version) => {
                    tools.insert(name, version);
                }
                Err(reason) => {
                    complete = false;
                    tools.insert(name, ToolVersionFingerprint::Unavailable { reason });
                }
            }
        }

        let input = ProfileFingerprint {
            schema_version: 1,
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            environment,
            tools,
        };
        Self {
            digest: complete.then(|| digest(&input)),
        }
    }

    pub(crate) fn digest(&self) -> Option<&str> {
        self.digest.as_deref()
    }

    pub(crate) fn provenance(&self) -> GateCacheProvenance {
        GateCacheProvenance {
            execution_profile_digest: self.digest.clone(),
            profile_scope: EXECUTION_PROFILE_SCOPE.to_string(),
        }
    }
}

#[derive(Serialize)]
struct ProfileFingerprint {
    schema_version: u32,
    os: &'static str,
    architecture: &'static str,
    environment: BTreeMap<String, Option<Vec<u8>>>,
    tools: BTreeMap<String, ToolVersionFingerprint>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ToolVersionFingerprint {
    Present {
        exit_code: i32,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    Missing,
    Unavailable {
        reason: String,
    },
}

fn probe_version(
    probe: &ToolProbe,
    subprocess_path: Option<&OsStr>,
) -> Result<ToolVersionFingerprint, String> {
    let mut command = Command::new(probe.program);
    command.args(probe.args);
    if let Some(path) = subprocess_path {
        command.env("PATH", path);
    }
    match crate::bounded_output::output_within(&mut command, PROBE_BUDGET) {
        Ok(None) => Err("timed out".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ToolVersionFingerprint::Missing)
        }
        Err(error) => Err(format!("spawn {}: {}", error.kind(), error)),
        Ok(Some(output)) => Ok(ToolVersionFingerprint::Present {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        }),
    }
}

fn digest(input: &ProfileFingerprint) -> String {
    let encoded = serde_json::to_vec(input).expect("profile fingerprint is serializable");
    format!("sha256:{:x}", Sha256::digest(encoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn fingerprint(tool_version: &str, cargo_home: &str) -> ProfileFingerprint {
        let environment = BTreeMap::from([
            ("CARGO_HOME".into(), Some(cargo_home.as_bytes().to_vec())),
            ("PATH".into(), Some("/toolchain/bin".as_bytes().to_vec())),
        ]);
        let tools = BTreeMap::from([(
            "rustc".into(),
            ToolVersionFingerprint::Present {
                exit_code: 0,
                stdout: tool_version.as_bytes().to_vec(),
                stderr: Vec::new(),
            },
        )]);
        ProfileFingerprint {
            schema_version: 1,
            os: "test-os",
            architecture: "test-arch",
            environment,
            tools,
        }
    }

    #[test]
    fn profile_digest_binds_toolchain_and_relevant_environment() {
        let base = fingerprint("rustc 1.90.0", "/toolchain/home");
        assert_ne!(
            digest(&base),
            digest(&fingerprint("rustc 1.91.0", "/toolchain/home")),
            "a different toolchain must produce a different profile"
        );
        assert_ne!(
            digest(&base),
            digest(&fingerprint("rustc 1.90.0", "/other/toolchain/home")),
            "a relevant environment change must produce a different profile"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_profile_without_a_complete_probe_has_no_cache_digest() {
        let directory = tempfile::tempdir().unwrap();
        let shell = directory.path().join("sh");
        std::fs::write(&shell, "not executable").unwrap();
        let mut permissions = std::fs::metadata(&shell).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&shell, permissions).unwrap();

        let profile = ExecutionProfile::capture(Some(directory.path().as_os_str()));
        assert!(profile.digest().is_none());
        assert!(profile.provenance().execution_profile_digest.is_none());
    }
}
