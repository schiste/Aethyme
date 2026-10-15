//! Exact-ID Docker commands. No workload path is used as a host destination.
use super::*;
use crate::execution_snapshot::ResourceLimits;
use std::collections::BTreeMap;
use std::process::{Command, Stdio};

const RESPONSE_LIMIT: u64 = 1024 * 1024;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(15);
const POLICY: &str = "rootfs-ro;uid65534;caps-none;nnp;seccomp-builtin;network-none;private-pid-ipc-cgroup;cpu1;swap-none;pids-bounded;tmpfs-output-bounded;no-logs;no-healthcheck;env-clear;no-cache;timeout-pid1-v0";
pub(super) struct Connection {
    pub cli: PathBuf,
    pub endpoint: String,
}
impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("cli", &self.cli)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}
impl Connection {
    pub fn local(cli: PathBuf) -> Result<Self, SupervisorError> {
        let endpoint = if let Some(host) = std::env::var_os("DOCKER_HOST") {
            host.into_string()
                .map_err(|_| SupervisorError::Unsupported("non-UTF-8 Docker endpoint".into()))?
        } else {
            let response = call_raw(
                &cli,
                &strings(&[
                    "context",
                    "inspect",
                    "--format",
                    "{{json .Endpoints.docker.Host}}",
                ]),
                RESPONSE_LIMIT,
                CONTROL_TIMEOUT,
            )?;
            if !response.status.success() {
                return Err(SupervisorError::Infrastructure(
                    "Docker context inspection failed".into(),
                ));
            }
            serde_json::from_slice::<String>(&std::fs::read(response.file.path())?)?
        };
        if !endpoint.starts_with("unix:///") || endpoint.contains('\0') {
            return Err(SupervisorError::Unsupported(
                "Local Unix Docker endpoint required".into(),
            ));
        }
        Ok(Self { cli, endpoint })
    }

    pub fn pinned(cli: PathBuf, endpoint: &str) -> Result<Self, SupervisorError> {
        if !cli.is_absolute()
            || endpoint.len() > 4096
            || !endpoint.starts_with("unix:///")
            || endpoint.contains('\0')
            || endpoint.contains('?')
            || endpoint.contains('#')
        {
            return Err(SupervisorError::Unsupported(
                "absolute Docker CLI and explicit Unix endpoint required".into(),
            ));
        }
        let path = Path::new(endpoint.strip_prefix("unix://").expect("checked prefix"));
        if !path.is_absolute()
            || path.components().any(|part| {
                matches!(
                    part,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        {
            return Err(SupervisorError::Unsupported(
                "explicit Unix Docker endpoint must be an absolute normalized path".into(),
            ));
        }
        Ok(Self {
            cli,
            endpoint: endpoint.into(),
        })
    }
}
struct Client {
    child: std::process::Child,
    reaped: bool,
}
impl Drop for Client {
    fn drop(&mut self) {
        if !self.reaped {
            crate::warn_unrecorded("stop an unreaped Docker client", self.child.kill());
            crate::warn_unrecorded("reap a Docker client", self.child.wait());
        }
    }
}
struct Response {
    status: std::process::ExitStatus,
    file: tempfile::NamedTempFile,
    error: String,
}
fn call(
    cli: &Connection,
    args: &[String],
    limit: u64,
    timeout: Duration,
) -> Result<Response, SupervisorError> {
    let mut pinned = strings(&["--host", &cli.endpoint]);
    pinned.extend_from_slice(args);
    call_raw(&cli.cli, &pinned, limit, timeout)
}
fn call_raw(
    cli: &Path,
    args: &[String],
    limit: u64,
    timeout: Duration,
) -> Result<Response, SupervisorError> {
    let output = tempfile::NamedTempFile::new()?;
    let stderr = tempfile::NamedTempFile::new()?;
    let child = Command::new(cli)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output.reopen()?))
        .stderr(Stdio::from(stderr.reopen()?))
        .spawn()?;
    let mut process = Client {
        child,
        reaped: false,
    };
    let started = Instant::now();
    let status = loop {
        if let Some(status) = process.child.try_wait()? {
            break status;
        }
        if started.elapsed() > timeout
            || output.as_file().metadata()?.len() > limit
            || stderr.as_file().metadata()?.len() > RESPONSE_LIMIT
        {
            crate::warn_unrecorded("stop the exact Docker client", process.child.kill());
            crate::warn_unrecorded("reap the exact Docker client", process.child.wait());
            return Err(SupervisorError::Infrastructure(
                "bounded Docker operation did not complete".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    process.reaped = true;
    if output.as_file().metadata()?.len() > limit
        || stderr.as_file().metadata()?.len() > RESPONSE_LIMIT
    {
        return Err(SupervisorError::Limit);
    }
    let error = String::from_utf8_lossy(&std::fs::read(stderr.path())?).into_owned();
    Ok(Response {
        status,
        file: output,
        error,
    })
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).into()).collect()
}
fn ok(cli: &Connection, args: &[String]) -> Result<String, SupervisorError> {
    let r = call(cli, args, RESPONSE_LIMIT, CONTROL_TIMEOUT)?;
    if !r.status.success() {
        return Err(SupervisorError::Infrastructure(format!(
            "Docker operation failed: {}",
            r.error
        )));
    }
    String::from_utf8(std::fs::read(r.file.path())?)
        .map(|s| s.trim().to_string())
        .map_err(|_| SupervisorError::Infrastructure("non-UTF-8 Docker response".into()))
}
fn json(cli: &Connection, args: &[String]) -> Result<serde_json::Value, SupervisorError> {
    Ok(serde_json::from_str(&ok(cli, args)?)?)
}
fn executable_digest(path: &Path) -> Result<String, SupervisorError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut chunk = [0u8; 65536];
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        hash.update(&chunk[..n]);
    }
    Ok(format!("sha256:{:x}", hash.finalize()))
}
pub(super) fn probe(cli: &Connection) -> Result<RuntimeDescription, SupervisorError> {
    let version = json(cli, &strings(&["version", "--format", "{{json .Server}}"]))?;
    let info = json(
        cli,
        &strings(&[
            "info",
            "--format",
            r#"{"id":{{json .ID}},"kernel":{{json .KernelVersion}},"cgroup":{{json .CgroupVersion}},"security":{{json .SecurityOptions}}}"#,
        ]),
    )?;
    let get = |v: &serde_json::Value, k: &str| {
        v[k].as_str()
            .map(str::to_string)
            .ok_or_else(|| SupervisorError::Unsupported(format!("missing runtime {k}")))
    };
    let architecture = get(&version, "Arch")?;
    if get(&version, "Os")? != "linux"
        || !matches!(architecture.as_str(), "arm64" | "amd64")
        || info["cgroup"].as_str() != Some("2")
        || !info["security"].as_array().is_some_and(|v| {
            v.iter().any(|v| {
                v.as_str()
                    .is_some_and(|s| s.contains("seccomp") && s.contains("builtin"))
            })
        })
    {
        return Err(SupervisorError::Unsupported(
            "Linux cgroup2 with builtin seccomp required".into(),
        ));
    }
    Ok(RuntimeDescription {
        native: executable_digest(&std::env::current_exe()?)?,
        client: executable_digest(&cli.cli)?,
        profile: PROFILE.into(),
        endpoint: cli.endpoint.clone(),
        daemon_id: get(&info, "id")?,
        engine: get(&version, "Version")?,
        api: get(&version, "ApiVersion")?,
        kernel: get(&info, "kernel")?,
        architecture,
        components: version["Components"].clone(),
        policy: POLICY.into(),
    })
}
pub(super) fn image(cli: &Connection, id: &str, architecture: &str) -> Result<(), SupervisorError> {
    let images = json(cli, &strings(&["image", "inspect", id]))?;
    let image = images
        .as_array()
        .and_then(|v| v.first())
        .ok_or_else(|| SupervisorError::Unsupported("missing pinned image".into()))?;
    if image["Id"].as_str() != Some(id)
        || image["Os"].as_str() != Some("linux")
        || image["Architecture"].as_str() != Some(architecture)
        || image["Config"]["Volumes"]
            .as_object()
            .is_some_and(|v| !v.is_empty())
    {
        return Err(SupervisorError::Unsupported(
            "image identity, architecture or implicit volumes".into(),
        ));
    }
    Ok(())
}
fn identity(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
fn container(cli: &Connection, id: &str) -> Result<serde_json::Value, SupervisorError> {
    if !identity(id) {
        return Err(SupervisorError::Tampered(
            "invalid container identity".into(),
        ));
    }
    let values = json(cli, &strings(&["container", "inspect", id]))?;
    values
        .as_array()
        .and_then(|v| v.first())
        .cloned()
        .ok_or_else(|| SupervisorError::Infrastructure("container observation missing".into()))
}
fn options(name: &str, image: &str, limits: ResourceLimits) -> Vec<String> {
    let mut a = strings(&[
        "container",
        "create",
        "--name",
        name,
        "--label",
        "aethyme.local.profile=offline-v0",
        "--pull",
        "never",
        "--network",
        "none",
        "--read-only",
        "--init=false",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges:true",
        "--user",
        "65534:65534",
        "--ipc",
        "private",
        "--cgroupns",
        "private",
        "--cpus",
        "1",
        "--no-healthcheck",
        "--log-driver",
        "none",
        "--restart",
        "no",
        "--stop-signal",
        "SIGKILL",
        "--ulimit",
        "nofile=128:128",
        "--ulimit",
        "core=0:0",
    ]);
    a.extend(strings(&[
        "--memory",
        &limits.memory_bytes.to_string(),
        "--memory-swap",
        &limits.memory_bytes.to_string(),
        "--pids-limit",
        &limits.pids.to_string(),
        "--shm-size",
        "1048576",
        "--tmpfs",
        &format!(
            "/tmp:rw,noexec,nosuid,nodev,size={}",
            limits.memory_bytes.min(64 * 1024 * 1024) / 4
        ),
        "--entrypoint",
        "/usr/bin/timeout",
    ]));
    // Image and command are appended only after all resource and mount options.
    debug_assert!(image.starts_with("sha256:"));
    a
}
fn validate_container(
    value: &serde_json::Value,
    image: &str,
    limits: ResourceLimits,
    mounts: &BTreeMap<String, (String, bool)>,
) -> Result<(), SupervisorError> {
    let h = &value["HostConfig"];
    let absent_array =
        |name: &str| h[name].is_null() || h[name].as_array().is_some_and(Vec::is_empty);
    let scratch = format!(
        "rw,noexec,nosuid,nodev,size={}",
        limits.memory_bytes.min(64 * 1024 * 1024) / 4
    );
    if value["Config"]["Entrypoint"] != serde_json::json!(["/usr/bin/timeout"])
        || value["Config"]["StopSignal"].as_str() != Some("SIGKILL")
        || value["Config"]["Healthcheck"]["Test"] != serde_json::json!(["NONE"])
        || h["RestartPolicy"]["Name"].as_str() != Some("no")
        || !absent_array("Binds")
        || !absent_array("Devices")
        || !h["PortBindings"]
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
        || h["Tmpfs"] != serde_json::json!({"/tmp": scratch})
        || h["NanoCpus"].as_u64() != Some(1_000_000_000)
        || h["Init"] != false
        || h["ShmSize"].as_u64() != Some(1_048_576)
        || value["Image"].as_str() != Some(image)
        || value["Config"]["User"].as_str() != Some("65534:65534")
        || h["ReadonlyRootfs"] != true
        || h["Privileged"] != false
        || h["NetworkMode"].as_str() != Some("none")
        || h["PidMode"].as_str() != Some("")
        || h["IpcMode"].as_str() != Some("private")
        || h["CgroupnsMode"].as_str() != Some("private")
        || h["Memory"].as_u64() != Some(limits.memory_bytes)
        || h["MemorySwap"].as_u64() != Some(limits.memory_bytes)
        || h["PidsLimit"].as_u64() != Some(limits.pids as u64)
        || !h["CapDrop"]
            .as_array()
            .is_some_and(|v| v.len() == 1 && v[0].as_str() == Some("ALL"))
        || h["CapAdd"].as_array().is_some_and(|v| !v.is_empty())
        || !h["SecurityOpt"].as_array().is_some_and(|v| {
            v.len() == 1
                && v.iter()
                    .any(|v| v.as_str() == Some("no-new-privileges:true"))
        })
        || h["LogConfig"]["Type"].as_str() != Some("none")
    {
        return Err(SupervisorError::Tampered(
            "actual container policy differs".into(),
        ));
    }
    let observed = value["Mounts"]
        .as_array()
        .ok_or_else(|| SupervisorError::Tampered("missing mounts".into()))?;
    if observed.len() != mounts.len() {
        return Err(SupervisorError::Tampered(
            "unexpected workload mount".into(),
        ));
    }
    for mount in observed {
        let path = mount["Destination"]
            .as_str()
            .ok_or_else(|| SupervisorError::Tampered("mount path".into()))?;
        let expected = mounts
            .get(path)
            .ok_or_else(|| SupervisorError::Tampered("extra workload mount".into()))?;
        if mount["Type"].as_str() != Some("volume")
            || mount["Name"].as_str() != Some(&expected.0)
            || mount["RW"].as_bool() != Some(expected.1)
        {
            return Err(SupervisorError::Tampered("mount policy differs".into()));
        }
    }
    Ok(())
}
fn inspect_owned(
    cli: &Connection,
    object: &crate::execution_resources::Object,
    generation: &str,
) -> Result<bool, SupervisorError> {
    use crate::execution_resources::ObjectKind;
    let id = object.id.as_ref().ok_or_else(|| {
        SupervisorError::Inconclusive(
            "unacknowledged daemon creation; capacity remains reserved".into(),
        )
    })?;
    let (kind, filter, format) = match object.kind {
        ObjectKind::Container => ("container", format!("id={id}"), "{{.ID}}"),
        ObjectKind::Volume => ("volume", format!("name={id}"), "{{.Name}}"),
    };
    let mut args = strings(&[kind, "ls"]);
    if object.kind == ObjectKind::Container {
        args.extend(strings(&["--all", "--no-trunc"]));
    }
    args.extend(strings(&["--filter", &filter, "--format", format]));
    let list = ok(cli, &args)?;
    if !list.lines().any(|value| value == id) {
        return Ok(false);
    }
    let value = if object.kind == ObjectKind::Container {
        container(cli, id)?
    } else {
        let value: serde_json::Value =
            serde_json::from_str(&ok(cli, &strings(&["volume", "inspect", id]))?)?;
        value
            .as_array()
            .and_then(|v| v.first())
            .cloned()
            .ok_or_else(|| SupervisorError::Tampered("volume inspection".into()))?
    };
    let valid = match object.kind {
        ObjectKind::Container => {
            value["Id"].as_str() == Some(id)
                && value["Name"].as_str() == Some(&format!("/{}", object.name))
                && value["Config"]["Labels"]["aethyme.local.generation"].as_str()
                    == Some(generation)
        }
        ObjectKind::Volume => {
            value["Name"].as_str() == Some(id)
                && value["Driver"].as_str() == Some("local")
                && value["Labels"]["aethyme.local.generation"].as_str() == Some(generation)
        }
    };
    if !valid {
        return Err(SupervisorError::Tampered(
            "cleanup identity/generation changed".into(),
        ));
    }
    Ok(true)
}
pub(super) fn recover(
    cli: &Connection,
    run: &mut crate::execution_resources::Run,
) -> Result<(), SupervisorError> {
    if run.journal.endpoint != cli.endpoint {
        return Err(SupervisorError::Tampered("cleanup endpoint changed".into()));
    }
    let mut objects = run.journal.objects.clone();
    // Containers precede volumes so attached readers cannot outlive removal.
    objects.sort_by_key(|o| {
        if o.kind == crate::execution_resources::ObjectKind::Container {
            0
        } else {
            1
        }
    });
    for object in objects {
        if object.removed {
            continue;
        }
        let id = object.id.as_ref().ok_or_else(|| {
            SupervisorError::Inconclusive(
                "creation acknowledgement missing; explicit quiescence reconciliation required"
                    .into(),
            )
        })?;
        if inspect_owned(cli, &object, &object.generation)? {
            match object.kind {
                crate::execution_resources::ObjectKind::Container => {
                    ok(cli, &strings(&["container", "rm", "--force", id]))?;
                }
                crate::execution_resources::ObjectKind::Volume => {
                    ok(cli, &strings(&["volume", "rm", id]))?;
                }
            }
        }
        run.removed(object.kind, id)?;
    }
    run.cleaned()?;
    run.release_clean()?;
    Ok(())
}
pub(super) struct Job<'a> {
    cli: &'a Connection,
    generation: &'a str,
    containers: Vec<String>,
    volumes: Vec<String>,
    admission: crate::execution_resources::Admission<'a>,
    next: u32,
}
impl<'a> Job<'a> {
    pub fn new(
        cli: &'a Connection,
        generation: &'a str,
        admission: crate::execution_resources::Admission<'a>,
    ) -> Self {
        Self {
            cli,
            generation,
            containers: vec![],
            volumes: vec![],
            admission,
            next: 0,
        }
    }
    pub(super) fn capacity<F, G>(
        &mut self,
        policy: &crate::execution_resources::ResourcePolicy,
        cancel: &AtomicBool,
        mut authorize: F,
    ) -> Result<(), SupervisorError>
    where
        F: FnMut() -> Result<G, SupervisorError>,
    {
        let info: serde_json::Value = serde_json::from_str(&ok(
            self.cli,
            &strings(&["info", "--format", "{{json .}}"]),
        )?)?;
        if info["ID"].as_str() != Some(&policy.daemon)
            || info["MemTotal"]
                .as_u64()
                .is_none_or(|bytes| bytes < u64::from(policy.memory_mib) * 1024 * 1024)
        {
            return Err(SupervisorError::Infrastructure(
                "daemon memory admission cannot be witnessed".into(),
            ));
        }
        // Only an empty volume owned by this run is mounted. Candidate input
        // volumes use the same default local driver without custom options.
        let volume = self.volume("meter", None)?;
        let limits = ResourceLimits {
            timeout_ms: 10_000,
            memory_bytes: 64 * 1024 * 1024,
            output_bytes: 1024 * 1024,
            pids: 8,
        };
        let mut args = options(
            &format!("aethyme-{}-meter", self.generation),
            &policy.meter_image,
            limits,
        );
        args.extend(strings(&[
            "--label",
            &format!("aethyme.local.generation={}", self.generation),
            "--mount",
            &format!("type=volume,source={volume},target=/meter,readonly,volume-nocopy"),
            &policy.meter_image,
            "--signal=KILL",
            "--kill-after=1s",
            "10s",
            "/usr/bin/df",
            "--output=avail",
            "-B1",
            "/meter",
        ]));
        let id = self.create(&args)?;
        if cancel.load(Ordering::SeqCst) {
            return Err(SupervisorError::Cancelled);
        }
        // The operator pins this immutable capacity-tool image separately from
        // the candidate build image. No source, secrets or expected answers.
        let guard = authorize()?;
        let output = ok(self.cli, &strings(&["container", "start", "--attach", &id]))?;
        drop(guard);
        let available = output
            .lines()
            .last()
            .and_then(|line| line.trim().parse::<u64>().ok())
            .ok_or_else(|| SupervisorError::Infrastructure("daemon disk meter response".into()))?;
        self.remove_container(&id, false)?;
        if let Some(run) = self.admission.run() {
            run.removed(crate::execution_resources::ObjectKind::Container, &id)?;
        }
        self.containers.retain(|value| value != &id);
        if self.removal_owned(crate::execution_resources::ObjectKind::Volume, &volume)? {
            ok(self.cli, &strings(&["volume", "rm", &volume]))?;
        }
        if let Some(run) = self.admission.run() {
            run.removed(crate::execution_resources::ObjectKind::Volume, &volume)?;
        }
        self.volumes.retain(|value| value != &volume);
        if available
            < policy
                .daemon_floor_bytes
                .checked_add(u64::from(policy.disk_mib) * 1024 * 1024)
                .ok_or(SupervisorError::Limit)?
        {
            return Err(SupervisorError::Infrastructure("daemon disk floor".into()));
        }
        Ok(())
    }
    fn removal_owned(
        &self,
        kind: crate::execution_resources::ObjectKind,
        id: &str,
    ) -> Result<bool, SupervisorError> {
        let Some(journal) = self.admission.journal() else {
            return Ok(true);
        };
        let object = journal
            .objects
            .iter()
            .find(|o| o.kind == kind && o.id.as_deref() == Some(id))
            .ok_or_else(|| SupervisorError::Tampered("cleanup inventory mismatch".into()))?;
        inspect_owned(self.cli, object, &object.generation)
    }
    fn remove_container(&self, id: &str, force: bool) -> Result<(), SupervisorError> {
        if self.removal_owned(crate::execution_resources::ObjectKind::Container, id)? {
            let mut args = strings(&["container", "rm"]);
            if force {
                args.push("--force".into())
            }
            args.push(id.into());
            ok(self.cli, &args)?;
        }
        Ok(())
    }
    fn create(&mut self, args: &[String]) -> Result<String, SupervisorError> {
        let mut args = args.to_vec();
        let name = if let Some(index) = args.iter().position(|v| v == "--name") {
            args.get(index + 1)
                .ok_or_else(|| SupervisorError::Tampered("container name missing".into()))?
                .clone()
        } else {
            let name = format!("aethyme-{}-import-{}", self.generation, self.next);
            self.next += 1;
            args.splice(2..2, ["--name".into(), name.clone()]);
            name
        };
        if let Some(run) = self.admission.run() {
            run.intent(
                crate::execution_resources::ObjectKind::Container,
                name.clone(),
                self.generation.into(),
            )?;
        }
        let id = ok(self.cli, &args)?;

        if !identity(&id) {
            return Err(SupervisorError::Infrastructure(
                "ambiguous container creation".into(),
            ));
        }
        if let Some(run) = self.admission.run() {
            run.created(
                crate::execution_resources::ObjectKind::Container,
                &name,
                id.clone(),
            )?;
        }
        self.containers.push(id.clone());
        Ok(id)
    }
    fn start<F, G>(
        &self,
        id: &str,
        cancel: &AtomicBool,
        authorize: F,
    ) -> Result<(), SupervisorError>
    where
        F: FnOnce() -> Result<G, SupervisorError>,
    {
        if cancel.load(Ordering::SeqCst) {
            return Err(SupervisorError::Cancelled);
        }
        // Freeze takes this lock exclusively. Dispatch and its acknowledgement
        // therefore precede revocation, or fresh authorization refuses start.
        let authority = authorize()?;
        ok(self.cli, &strings(&["container", "start", id]))?;
        drop(authority);
        Ok(())
    }
    fn volume(&mut self, role: &str, size: Option<u64>) -> Result<String, SupervisorError> {
        let name = format!("aethyme-{}-{role}", self.generation);
        let mut args = strings(&[
            "volume",
            "create",
            "--driver",
            "local",
            "--label",
            &format!("aethyme.local.generation={}", self.generation),
        ]);
        if let Some(size) = size {
            args.extend(strings(&[
                "--opt",
                "type=tmpfs",
                "--opt",
                "device=tmpfs",
                "--opt",
                &format!(
                    "o=size={size},uid=65534,gid=65534,mode=0755,nr_inodes=16384,nosuid,nodev"
                ),
            ]));
        }
        args.push(name.clone());
        if let Some(run) = self.admission.run() {
            run.intent(
                crate::execution_resources::ObjectKind::Volume,
                name.clone(),
                self.generation.into(),
            )?;
        }
        if ok(self.cli, &args)? != name {
            return Err(SupervisorError::Infrastructure(
                "volume creation identity".into(),
            ));
        }
        if let Some(run) = self.admission.run() {
            run.created(
                crate::execution_resources::ObjectKind::Volume,
                &name,
                name.clone(),
            )?;
        }
        self.volumes.push(name.clone());
        Ok(name)
    }
    fn input(
        &mut self,
        image: &str,
        role: &str,
        path: &Path,
        expected: &str,
    ) -> Result<String, SupervisorError> {
        let volume = self.volume(role, None)?;
        // This importer is never started; no image entrypoint or hook executes.
        let id = self.create(&strings(&[
            "container",
            "create",
            "--pull",
            "never",
            "--network",
            "none",
            "--label",
            &format!("aethyme.local.generation={}", self.generation),
            "--mount",
            &format!("type=volume,source={volume},target=/input,volume-nocopy"),
            "--entrypoint",
            "/bin/true",
            image,
        ]))?;
        ok(
            self.cli,
            &strings(&[
                "cp",
                &format!("{}/.", path.display()),
                &format!("{id}:/input"),
            ]),
        )?;
        let tar = call(
            self.cli,
            &strings(&["cp", &format!("{id}:/input/."), "-"]),
            288 * 1024 * 1024,
            Duration::from_secs(60),
        )?;
        if !tar.status.success() {
            return Err(SupervisorError::Infrastructure(
                "input transfer verification failed".into(),
            ));
        }
        let actual = artifact::read(tar.file.path(), 256 * 1024 * 1024, artifact::Purpose::Input)?;
        if actual.manifest.id().as_str() != expected {
            return Err(SupervisorError::Tampered(
                "transferred input differs".into(),
            ));
        }
        self.remove_container(&id, false)?;
        if let Some(run) = self.admission.run() {
            run.removed(crate::execution_resources::ObjectKind::Container, &id)?;
        }
        self.containers.retain(|v| v != &id);
        Ok(volume)
    }
    pub fn build<F, G>(
        &mut self,
        inputs: &MaterializedInputs,
        cancel: &AtomicBool,
        mut active: F,
    ) -> Result<(i32, artifact::Output), SupervisorError>
    where
        F: FnMut() -> Result<G, SupervisorError>,
    {
        let spec = inputs.snapshot().spec();
        let image = &spec.profile.build_image;
        let limits = spec.profile.build_limits;
        let source = self.input(
            image,
            "source",
            &inputs.source_dir(),
            &spec.candidate_source,
        )?;
        let mut mounts = BTreeMap::from([("/source".to_string(), (source, false))]);
        for (role, id) in [
            ("dependencies", spec.dependencies.as_deref()),
            ("configuration", spec.configuration.as_deref()),
        ] {
            if let Some(id) = id {
                let volume = self.input(image, role, &inputs.root().join(role), id)?;
                mounts.insert(format!("/{role}"), (volume, false));
            }
        }
        let output = self.volume("output", Some(limits.output_bytes))?;
        let guard_limits = ResourceLimits {
            timeout_ms: limits.timeout_ms + 120_000,
            memory_bytes: 32 * 1024 * 1024,
            output_bytes: limits.output_bytes,
            pids: 8,
        };
        let mut args = options(
            &format!("aethyme-{}-guardian", self.generation),
            image,
            guard_limits,
        );
        args.extend(strings(&[
            "--label",
            &format!("aethyme.local.generation={}", self.generation),
            "--mount",
            &format!("type=volume,source={output},target=/outputs,readonly,volume-nocopy"),
            image,
            "--signal=KILL",
            "--kill-after=1s",
            &format!("{}s", guard_limits.timeout_ms.div_ceil(1000)),
            "/bin/sleep",
            &guard_limits.timeout_ms.div_ceil(1000).to_string(),
        ]));
        let guard = self.create(&args)?;
        validate_container(
            &container(self.cli, &guard)?,
            image,
            guard_limits,
            &BTreeMap::from([("/outputs".into(), (output.clone(), false))]),
        )?;
        self.start(&guard, cancel, &mut active)?;
        mounts.insert("/outputs".into(), (output, true));
        let mut args = options(&format!("aethyme-{}-build", self.generation), image, limits);
        args.extend(strings(&[
            "--label",
            &format!("aethyme.local.generation={}", self.generation),
            "--workdir",
            "/source",
        ]));
        for (path, (volume, rw)) in &mounts {
            args.extend(strings(&[
                "--mount",
                &format!(
                    "type=volume,source={volume},target={path},volume-nocopy{}",
                    if *rw { "" } else { ",readonly" }
                ),
            ]));
        }
        args.extend(strings(&[
            image,
            "--signal=KILL",
            "--kill-after=1s",
            &format!("{}s", limits.timeout_ms.div_ceil(1000)),
            "/usr/bin/env",
            "-i",
        ]));
        for (key, value) in &spec.build.environment {
            args.push(format!("{key}={value}"));
        }
        args.push(spec.build.program.clone());
        args.extend(spec.build.arguments.clone());
        let id = self.create(&args)?;
        validate_container(&container(self.cli, &id)?, image, limits, &mounts)?;
        if cancel.load(Ordering::SeqCst) {
            return Err(SupervisorError::Cancelled);
        }
        let started = Instant::now();
        self.start(&id, cancel, &mut active)?;
        let state = loop {
            let value = container(self.cli, &id)?;
            if cancel.load(Ordering::SeqCst) {
                return Err(SupervisorError::Cancelled);
            }
            active()?;
            if started.elapsed() > Duration::from_millis(limits.timeout_ms) {
                return Err(SupervisorError::TimedOut);
            }
            if value["State"]["Running"] == false {
                break value["State"].clone();
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        // The builder's PID namespace has ended. Remove its only writable output
        // mount before copying via the separate read-only holder.
        if state["Pid"].as_u64() != Some(0) || state["Dead"] == true {
            return Err(SupervisorError::Inconclusive(
                "workload descendants not known stopped".into(),
            ));
        }
        if state["OOMKilled"] == true {
            return Err(SupervisorError::Infrastructure("workload OOM".into()));
        }
        let exit = state["ExitCode"]
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| SupervisorError::Inconclusive("missing exit status".into()))?;
        self.remove_container(&id, false)?;
        if let Some(run) = self.admission.run() {
            run.removed(crate::execution_resources::ObjectKind::Container, &id)?;
        }
        self.containers.retain(|v| v != &id);
        if exit != 0 {
            return Ok((
                exit,
                artifact::Output {
                    manifest: aethyme_contracts::experimental_v0::SourceSnapshot::new([])
                        .map_err(|e| SupervisorError::Inconclusive(e.to_string()))?,
                    files: BTreeMap::new(),
                },
            ));
        }
        let holder = container(self.cli, &guard)?;
        if holder["State"]["Running"] != true {
            return Err(SupervisorError::Infrastructure(
                "sealed output holder stopped".into(),
            ));
        }
        let tar = call(
            self.cli,
            &strings(&["cp", &format!("{guard}:/outputs/."), "-"]),
            limits.output_bytes + 32 * 1024 * 1024,
            Duration::from_secs(60),
        )?;
        if !tar.status.success() {
            return Err(SupervisorError::Infrastructure(
                "sealed output transfer failed".into(),
            ));
        }
        Ok((
            exit,
            artifact::read(
                tar.file.path(),
                limits.output_bytes,
                artifact::Purpose::Output,
            )?,
        ))
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn cli_trial<F, G>(
        &mut self,
        spec: &snapshot::ExecutionSpec,
        path: &Path,
        output_id: &str,
        arguments: &[String],
        stdin: &[u8],
        cancel: &AtomicBool,
        mut authorize: F,
    ) -> Result<super::ObservedCommand, SupervisorError>
    where
        F: FnMut() -> Result<G, SupervisorError>,
    {
        use std::io::Write;
        let image = &spec.profile.verifier_image;
        let limits = spec.profile.verifier_limits;
        let artifact = self.input(image, "artifact", path, output_id)?;
        let mounts = BTreeMap::from([("/artifact".into(), (artifact.clone(), false))]);
        let mut args = options(&format!("aethyme-{}-check", self.generation), image, limits);
        args.extend(strings(&[
            "--label",
            &format!("aethyme.local.generation={}", self.generation),
            "--interactive",
            "--workdir",
            "/artifact",
            "--mount",
            &format!("type=volume,source={artifact},target=/artifact,readonly,volume-nocopy"),
            image,
            "--signal=KILL",
            "--kill-after=1s",
            &format!("{}s", limits.timeout_ms.div_ceil(1000)),
            "/usr/bin/env",
            "-i",
        ]));
        for (key, value) in &spec.verify.environment {
            args.push(format!("{key}={value}"));
        }
        args.push(spec.verify.program.clone());
        args.extend(spec.verify.arguments.clone());
        args.extend_from_slice(arguments);
        let id = self.create(&args)?;
        validate_container(&container(self.cli, &id)?, image, limits, &mounts)?;
        let mut input = tempfile::NamedTempFile::new()?;
        input.write_all(stdin)?;
        let stdout = tempfile::NamedTempFile::new()?;
        let stderr = tempfile::NamedTempFile::new()?;
        if cancel.load(Ordering::SeqCst) {
            return Err(SupervisorError::Cancelled);
        }
        let mut dispatch = Some(authorize()?);
        let child = Command::new(&self.cli.cli)
            .args(strings(&[
                "--host",
                &self.cli.endpoint,
                "container",
                "start",
                "--attach",
                "--interactive",
                &id,
            ]))
            .stdin(Stdio::from(input.reopen()?))
            .stdout(Stdio::from(stdout.reopen()?))
            .stderr(Stdio::from(stderr.reopen()?))
            .spawn()?;
        let mut client = Client {
            child,
            reaped: false,
        };
        let started = Instant::now();
        let state = loop {
            let done = client.child.try_wait()?;
            if done.is_some() {
                client.reaped = true;
            }
            let observed = container(self.cli, &id)?;
            let state = &observed["State"];
            let acknowledged = state["StartedAt"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && !s.starts_with("0001-"));
            if acknowledged {
                drop(dispatch.take());
            }
            if cancel.load(Ordering::SeqCst) {
                return Err(SupervisorError::Cancelled);
            }
            // The dispatch guard already holds current authority and the ledger
            // transaction until acknowledgement. Re-entering it would start a
            // second transaction on the same connection.
            if dispatch.is_none() {
                authorize()?;
            }
            if started.elapsed() > Duration::from_millis(limits.timeout_ms) {
                return Err(SupervisorError::TimedOut);
            }
            if !acknowledged && started.elapsed() > CONTROL_TIMEOUT {
                return Err(SupervisorError::Infrastructure(
                    "dispatch acknowledgement unknown".into(),
                ));
            }
            if stdout.as_file().metadata()?.len() > limits.output_bytes.min(RESPONSE_LIMIT)
                || stderr.as_file().metadata()?.len() > limits.output_bytes.min(RESPONSE_LIMIT)
            {
                return Err(SupervisorError::Limit);
            }
            if let Some(client_status) = done {
                if !client_status.success() || state["ExitCode"].as_i64() != Some(0) {
                    return Err(SupervisorError::Infrastructure(
                        "verifier stream completion is ambiguous".into(),
                    ));
                }
                if !acknowledged
                    || state["Running"] != false
                    || state["Pid"].as_u64() != Some(0)
                    || state["Dead"] != false
                {
                    return Err(SupervisorError::Inconclusive(
                        "verifier process completion unknown".into(),
                    ));
                }
                break state.clone();
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if state["OOMKilled"] == true {
            return Err(SupervisorError::Infrastructure(
                "verifier workload OOM".into(),
            ));
        }
        let exit = state["ExitCode"]
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| SupervisorError::Inconclusive("missing verifier exit".into()))?;
        self.remove_container(&id, false)?;
        if let Some(run) = self.admission.run() {
            run.removed(crate::execution_resources::ObjectKind::Container, &id)?;
        }
        self.containers.retain(|v| v != &id);
        // Host capture files were never mounted into the workload. The daemon
        // stream, not an app-written success file, determines these observations.
        Ok(super::ObservedCommand {
            exit,
            stdout: std::fs::read(stdout.path())?,
            stderr: std::fs::read(stderr.path())?,
        })
    }

    pub fn cleanup(&mut self) -> Result<(), SupervisorError> {
        // Only exact IDs created by this invocation are stopped. On failure,
        // retain the inventory so a later explicit cleanup can retry safely.
        while let Some(id) = self.containers.last() {
            self.remove_container(id, true)?;
            if let Some(run) = self.admission.run() {
                run.removed(crate::execution_resources::ObjectKind::Container, id)?;
            }
            self.containers.truncate(self.containers.len() - 1);
        }
        while let Some(volume) = self.volumes.last() {
            if self.removal_owned(crate::execution_resources::ObjectKind::Volume, volume)? {
                ok(self.cli, &strings(&["volume", "rm", volume]))?;
            }
            if let Some(run) = self.admission.run() {
                run.removed(crate::execution_resources::ObjectKind::Volume, volume)?;
            }
            self.volumes.truncate(self.volumes.len() - 1);
        }
        Ok(())
    }
}
impl Drop for Job<'_> {
    fn drop(&mut self) {
        crate::warn_unrecorded("clean exact Local execution resources", self.cleanup());
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> ResourceLimits {
        ResourceLimits {
            timeout_ms: 1000,
            memory_bytes: 64 * 1024 * 1024,
            output_bytes: 1024 * 1024,
            pids: 32,
        }
    }
    fn observed() -> serde_json::Value {
        serde_json::json!({"Image":"sha256:pinned", "Config":{"User":"65534:65534","Entrypoint":["/usr/bin/timeout"],"StopSignal":"SIGKILL","Healthcheck":{"Test":["NONE"]}},"HostConfig":{"NanoCpus":1000000000,"Init":false,"ShmSize":1048576,"ReadonlyRootfs":true,"Privileged":false,"NetworkMode":"none","PidMode":"","IpcMode":"private","CgroupnsMode":"private","Memory":67108864,"MemorySwap":67108864,"PidsLimit":32,"CapDrop":["ALL"],"CapAdd":null,"SecurityOpt":["no-new-privileges:true"],"LogConfig":{"Type":"none"},"RestartPolicy":{"Name":"no"},"Binds":null,"Devices":[],"PortBindings":{},"Tmpfs":{"/tmp":"rw,noexec,nosuid,nodev,size=16777216"}},"Mounts":[{"Type":"volume","Name":"exact-source","Destination":"/source","RW":false}]})
    }
    #[test]
    fn observed_policy_requires_every_control_and_the_exact_mount() {
        let expected = BTreeMap::from([("/source".into(), ("exact-source".into(), false))]);
        let good = observed();
        validate_container(&good, "sha256:pinned", limits(), &expected).unwrap();
        let mutations = [
            ("/Image", serde_json::json!("sha256:other")),
            ("/Config/User", serde_json::json!("0:0")),
            ("/Config/Entrypoint", serde_json::json!(["/bin/sh"])),
            ("/Config/StopSignal", serde_json::json!("SIGTERM")),
            (
                "/Config/Healthcheck/Test",
                serde_json::json!(["CMD", "injected"]),
            ),
            ("/HostConfig/NanoCpus", serde_json::json!(0)),
            ("/HostConfig/Init", serde_json::json!(true)),
            ("/HostConfig/ShmSize", serde_json::json!(67108864)),
            ("/HostConfig/ReadonlyRootfs", serde_json::json!(false)),
            ("/HostConfig/Privileged", serde_json::json!(true)),
            ("/HostConfig/NetworkMode", serde_json::json!("host")),
            ("/HostConfig/PidMode", serde_json::json!("host")),
            ("/HostConfig/IpcMode", serde_json::json!("host")),
            ("/HostConfig/CgroupnsMode", serde_json::json!("host")),
            ("/HostConfig/Memory", serde_json::json!(0)),
            ("/HostConfig/MemorySwap", serde_json::json!(-1)),
            ("/HostConfig/PidsLimit", serde_json::json!(0)),
            ("/HostConfig/CapDrop", serde_json::json!([])),
            ("/HostConfig/CapAdd", serde_json::json!(["SYS_ADMIN"])),
            (
                "/HostConfig/SecurityOpt",
                serde_json::json!(["no-new-privileges:true", "seccomp=unconfined"]),
            ),
            ("/HostConfig/LogConfig/Type", serde_json::json!("json-file")),
            (
                "/HostConfig/RestartPolicy/Name",
                serde_json::json!("always"),
            ),
            ("/HostConfig/Binds", serde_json::json!(["/host:/host"])),
            (
                "/HostConfig/Devices",
                serde_json::json!([{"PathOnHost":"/dev/sda"}]),
            ),
            (
                "/HostConfig/PortBindings",
                serde_json::json!({"80/tcp":[{"HostPort":"80"}]}),
            ),
            (
                "/HostConfig/Tmpfs",
                serde_json::json!({"/tmp":"rw,size=999999999"}),
            ),
            ("/Mounts/0/Name", serde_json::json!("other-source")),
            ("/Mounts/0/RW", serde_json::json!(true)),
            ("/Mounts/0/Type", serde_json::json!("bind")),
        ];
        for (path, value) in mutations {
            let mut bad = good.clone();
            *bad.pointer_mut(path).unwrap() = value;
            assert!(
                validate_container(&bad, "sha256:pinned", limits(), &expected).is_err(),
                "{path}"
            );
        }
        let mut extra = good.clone();
        extra["Mounts"].as_array_mut().unwrap().push(
            serde_json::json!({"Type":"volume","Name":"extra","Destination":"/extra","RW":true}),
        );
        assert!(validate_container(&extra, "sha256:pinned", limits(), &expected).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn revocation_waits_for_the_actual_dispatch_acknowledgement() {
        use crate::collaboration_state::{CollaborationRoot, ProjectKey};
        use crate::local_target::tests::Fixture;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::mpsc;
        let mut f = Fixture::new();
        let owner = target::acquire_controller(&mut f.store, "dispatch-lock", "appointed").unwrap();
        let epoch = owner.authority_epoch().clone();
        let host = f._host.path().to_path_buf();
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("started");
        let release = temp.path().join("release");
        let cli = temp.path().join("docker-control");
        let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
        std::fs::write(&cli,format!("#!/bin/sh\nset -eu\ntest \"$3 $4\" = 'container start'\nprintf started > {}\nwhile test ! -f {}; do sleep 0.01; done\n",quote(&marker),quote(&release))).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (attempted_tx, attempted_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let freezing = std::thread::spawn(move || {
            let started = Instant::now();
            while !marker.is_file() {
                assert!(started.elapsed() < Duration::from_secs(5));
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut store = CollaborationStore::open(
                &CollaborationRoot::under_host_state(&host),
                &ProjectKey::parse("local-target-test").unwrap(),
                &[],
            )
            .unwrap();
            attempted_tx.send(()).unwrap();
            crate::local_authority::freeze(&mut store, &epoch, "test dispatch interleaving")
                .unwrap();
            done_tx.send(()).unwrap();
        });
        let controlling = std::thread::spawn(move || {
            attempted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let blocked = done_rx.recv_timeout(Duration::from_millis(100));
            std::fs::write(release, b"allow acknowledged dispatch").unwrap();
            assert!(
                matches!(blocked, Err(mpsc::RecvTimeoutError::Timeout)),
                "freeze completed before dispatch acknowledgement"
            );
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        let connection = Connection {
            cli,
            endpoint: "unix:///trusted-test".into(),
        };
        let job = Job::new(
            &connection,
            "test-dispatch-only",
            crate::execution_resources::Admission::ProfileOnly,
        );
        job.start(&"a".repeat(64), &AtomicBool::new(false), || {
            Ok(target::authorize(&f.store, &owner)?)
        })
        .unwrap();
        controlling.join().unwrap();
        freezing.join().unwrap();
        assert!(target::authorize(&f.store, &owner).is_err());
    }
}
