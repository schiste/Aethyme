//! Container engine selection for `compose` and `container` components
//! (#1210).
//!
//! Order: the component's `engine` (or `[engine] default`, already folded
//! into the snapshot), then `AETHYME_CONTAINER_RUNTIME` -- the variable
//! `broker doctor` already honours -- then `docker` or `podman` on `PATH`.
//! Switching engines never requires a profile edit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::exec::DriverError;
use super::spec::Engine;
use crate::gate_debris::CONTAINER_RUNTIME_ENV;

/// One engine call. A wedged daemon must not hang a lifecycle step.
pub const ENGINE_BUDGET: Duration = Duration::from_secs(120);
const ENGINE_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineCli {
    pub engine: Engine,
    pub program: PathBuf,
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

fn engine_named(name: &str) -> Option<Engine> {
    Some(match name {
        "docker" => Engine::Docker,
        "podman" => Engine::Podman,
        "container" => Engine::Container,
        "nerdctl" => Engine::Nerdctl,
        _ => return None,
    })
}

pub fn resolve(declared: Option<Engine>) -> Result<EngineCli, DriverError> {
    if let Some(engine) = declared {
        return find_on_path(engine.as_str())
            .map(|program| EngineCli { engine, program })
            .ok_or_else(|| {
                DriverError::Engine(format!("{} is not on PATH", engine.as_str()))
            });
    }
    if let Ok(requested) = std::env::var(CONTAINER_RUNTIME_ENV) {
        let requested = requested.trim();
        if !requested.is_empty() && requested != "none" {
            let path = PathBuf::from(requested);
            let program = if path.components().count() > 1 {
                path.is_file().then_some(path)
            } else {
                find_on_path(requested)
            }
            .ok_or_else(|| {
                DriverError::Engine(format!("{CONTAINER_RUNTIME_ENV}={requested} is not executable"))
            })?;
            let name = program
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let engine = engine_named(&name).ok_or_else(|| {
                DriverError::Engine(format!(
                    "{CONTAINER_RUNTIME_ENV}={requested}: unknown engine {name:?} (docker, podman, container, nerdctl)"
                ))
            })?;
            return Ok(EngineCli { engine, program });
        }
    }
    [Engine::Docker, Engine::Podman]
        .into_iter()
        .find_map(|engine| {
            find_on_path(engine.as_str()).map(|program| EngineCli { engine, program })
        })
        .ok_or_else(|| {
            DriverError::Engine(
                "no container engine found: install docker or podman, or set engine in environments.toml".into(),
            )
        })
}

/// How compose is invoked: the engine's plugin (`docker compose`) when it
/// answers, else the standalone binary (`docker-compose`, `podman-compose`).
/// Hosts with only the standalone binary are common (Homebrew installs it
/// without the CLI plugin), and both accept the same subcommands used here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeCli {
    pub program: PathBuf,
    /// `["compose"]` for the plugin form, empty for a standalone binary.
    pub prefix: Vec<String>,
}

impl EngineCli {
    pub fn compose(&self) -> Result<ComposeCli, DriverError> {
        if !self.engine.supports_compose() {
            return Err(DriverError::Unsupported(format!(
                "the {} engine has no compose support",
                self.engine.as_str()
            )));
        }
        let plugin = ComposeCli {
            program: self.program.clone(),
            prefix: vec!["compose".into()],
        };
        if plugin.run(&["version"], Path::new("/"), &BTreeMap::new()).is_ok() {
            return Ok(plugin);
        }
        let standalone = match self.engine {
            Engine::Podman => "podman-compose",
            _ => "docker-compose",
        };
        find_on_path(standalone)
            .map(|program| ComposeCli {
                program,
                prefix: Vec::new(),
            })
            .ok_or_else(|| {
                DriverError::Engine(format!(
                    "neither `{} compose` nor `{standalone}` is available",
                    self.engine.as_str()
                ))
            })
    }

    /// Run the engine with a cleared environment plus `vars`, under the
    /// engine budget. Returns stdout; a non-zero exit is an error with stderr.
    pub fn run(
        &self,
        args: &[&str],
        cwd: &Path,
        vars: &BTreeMap<String, String>,
    ) -> Result<String, DriverError> {
        run_program(&self.program, self.engine.as_str(), args, cwd, vars)
    }
}

fn run_program(
    program: &Path,
    name: &str,
    args: &[&str],
    cwd: &Path,
    vars: &BTreeMap<String, String>,
) -> Result<String, DriverError> {
        let mut command = Command::new(program);
        command.args(args).current_dir(cwd).env_clear();
        for variable in super::exec::INHERITED_VARIABLES
            .iter()
            .copied()
            .chain(["DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG", "CONTAINER_HOST", "XDG_RUNTIME_DIR"])
        {
            if let Some(value) = std::env::var_os(variable) {
                command.env(variable, value);
            }
        }
        command.envs(vars);
        let label = format!("{name} {}", args.join(" "));
        let outcome = crate::bounded_output::output_limited_in_owned_group(
            &mut command,
            ENGINE_BUDGET,
            ENGINE_OUTPUT_LIMIT,
        )
        .map_err(|error| DriverError::Engine(format!("{label}: {error}")))?;
        match outcome {
            crate::bounded_output::LimitedOutput::Completed(output) if output.status.success() => {
                Ok(String::from_utf8_lossy(&output.stdout).into_owned())
            }
            crate::bounded_output::LimitedOutput::Completed(output) => Err(DriverError::Engine(
                format!(
                    "{label} failed ({}): {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            )),
            crate::bounded_output::LimitedOutput::Deadline => Err(DriverError::Engine(format!(
                "{label} did not answer within {}s",
                ENGINE_BUDGET.as_secs()
            ))),
            crate::bounded_output::LimitedOutput::OutputLimit => {
                Err(DriverError::Engine(format!("{label}: output limit exceeded")))
            }
        }
}

impl ComposeCli {
    pub fn run(
        &self,
        args: &[&str],
        cwd: &Path,
        vars: &BTreeMap<String, String>,
    ) -> Result<String, DriverError> {
        let mut full: Vec<&str> = self.prefix.iter().map(String::as_str).collect();
        full.extend_from_slice(args);
        let name = self
            .program
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        run_program(&self.program, &name, &full, cwd, vars)
    }
}

impl EngineCli {
    /// Whether the engine answers at all (used to skip conformance tests).
    pub fn available(&self) -> bool {
        let args: &[&str] = match self.engine {
            Engine::Container => &["system", "status"],
            _ => &["info"],
        };
        self.run(args, Path::new("/"), &BTreeMap::new()).is_ok()
    }

    /// The verb that force-removes a container.
    pub fn remove_args<'a>(&self, id: &'a str) -> Vec<&'a str> {
        match self.engine {
            Engine::Container => vec!["delete", "--force", id],
            _ => vec!["rm", "-f", id],
        }
    }
}
