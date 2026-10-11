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
pub(super) struct Job<'a> {
    cli: &'a Connection,
    generation: &'a str,
    containers: Vec<String>,
    volumes: Vec<String>,
}
impl<'a> Job<'a> {
    pub fn new(cli: &'a Connection, generation: &'a str) -> Self {
        Self {
            cli,
            generation,
            containers: vec![],
            volumes: vec![],
        }
    }
    fn create(&mut self, args: &[String]) -> Result<String, SupervisorError> {
        let id = ok(self.cli, args)?;
        if !identity(&id) {
            return Err(SupervisorError::Infrastructure(
                "ambiguous container creation".into(),
            ));
        }
        self.containers.push(id.clone());
        Ok(id)
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
        if ok(self.cli, &args)? != name {
            return Err(SupervisorError::Infrastructure(
                "volume creation identity".into(),
            ));
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
        ok(self.cli, &strings(&["container", "rm", &id]))?;
        self.containers.retain(|v| v != &id);
        Ok(volume)
    }
    pub fn build<F>(
        &mut self,
        inputs: &MaterializedInputs,
        cancel: &AtomicBool,
        mut active: F,
    ) -> Result<(i32, artifact::Output), SupervisorError>
    where
        F: FnMut() -> Result<(), SupervisorError>,
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
        ok(self.cli, &strings(&["container", "start", &guard]))?;
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
        active()?;
        let started = Instant::now();
        ok(self.cli, &strings(&["container", "start", &id]))?;
        let state = loop {
            let value = container(self.cli, &id)?;
            if value["State"]["Running"] == false {
                break value["State"].clone();
            }
            if cancel.load(Ordering::SeqCst) {
                return Err(SupervisorError::Cancelled);
            }
            active()?;
            if started.elapsed() > Duration::from_millis(limits.timeout_ms) {
                return Err(SupervisorError::TimedOut);
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
        ok(self.cli, &strings(&["container", "rm", &id]))?;
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
    pub fn cleanup(&mut self) -> Result<(), SupervisorError> {
        // Only exact IDs created by this invocation are stopped. On failure,
        // retain the inventory so a later explicit cleanup can retry safely.
        while let Some(id) = self.containers.last() {
            ok(self.cli, &strings(&["container", "rm", "--force", id]))?;
            self.containers.truncate(self.containers.len() - 1);
        }
        while let Some(volume) = self.volumes.last() {
            ok(self.cli, &strings(&["volume", "rm", volume]))?;
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
}
