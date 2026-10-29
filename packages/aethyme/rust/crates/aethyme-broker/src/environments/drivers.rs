//! Runtime drivers (#1210): how one component is provisioned, started,
//! probed, stopped and destroyed.
//!
//! Drivers are an internal trait, not a plugin ABI. Each one records the
//! identity of what it started -- `(pid, pgid, start_time)` for a process,
//! full container ids for an engine -- and every later stop, destroy or
//! observation acts only on that recorded identity. When identity cannot be
//! proven the driver refuses with `identity_unproven` and signals nothing.
//!
//! Containers are labelled `aethyme.repo`, `aethyme.env` and
//! `aethyme.component`, so anything a crash leaves behind is still found by
//! the existing reviewed `broker doctor plan` debris path.
//!
//! `static` components are served by the environment proxy (#1214); until it
//! exists their driver only checks that `serve_dir` exists.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::compile::{CompiledComponent, CompiledEnvironment};
use super::engine::{ComposeCli, EngineCli, resolve};
use super::exec::{self, Bindings, DriverError, append_log, io, log_tail, run_hook};
use super::identity::{Observation, ProcessIdentity};
use super::logs::{LOG_CAP_BYTES, open_for_append};
use super::spec::{Engine, HealthSpec, Runtime};
use crate::managed_services::FullContainerId;

const LABEL_REPO: &str = "aethyme.repo";
const LABEL_ENV: &str = "aethyme.env";
const LABEL_COMPONENT: &str = "aethyme.component";
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// After SIGKILL, how long a group may take to disappear before stop is
/// reported as unproven.
const KILL_SETTLE: Duration = Duration::from_secs(5);

/// What a driver started, as recorded on the component row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComponentIdentity {
    Process(ProcessIdentity),
    Containers {
        engine: Engine,
        #[serde(skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        ids: Vec<String>,
    },
    /// Nothing runs (`hooks`, `static`).
    None,
}

/// Everything a driver needs for one component.
pub struct DriverContext<'a> {
    pub environment: &'a CompiledEnvironment,
    pub component: &'a CompiledComponent,
    pub bindings: &'a Bindings,
    pub attempt: u32,
    /// `<host state>/environments/<env_id>`.
    pub directory: PathBuf,
    pub hook_timeout: Duration,
}

impl DriverContext<'_> {
    pub fn log(&self) -> PathBuf {
        self.directory.join(format!("{}.log", self.component.name))
    }

    pub fn vars(&self) -> BTreeMap<String, String> {
        exec::environment_variables(
            self.environment,
            self.component,
            self.bindings,
            self.attempt,
        )
    }

    fn cwd(&self) -> &Path {
        &self.bindings.worktree
    }

    pub fn hook(&self, label: &str, text: &str) -> Result<Option<Value>, DriverError> {
        run_hook(
            &format!("{}: {label}", self.component.name),
            text,
            self.cwd(),
            &self.component.pass_env,
            &self.vars(),
            &self.log(),
            self.hook_timeout,
        )
        .map(|outcome| outcome.receipt)
    }

    fn port(&self) -> Option<u16> {
        self.bindings.ports.get(&self.component.name).copied()
    }

    fn labels(&self) -> [(&'static str, String); 3] {
        [
            (LABEL_REPO, self.environment.repository.clone()),
            (LABEL_ENV, self.environment.env_id.clone()),
            (LABEL_COMPONENT, self.component.name.clone()),
        ]
    }

    fn container_name(&self) -> String {
        format!(
            "{}-{}",
            project_name(&self.bindings.namespace),
            self.component.name
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Healthy,
    NotYet(String),
}

pub trait Driver {
    /// Once per environment: create durable resources. Returns a receipt.
    fn provision(&self, ctx: &DriverContext<'_>) -> Result<Option<Value>, DriverError> {
        match &ctx.component.commands.provision {
            Some(text) => ctx.hook("provision", text),
            None => Ok(None),
        }
    }
    /// Start and return the identity to record before anything else happens.
    fn start(&self, ctx: &DriverContext<'_>) -> Result<ComponentIdentity, DriverError>;
    /// Stop what `identity` names, releasing its memory but keeping its data.
    fn stop(
        &self,
        ctx: &DriverContext<'_>,
        identity: &ComponentIdentity,
    ) -> Result<(), DriverError>;
    /// Remove everything this component created. Idempotent.
    fn destroy(
        &self,
        ctx: &DriverContext<'_>,
        identity: Option<&ComponentIdentity>,
    ) -> Result<(), DriverError>;
    /// Whether the recorded identity still exists. Never signals.
    fn observe(&self, ctx: &DriverContext<'_>, identity: &ComponentIdentity) -> Observation;
}

pub fn driver_for(runtime: Runtime) -> Box<dyn Driver> {
    match runtime {
        Runtime::Process => Box::new(ProcessDriver),
        Runtime::Compose => Box::new(ComposeDriver),
        Runtime::Container => Box::new(ContainerDriver),
        Runtime::Static => Box::new(StaticDriver),
        Runtime::Hooks => Box::new(HooksDriver),
    }
}

/// Probe once. Without a declared health check, a started component is
/// healthy as soon as it is observed alive.
pub fn probe(ctx: &DriverContext<'_>) -> Health {
    let Some(spec) = &ctx.component.health else {
        return Health::Healthy;
    };
    probe_spec(ctx, spec)
}

fn probe_spec(ctx: &DriverContext<'_>, spec: &HealthSpec) -> Health {
    if let Some(text) = &spec.command {
        let mut command = exec::command(text, ctx.cwd(), &ctx.component.pass_env, &ctx.vars());
        return match crate::bounded_output::output_limited_in_owned_group(
            &mut command,
            PROBE_TIMEOUT.max(Duration::from_millis(spec.interval_ms)),
            64 * 1024,
        ) {
            Ok(crate::bounded_output::LimitedOutput::Completed(output))
                if output.status.success() =>
            {
                Health::Healthy
            }
            Ok(crate::bounded_output::LimitedOutput::Completed(output)) => {
                Health::NotYet(format!("health command exited {}", output.status))
            }
            Ok(_) => Health::NotYet("health command timed out".into()),
            Err(error) => Health::NotYet(format!("health command: {error}")),
        };
    }
    let Some(port) = ctx.port() else {
        return Health::NotYet("no port allocated for this component".into());
    };
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = match TcpStream::connect_timeout(&address, PROBE_TIMEOUT) {
        Ok(stream) => stream,
        Err(error) => return Health::NotYet(format!("connect 127.0.0.1:{port}: {error}")),
    };
    let Some(path) = &spec.http else {
        return Health::Healthy;
    };
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));
    let request = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    if let Err(error) = stream.write_all(request.as_bytes()) {
        return Health::NotYet(format!("send: {error}"));
    }
    let mut head = [0u8; 64];
    let read = stream.read(&mut head).unwrap_or(0);
    let line = String::from_utf8_lossy(&head[..read]);
    match line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
    {
        Some(code) if (200..400).contains(&code) => Health::Healthy,
        Some(code) => Health::NotYet(format!("GET {path} answered {code}")),
        None => Health::NotYet(format!("GET {path}: no HTTP status line")),
    }
}

/// Poll until healthy, the timeout, or the component disappearing.
pub fn wait_healthy(
    driver: &dyn Driver,
    ctx: &DriverContext<'_>,
    identity: &ComponentIdentity,
) -> Result<(), DriverError> {
    let (timeout, interval) = ctx
        .component
        .health
        .as_ref()
        .map(|h| {
            (
                Duration::from_secs(h.timeout_seconds),
                Duration::from_millis(h.interval_ms),
            )
        })
        .unwrap_or((Duration::from_secs(0), Duration::from_millis(100)));
    let deadline = Instant::now() + timeout;
    loop {
        if driver.observe(ctx, identity) == Observation::Gone {
            return Err(DriverError::StartExited {
                component: ctx.component.name.clone(),
                tail: log_tail(&ctx.log()),
            });
        }
        let last = match probe(ctx) {
            Health::Healthy => return Ok(()),
            Health::NotYet(reason) => reason,
        };
        if Instant::now() >= deadline {
            append_log(&ctx.log(), &format!("[aethyme] health: gave up: {last}\n"));
            return Err(DriverError::Unhealthy {
                component: ctx.component.name.clone(),
                seconds: timeout.as_secs(),
                last,
            });
        }
        std::thread::sleep(interval);
    }
}

// ---------------------------------------------------------------- process

pub struct ProcessDriver;

impl Driver for ProcessDriver {
    fn start(&self, ctx: &DriverContext<'_>) -> Result<ComponentIdentity, DriverError> {
        let text = ctx.component.commands.start.as_deref().ok_or_else(|| {
            DriverError::Unsupported(format!("{} has no start command", ctx.component.name))
        })?;
        let log = open_for_append(&ctx.log(), LOG_CAP_BYTES).map_err(io("open component log"))?;
        let mut command = exec::command(text, ctx.cwd(), &ctx.component.pass_env, &ctx.vars());
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(io("clone log handle"))?)
            .stderr(log);
        {
            use std::os::unix::process::CommandExt as _;
            // Its own group, so stop signals exactly what this start created.
            command.process_group(0);
        }
        append_log(
            &ctx.log(),
            &format!("[aethyme] {}: start\n", ctx.component.name),
        );
        let mut child = command
            .spawn()
            .map_err(io(format!("start {}", ctx.component.name)))?;
        let pid = child.id() as i32;
        let identity = ProcessIdentity::capture(pid);
        // Reap the child whenever it exits so it never lingers as a zombie of
        // a long-lived parent (the supervisor). Identity outlives the handle.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        match identity {
            Some(identity) => Ok(ComponentIdentity::Process(identity)),
            None => Err(DriverError::StartExited {
                component: ctx.component.name.clone(),
                tail: log_tail(&ctx.log()),
            }),
        }
    }

    fn stop(
        &self,
        ctx: &DriverContext<'_>,
        identity: &ComponentIdentity,
    ) -> Result<(), DriverError> {
        let ComponentIdentity::Process(process) = identity else {
            return Err(mismatched(ctx, identity));
        };
        match process.observe() {
            Observation::Gone => return Ok(()),
            Observation::Unproven => {
                return Err(DriverError::Unproven {
                    component: ctx.component.name.clone(),
                    detail: format!("pid {} / pgid {}", process.pid, process.pgid),
                });
            }
            Observation::Alive => {}
        }
        if let Some(text) = &ctx.component.commands.stop {
            // A declared stop runs first; signals follow only if it did not
            // finish the job.
            if let Err(error) = ctx.hook("stop", text) {
                append_log(
                    &ctx.log(),
                    &format!("[aethyme] stop hook failed: {error}\n"),
                );
            }
        }
        terminate_group(
            process.pgid,
            Duration::from_secs(ctx.component.stop_grace_seconds),
        )
        .map_err(|detail| DriverError::Unproven {
            component: ctx.component.name.clone(),
            detail,
        })
    }

    fn destroy(
        &self,
        ctx: &DriverContext<'_>,
        identity: Option<&ComponentIdentity>,
    ) -> Result<(), DriverError> {
        if let Some(identity) = identity {
            self.stop(ctx, identity)?;
        }
        if let Some(text) = &ctx.component.commands.destroy {
            ctx.hook("destroy", text)?;
        }
        Ok(())
    }

    fn observe(&self, _: &DriverContext<'_>, identity: &ComponentIdentity) -> Observation {
        match identity {
            ComponentIdentity::Process(process) => process.observe(),
            _ => Observation::Unproven,
        }
    }
}

/// SIGTERM the group, wait up to `grace`, then SIGKILL and wait for it to
/// disappear. Called only after identity was proven `Alive`.
fn terminate_group(pgid: i32, grace: Duration) -> Result<(), String> {
    let gone = || !super::identity::group_exists(pgid);
    // SAFETY: killpg only sends a signal to the proven group.
    unsafe { libc::killpg(pgid, libc::SIGTERM) };
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if gone() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // SAFETY: as above.
    unsafe { libc::killpg(pgid, libc::SIGKILL) };
    let deadline = Instant::now() + KILL_SETTLE;
    while Instant::now() < deadline {
        if gone() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("process group {pgid} survived SIGKILL"))
}

fn mismatched(ctx: &DriverContext<'_>, identity: &ComponentIdentity) -> DriverError {
    DriverError::Unproven {
        component: ctx.component.name.clone(),
        detail: format!("recorded identity {identity:?} does not belong to this runtime"),
    }
}

// ---------------------------------------------------------------- hooks

pub struct HooksDriver;

impl Driver for HooksDriver {
    fn start(&self, _: &DriverContext<'_>) -> Result<ComponentIdentity, DriverError> {
        Ok(ComponentIdentity::None)
    }

    fn stop(&self, _: &DriverContext<'_>, _: &ComponentIdentity) -> Result<(), DriverError> {
        Ok(())
    }

    fn destroy(
        &self,
        ctx: &DriverContext<'_>,
        _: Option<&ComponentIdentity>,
    ) -> Result<(), DriverError> {
        if let Some(text) = &ctx.component.commands.destroy {
            ctx.hook("destroy", text)?;
        }
        Ok(())
    }

    fn observe(&self, _: &DriverContext<'_>, _: &ComponentIdentity) -> Observation {
        // Nothing runs; "alive" means "provisioned and not yet destroyed",
        // which the store, not the host, knows.
        Observation::Alive
    }
}

// ---------------------------------------------------------------- static

pub struct StaticDriver;

impl Driver for StaticDriver {
    fn start(&self, ctx: &DriverContext<'_>) -> Result<ComponentIdentity, DriverError> {
        let dir = ctx.component.serve_dir.as_deref().unwrap_or_default();
        if !ctx.cwd().join(dir).is_dir() {
            return Err(DriverError::StartExited {
                component: ctx.component.name.clone(),
                tail: format!("serve_dir {dir:?} does not exist (did build run?)"),
            });
        }
        Ok(ComponentIdentity::None)
    }

    fn stop(&self, _: &DriverContext<'_>, _: &ComponentIdentity) -> Result<(), DriverError> {
        Ok(())
    }

    fn destroy(
        &self,
        ctx: &DriverContext<'_>,
        _: Option<&ComponentIdentity>,
    ) -> Result<(), DriverError> {
        if let Some(text) = &ctx.component.commands.destroy {
            ctx.hook("destroy", text)?;
        }
        Ok(())
    }

    fn observe(&self, _: &DriverContext<'_>, _: &ComponentIdentity) -> Observation {
        Observation::Alive
    }
}

// ---------------------------------------------------------------- compose

/// Compose project names: lowercase alphanumerics, `-` and `_`, starting
/// with a letter or digit.
pub fn project_name(namespace: &str) -> String {
    let name: String = namespace
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '-' | '_') => c,
            _ => '-',
        })
        .collect();
    name.trim_start_matches(['-', '_']).to_string()
}

pub struct ComposeDriver;

impl ComposeDriver {
    fn engine(ctx: &DriverContext<'_>) -> Result<(EngineCli, ComposeCli), DriverError> {
        let engine = resolve(ctx.component.engine)?;
        let compose = engine.compose().map_err(|error| match error {
            DriverError::Unsupported(reason) => {
                DriverError::Unsupported(format!("{}: {reason}", ctx.component.name))
            }
            other => other,
        })?;
        Ok((engine, compose))
    }

    fn file_args(ctx: &DriverContext<'_>) -> Vec<String> {
        match &ctx.component.compose_file {
            Some(file) if ctx.cwd().join(file).is_file() => vec!["-f".into(), file.clone()],
            _ => Vec::new(),
        }
    }

    /// Container ids carrying the project's label, under either the Docker
    /// Compose or the podman-compose label.
    fn project_ids(
        engine: &EngineCli,
        ctx: &DriverContext<'_>,
        project: &str,
    ) -> Result<Vec<String>, DriverError> {
        let mut ids = Vec::new();
        for label in ["com.docker.compose.project", "io.podman.compose.project"] {
            let filter = format!("label={label}={project}");
            let out = engine.run(
                &[
                    "ps",
                    "-a",
                    "--no-trunc",
                    "--filter",
                    &filter,
                    "--format",
                    "{{.ID}}",
                ],
                ctx.cwd(),
                &BTreeMap::new(),
            )?;
            for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
                let id = FullContainerId::parse(line).ok_or_else(|| {
                    DriverError::Engine(format!(
                        "{} printed a non-full container id {line:?}",
                        engine.engine.as_str()
                    ))
                })?;
                if !ids.contains(&id.as_str().to_string()) {
                    ids.push(id.as_str().to_string());
                }
            }
        }
        Ok(ids)
    }

    /// An override file adding Aethyme's labels to every service, so leaked
    /// containers stay discoverable by `broker doctor`.
    fn labels_override(
        compose: &ComposeCli,
        ctx: &DriverContext<'_>,
        project: &str,
    ) -> Result<PathBuf, DriverError> {
        let mut args: Vec<String> = vec!["-p".into(), project.into()];
        args.extend(Self::file_args(ctx));
        args.extend(["config".into(), "--services".into()]);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let services = compose.run(&refs, ctx.cwd(), &ctx.vars())?;
        let mut yaml = String::from("services:\n");
        for service in services.lines().map(str::trim).filter(|s| !s.is_empty()) {
            yaml.push_str(&format!("  {}:\n    labels:\n", yaml_scalar(service)));
            for (key, value) in ctx.labels() {
                yaml.push_str(&format!(
                    "      {}: {}\n",
                    yaml_scalar(key),
                    yaml_scalar(&value)
                ));
            }
        }
        let path = ctx
            .directory
            .join(format!("{}.compose-labels.yml", ctx.component.name));
        std::fs::create_dir_all(&ctx.directory).map_err(io("create environment directory"))?;
        std::fs::write(&path, yaml).map_err(io("write compose label override"))?;
        Ok(path)
    }
}

fn yaml_scalar(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

impl Driver for ComposeDriver {
    fn start(&self, ctx: &DriverContext<'_>) -> Result<ComponentIdentity, DriverError> {
        let (engine, compose) = Self::engine(ctx)?;
        let project = project_name(&ctx.bindings.namespace);
        let labels = Self::labels_override(&compose, ctx, &project)?;
        let mut args: Vec<String> = vec!["-p".into(), project.clone()];
        args.extend(Self::file_args(ctx));
        args.extend(["-f".into(), labels.to_string_lossy().into_owned()]);
        args.extend(["up".into(), "-d".into()]);
        args.extend(ctx.component.services.iter().cloned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        append_log(
            &ctx.log(),
            &format!(
                "[aethyme] {}: compose {}\n",
                ctx.component.name,
                args.join(" ")
            ),
        );
        let started = compose.run(&refs, ctx.cwd(), &ctx.vars());
        // Record whatever exists even if `up` failed half-way: destroy must
        // be able to find it.
        let ids = Self::project_ids(&engine, ctx, &project)?;
        started?;
        Ok(ComponentIdentity::Containers {
            engine: engine.engine,
            project: Some(project),
            ids,
        })
    }

    fn stop(
        &self,
        ctx: &DriverContext<'_>,
        identity: &ComponentIdentity,
    ) -> Result<(), DriverError> {
        let ComponentIdentity::Containers {
            project: Some(project),
            ..
        } = identity
        else {
            return Err(mismatched(ctx, identity));
        };
        let (_, compose) = Self::engine(ctx)?;
        compose.run(&["-p", project, "stop"], ctx.cwd(), &ctx.vars())?;
        Ok(())
    }

    fn destroy(
        &self,
        ctx: &DriverContext<'_>,
        identity: Option<&ComponentIdentity>,
    ) -> Result<(), DriverError> {
        let project = match identity {
            Some(ComponentIdentity::Containers {
                project: Some(project),
                ..
            }) => project.clone(),
            _ => project_name(&ctx.bindings.namespace),
        };
        let (engine, compose) = Self::engine(ctx)?;
        // `down` by project name works without the compose file, so a
        // missing worktree does not strand the project.
        compose.run(
            &["-p", &project, "down", "-v", "--remove-orphans"],
            ctx.cwd(),
            &ctx.vars(),
        )?;
        let left = Self::project_ids(&engine, ctx, &project)?;
        if !left.is_empty() {
            return Err(DriverError::Engine(format!(
                "compose project {project} still has {} container(s) after down",
                left.len()
            )));
        }
        if let Some(text) = &ctx.component.commands.destroy {
            ctx.hook("destroy", text)?;
        }
        let _ = std::fs::remove_file(
            ctx.directory
                .join(format!("{}.compose-labels.yml", ctx.component.name)),
        );
        Ok(())
    }

    fn observe(&self, ctx: &DriverContext<'_>, identity: &ComponentIdentity) -> Observation {
        let ComponentIdentity::Containers {
            project: Some(project),
            ..
        } = identity
        else {
            return Observation::Unproven;
        };
        match Self::engine(ctx).and_then(|(engine, _)| Self::project_ids(&engine, ctx, project)) {
            Ok(ids) if ids.is_empty() => Observation::Gone,
            Ok(_) => Observation::Alive,
            Err(_) => Observation::Unproven,
        }
    }
}

// ---------------------------------------------------------------- container

pub struct ContainerDriver;

impl Driver for ContainerDriver {
    fn start(&self, ctx: &DriverContext<'_>) -> Result<ComponentIdentity, DriverError> {
        let engine = resolve(ctx.component.engine)?;
        let image = ctx.component.image.as_deref().ok_or_else(|| {
            DriverError::Unsupported(format!("{} has no image", ctx.component.name))
        })?;
        let name = ctx.container_name();
        let mut args: Vec<String> = vec!["run".into(), "-d".into(), "--name".into(), name];
        for (key, value) in ctx.labels() {
            args.extend(["--label".into(), format!("{key}={value}")]);
        }
        if let (Some(port), Some(inner)) = (ctx.port(), ctx.component.container_port) {
            args.extend(["-p".into(), format!("127.0.0.1:{port}:{inner}")]);
        }
        // Values travel in an owner-only env file, never in argv where any
        // local user could read them from the process table.
        let env_file = ctx.directory.join(format!("{}.env", ctx.component.name));
        std::fs::create_dir_all(&ctx.directory).map_err(io("create environment directory"))?;
        let mut vars = ctx.vars();
        for name in &ctx.component.pass_env {
            if let Ok(value) = std::env::var(name) {
                vars.insert(name.clone(), value);
            }
        }
        let body: String = vars
            .iter()
            .filter(|(_, v)| !v.contains('\n'))
            .map(|(k, v)| format!("{k}={v}\n"))
            .collect();
        std::fs::write(&env_file, body).map_err(io("write container env file"))?;
        crate::host_state::protect_host_state_path(&env_file, false)
            .map_err(io("protect env file"))?;
        args.extend(["--env-file".into(), env_file.to_string_lossy().into_owned()]);
        args.push(image.to_string());
        args.extend(ctx.component.args.iter().cloned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        append_log(
            &ctx.log(),
            &format!(
                "[aethyme] {}: {} run {image}\n",
                ctx.component.name,
                engine.engine.as_str()
            ),
        );
        let out = engine.run(&refs, ctx.cwd(), &BTreeMap::new())?;
        let printed = out
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or_default();
        let id = match engine.engine {
            // Apple's `container` identifies containers by name.
            Engine::Container => printed.to_string(),
            _ => FullContainerId::parse(printed)
                .map(|id| id.as_str().to_string())
                .ok_or_else(|| {
                    DriverError::Engine(format!(
                        "{} run printed {printed:?}, not a full container id",
                        engine.engine.as_str()
                    ))
                })?,
        };
        Ok(ComponentIdentity::Containers {
            engine: engine.engine,
            project: None,
            ids: vec![id],
        })
    }

    fn stop(
        &self,
        ctx: &DriverContext<'_>,
        identity: &ComponentIdentity,
    ) -> Result<(), DriverError> {
        let ComponentIdentity::Containers { engine, ids, .. } = identity else {
            return Err(mismatched(ctx, identity));
        };
        let engine = resolve(Some(*engine))?;
        for id in ids {
            engine.run(&["stop", id], ctx.cwd(), &BTreeMap::new())?;
        }
        Ok(())
    }

    fn destroy(
        &self,
        ctx: &DriverContext<'_>,
        identity: Option<&ComponentIdentity>,
    ) -> Result<(), DriverError> {
        if let Some(ComponentIdentity::Containers { engine, ids, .. }) = identity {
            let engine = resolve(Some(*engine))?;
            for id in ids {
                if self.observe(
                    ctx,
                    &ComponentIdentity::Containers {
                        engine: engine.engine,
                        project: None,
                        ids: vec![id.clone()],
                    },
                ) != Observation::Gone
                {
                    engine.run(&engine.remove_args(id), ctx.cwd(), &BTreeMap::new())?;
                }
            }
        }
        let _ = std::fs::remove_file(ctx.directory.join(format!("{}.env", ctx.component.name)));
        if let Some(text) = &ctx.component.commands.destroy {
            ctx.hook("destroy", text)?;
        }
        Ok(())
    }

    fn observe(&self, ctx: &DriverContext<'_>, identity: &ComponentIdentity) -> Observation {
        let ComponentIdentity::Containers { engine, ids, .. } = identity else {
            return Observation::Unproven;
        };
        let Ok(engine) = resolve(Some(*engine)) else {
            return Observation::Unproven;
        };
        let mut any = false;
        for id in ids {
            match engine.run(&["inspect", id], ctx.cwd(), &BTreeMap::new()) {
                Ok(_) => any = true,
                Err(DriverError::Engine(message))
                    if message.to_ascii_lowercase().contains("no such")
                        || message.to_ascii_lowercase().contains("not found") => {}
                Err(_) => return Observation::Unproven,
            }
        }
        if any {
            Observation::Alive
        } else {
            Observation::Gone
        }
    }
}
