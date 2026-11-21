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
                    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL); }
                }
                return Err("validator exceeded deadline".into());
            }
            _ => {
                if broker_mode {
                    let _ = child.kill();
                } else {
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
        && (unsafe { libc::kill(-(child.id() as i32), 0) } == 0
            || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH))
    {
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL); }
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
        let result = run(&validator, &env, dir.path(), &dir.path().join("verdict"), 5, false, true);
        assert_eq!(result.unwrap_err(), "validator descendants may still be active");
        std::thread::sleep(Duration::from_secs(3));
        assert!(!marker.exists(), "validator descendant survived rejection");
    }
}
