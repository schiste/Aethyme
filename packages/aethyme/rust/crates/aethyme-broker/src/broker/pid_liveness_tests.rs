use super::{kernel_pid_alive, pid_alive};

/// `broker status` asks this once per live session, so the common answer
/// must come from the kernel rather than a `ps` fork. On the supported
/// platforms the kernel answers for our own process; a regression to the
/// fork-per-session path shows up here as `None`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn the_kernel_answers_for_a_live_process_without_forking() {
    let own = i64::from(std::process::id());
    assert_eq!(kernel_pid_alive(own), Some(true));
    assert!(pid_alive(own));
}

/// An exited process that has not been reaped is a zombie: it still has a
/// process-table entry, so a bare existence check calls it alive. An agent
/// in that state is gone. The test first proves the child really is a
/// zombie by `ps`'s account, so it cannot pass on an already-reaped PID.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_zombie_is_dead() {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = i64::from(child.id());
    // Wait for it to exit without reaping it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while kernel_pid_alive(pid) != Some(false) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let state = std::process::Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&state.stdout)
            .trim()
            .starts_with('Z'),
        "the child must still be an unreaped zombie for this test to mean anything"
    );
    assert_eq!(
        kernel_pid_alive(pid),
        Some(false),
        "zombie must read as dead"
    );
    assert!(!pid_alive(pid));
    child.wait().unwrap();
}

/// A reaped process no longer exists, and the kernel says so definitively
/// rather than leaving the question to `ps`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_reaped_process_is_dead() {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = i64::from(child.id());
    child.wait().unwrap();
    assert!(!pid_alive(pid));
    #[cfg(target_os = "macos")]
    assert_eq!(kernel_pid_alive(pid), Some(false));
}

/// PID 0 is the kernel itself and negative values name process groups;
/// neither is ever an agent.
#[test]
fn non_agent_pids_are_never_alive() {
    assert!(!pid_alive(0));
    assert!(!pid_alive(-1));
    assert!(!pid_alive(i64::from(i32::MAX) + 1));
}
