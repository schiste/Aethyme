//! Shared bounded validator process supervision.
use std::{
    collections::BTreeMap,
    io,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) fn run(
    validator: &[String],
    env: &BTreeMap<String, String>,
    change_path: &Path,
    verdict_path: &Path,
    timeout_seconds: u64,
    broker_mode: bool,
    silence: bool,
) -> Result<ExitStatus, String> {
    if validator.first().is_none_or(String::is_empty)
        || timeout_seconds == 0
        || timeout_seconds > 3600
    {
        return Err("invalid validator or timeout".into());
    }
    let mut command = Command::new(&validator[0]);
    command
        .env_clear()
        .envs(env)
        .args(&validator[1..])
        .env("CONTENT_GATE_CHANGE_FILE", change_path)
        .env("CONTENT_GATE_VERDICT_FILE", verdict_path)
        .stdin(Stdio::null());
    if silence {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    if !broker_mode {
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|_| "validator could not start")?;
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) if Instant::now() <= deadline => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(Some(_)) => {
                if !broker_mode {
                    // SAFETY: kill(2) takes plain integers and touches no Rust
                    // memory. The child was spawned with process_group(0), so
                    // its pid is also the id of the group it leads; the group
                    // outlives the reaped leader while any descendant remains.
                    // If the group is already gone the call fails with ESRCH,
                    // which is ignored: there is nothing left to stop.
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                }
                return Err("validator exceeded deadline".into());
            }
            _ => {
                if broker_mode {
                    let _ = child.kill();
                } else {
                    // SAFETY: kill(2) takes plain integers and touches no Rust
                    // memory. The child has not been reaped yet, so its pid
                    // still names it and the process group it leads
                    // (process_group(0)); the pid cannot be reused before the
                    // wait below. If the group already exited the call fails
                    // with ESRCH and the wait still collects the child.
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                }
                let _ = child.wait();
                return Err("validator timed out or could not be observed".into());
            }
        }
    };
    // A surviving process group could still mutate the verdict.
    if !broker_mode
        // SAFETY: kill(2) with signal 0 takes plain integers, delivers nothing
        // and touches no Rust memory. It probes the group the validator led
        // (process_group(0)); that id stays reserved while any descendant
        // remains in the group. ESRCH means the group is gone and is the only
        // result treated as "no survivors"; any other outcome fails closed.
        && (unsafe { libc::kill(-(child.id() as i32), 0) } == 0
            || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH))
    {
        // SAFETY: kill(2) takes plain integers and touches no Rust memory. The
        // probe above found the validator's group still present, so the id
        // names those surviving descendants. If they exit first the call
        // fails with ESRCH, which is ignored, and the run is rejected anyway.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        return Err("validator descendants may still be active".into());
    }
    // Nonzero exits never establish a trustworthy verdict.
    match status.code() {
        Some(0) => Ok(status),
        Some(1) => Err("validator exited with code 1 (failed verdict or execution error)".into()),
        Some(2) => Err("validator exited with code 2 (not checked or invalid input)".into()),
        Some(code) => Err(format!("validator exited with code {code}")),
        None => Err("validator terminated without an exit code".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kills_descendants_when_the_validator_exits_early() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let env = BTreeMap::from([("MARKER".to_owned(), marker.to_str().unwrap().to_owned())]);
        let validator = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "(/bin/sleep 2; /bin/echo survived > \"$MARKER\") & exit 0".to_owned(),
        ];
        let result = run(
            &validator,
            &env,
            dir.path(),
            &dir.path().join("verdict"),
            5,
            false,
            true,
        );
        assert_eq!(
            result.unwrap_err(),
            "validator descendants may still be active"
        );
        std::thread::sleep(Duration::from_secs(3));
        assert!(!marker.exists(), "validator descendant survived rejection");
    }
}
