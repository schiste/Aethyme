#!/usr/bin/python3
import datetime, json, os, pathlib, re, shlex, signal, stat, subprocess, sys, time

os.umask(0o077)
if len(sys.argv) != 9:
    raise SystemExit("expected control, cli, repo, target, issuer, revision, operation, receipts")
control, cli, repo, target, issuer, revision, operation, receipts = map(pathlib.Path, sys.argv[1:])
target, issuer, revision, operation = map(str, (target, issuer, revision, operation))
uid = os.getuid()
started = time.monotonic()
marker_deadline = started + 305.0
report = {"schema": "aethyme.t64-live-child-watcher/v1", "notice_attempts": 0}
exit_status = 1
gate_pid = None
gate_validated = False

def checked_control():
    st = os.lstat(control)
    if not stat.S_ISDIR(st.st_mode) or stat.S_ISLNK(st.st_mode):
        raise RuntimeError("control root is not a real directory")
    if st.st_uid != uid or stat.S_IMODE(st.st_mode) != 0o700:
        raise RuntimeError("control root owner/mode changed")

def verify_arm():
    fd = os.open(control / "arm-a", os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        st = os.fstat(fd)
        raw = os.read(fd, 17)
    finally:
        os.close(fd)
    if (not stat.S_ISREG(st.st_mode) or st.st_uid != uid
            or stat.S_IMODE(st.st_mode) != 0o600 or st.st_size != 6):
        raise RuntimeError("arm-a type/owner/mode refused")
    if raw != b"armed\n":
        raise RuntimeError("arm-a contents refused")

def sync_directory(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)

def create_once(path, data):
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    sync_directory(path.parent)

def read_marker(path):
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    try:
        st = os.fstat(fd)
        if (not stat.S_ISREG(st.st_mode) or st.st_uid != uid
                or stat.S_IMODE(st.st_mode) != 0o600 or st.st_size > 256):
            raise RuntimeError("gate marker type/owner/size refused")
        raw = os.read(fd, 257)
    finally:
        os.close(fd)
    match = re.fullmatch(rb"native-gate-child-active pid=([1-9][0-9]*)\n", raw)
    if match is None:
        raise RuntimeError("gate marker format refused")
    return int(match.group(1))

def child_identity(pid):
    result = subprocess.run(
        ["/bin/ps", "-p", str(pid), "-o", "pid=,uid=,stat=,command="],
        capture_output=True, text=True, timeout=2, check=False)
    if result.returncode != 0:
        return None
    fields = result.stdout.strip().split(maxsplit=3)
    if len(fields) != 4:
        return None
    got_pid, got_uid, state, command = fields
    argv = shlex.split(command)
    if int(got_pid) != pid or int(got_uid) != uid or "Z" in state:
        return None
    if argv not in (["sh", ".aethyme/t64-native-gate.sh"],
                    ["/bin/sh", ".aethyme/t64-native-gate.sh"]):
        return None
    return {"pid": pid, "uid": uid, "state": state, "command": command}

def record_file(path, value):
    create_once(path, (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode())

def check_private_dir(path):
    st = os.lstat(path)
    if (not stat.S_ISDIR(st.st_mode) or stat.S_ISLNK(st.st_mode)
            or st.st_uid != uid or stat.S_IMODE(st.st_mode) != 0o700):
        raise RuntimeError("private receipt directory type/owner/mode refused")

marker = control / "gate-a-started"
release = control / "release-a"
notice_out = receipts / "notice-b.stdout"
notice_err = receipts / "notice-b.stderr"
ready_path = receipts / "watch-b-ready.json"
waiting_path = receipts / "watch-b-waiting.json"
report_path = receipts / "watch-b.json"
proc = None
try:
    checked_control()
    check_private_dir(receipts)
    verify_arm()
    if set(os.listdir(control)) != {"arm-a"}:
        raise RuntimeError("unexpected control-root entries before service dispatch")
    report["started_utc"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    report["watcher_pid"] = os.getpid()
    record_file(ready_path, {"schema": "aethyme.t64-live-child-watcher-ready/v1",
                             "watcher_pid": os.getpid(), "uid": uid,
                             "started_utc": report["started_utc"],
                             "phase": "starting_marker_wait", "argv": sys.argv,
                             "control": str(control), "cli": str(cli), "repo": str(repo),
                             "target": target, "issuer": issuer, "revision": revision,
                             "operation": operation})
    record_file(waiting_path, {"schema": "aethyme.t64-live-child-watcher-waiting/v1",
                               "watcher_pid": os.getpid(), "uid": uid,
                               "waiting_for": "gate-a-started", "argv": sys.argv,
                               "control": str(control), "cli": str(cli), "repo": str(repo),
                               "target": target, "issuer": issuer, "revision": revision,
                               "operation": operation})
    while time.monotonic() < marker_deadline:
        try:
            pid = read_marker(marker)
            break
        except FileNotFoundError:
            time.sleep(min(0.1, max(0.0, marker_deadline - time.monotonic())))
    else:
        report["gate_marker_result"] = "not_observed_within_305s_unknown"
        raise TimeoutError("A gate marker was not observed within 305 seconds")
    report["gate_marker_result"] = "observed"
    gate_pid = pid
    report["gate_pid"] = pid
    report["gate_before"] = child_identity(pid)
    if report["gate_before"] is None:
        raise RuntimeError("exact A gate child is not live before B notice")
    gate_validated = True
    report["before_notice_utc"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    argv = [str(cli), "collab", "target", "capture-notice", "ingest",
            "--target", target, "--issuer", issuer, "--revision", revision,
            "--alias", "B", "--operation", operation, "--repo", str(repo), "--json"]
    report["notice_argv"] = argv
    with open(notice_out, "xb") as out, open(notice_err, "xb") as err:
        proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                start_new_session=True)
        report["notice_pid"] = proc.pid
        report["notice_attempts"] = 1
        try:
            report["notice_exit"] = proc.wait(timeout=5.0)
            report["notice_result"] = "completed"
        except subprocess.TimeoutExpired:
            report["notice_result"] = "timed_out_unknown"
            try:
                os.killpg(proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                proc.wait(timeout=1.0)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                proc.wait(timeout=1.0)
            report["notice_exit"] = proc.returncode
    report["after_notice_utc"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    report["gate_after"] = child_identity(pid)
    if report["notice_result"] != "completed" or report["notice_exit"] != 0:
        raise RuntimeError("B notice did not complete successfully; no retry")
    if report["gate_after"] is None:
        raise RuntimeError("exact A gate child ended before B notice post-check")
    exit_status = 0
except Exception as exc:
    report["failure"] = type(exc).__name__ + ": " + str(exc)
finally:
    report["finished_utc"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    try:
        checked_control()
        if not gate_validated or gate_pid is None:
            report["release_a_created_and_synced"] = False
            report["release_result"] = "withheld_no_validated_gate"
        elif (set(os.listdir(control)) != {"arm-a", "gate-a-started"}
              or read_marker(marker) != gate_pid or child_identity(gate_pid) is None):
            report["release_a_created_and_synced"] = False
            report["release_result"] = "withheld_gate_marker_or_exact_child_changed_unknown"
            exit_status = 1
        else:
            create_once(release, b"release\n")
            report["release_a_created_and_synced"] = True
            report["release_result"] = "created_after_exact_live_gate_validation"
    except Exception as exc:
        report["release_a_created_and_synced"] = False
        report["release_failure"] = type(exc).__name__ + ": " + str(exc)
        exit_status = 1
    try:
        record_file(report_path, report)
    except Exception as exc:
        print("watch report could not be persisted: " + str(exc), file=sys.stderr)
        exit_status = 1
print(json.dumps(report, sort_keys=True, separators=(",", ":")))
raise SystemExit(exit_status)
