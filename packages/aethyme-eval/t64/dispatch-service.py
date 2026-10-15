#!/usr/bin/python3
import datetime, json, os, pathlib, shlex, stat, subprocess, sys, time

os.umask(0o077)
if len(sys.argv) < 8 or sys.argv[6] != "--":
    raise SystemExit("usage: dispatch-service.py RUN_ROOT TARGET ISSUER REVISION NOTICE_OPERATION -- SERVICE_ARG...")
root = pathlib.Path(sys.argv[1])
target, issuer, revision, notice_operation = sys.argv[2:6]
argv = sys.argv[7:]
uid = os.getuid()
receipts = root / "receipts"
control = pathlib.Path(os.environ.get("CONTROL_ROOT", ""))
ready_path = receipts / "watch-b-ready.json"
waiting_path = receipts / "watch-b-waiting.json"
watcher_script = str(root / "bin" / "watch-b.py")
deadline = time.monotonic() + 5.0

def check_private_dir(path):
    st = os.lstat(path)
    if (not stat.S_ISDIR(st.st_mode) or stat.S_ISLNK(st.st_mode)
            or st.st_uid != uid or stat.S_IMODE(st.st_mode) != 0o700):
        raise RuntimeError("private directory type/owner/mode refused")

check_private_dir(root)
check_private_dir(receipts)
if not control.is_absolute():
    raise SystemExit("CONTROL_ROOT must be set to the reviewed absolute path")
check_private_dir(control)
if set(os.listdir(control)) != {"arm-a"}:
    raise SystemExit("control root changed before service dispatch")
arm_fd = os.open(control / "arm-a", os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
try:
    arm_stat = os.fstat(arm_fd)
    arm_bytes = os.read(arm_fd, 7)
finally:
    os.close(arm_fd)
if (not stat.S_ISREG(arm_stat.st_mode) or arm_stat.st_uid != uid
        or stat.S_IMODE(arm_stat.st_mode) != 0o600 or arm_bytes != b"armed\n"):
    raise SystemExit("arm-a identity/content changed; no service dispatched")

def read_private_json(path, max_bytes=8192):
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        st = os.fstat(fd)
        if (not stat.S_ISREG(st.st_mode) or st.st_uid != uid
                or stat.S_IMODE(st.st_mode) != 0o600
                or st.st_size > max_bytes):
            raise RuntimeError("receipt type/owner/mode/size refused")
        raw = os.read(fd, max_bytes + 1)
    finally:
        os.close(fd)
    return json.loads(raw)

def write_once(path, value):
    raw = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    dirfd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(dirfd)
    finally:
        os.close(dirfd)

while time.monotonic() < deadline:
    try:
        ready = read_private_json(ready_path)
        waiting = read_private_json(waiting_path)
        break
    except FileNotFoundError:
        time.sleep(0.05)
else:
    raise SystemExit("watcher ready receipt not observed within five seconds; no service dispatched")
if ready.get("schema") != "aethyme.t64-live-child-watcher-ready/v1":
    raise SystemExit("watcher ready receipt schema mismatch; no service dispatched")
if waiting.get("schema") != "aethyme.t64-live-child-watcher-waiting/v1":
    raise SystemExit("watcher waiting receipt schema mismatch; no service dispatched")
if not argv or argv[0] != str(root / "bin" / "aethyme"):
    raise SystemExit("service must use the run-pinned CLI; no service dispatched")

watcher_pid = int(ready["watcher_pid"])
expected_watcher_argv = [watcher_script, str(control), argv[0], str(root / "source"),
                         target, issuer, revision, notice_operation, str(receipts)]
if (ready.get("uid") != uid or ready.get("phase") != "starting_marker_wait"
        or ready.get("target") != target or ready.get("issuer") != issuer
        or ready.get("revision") != revision or ready.get("operation") != notice_operation
        or ready.get("control") != str(control) or ready.get("repo") != str(root / "source")
        or ready.get("argv") != expected_watcher_argv
        or waiting.get("watcher_pid") != watcher_pid or waiting.get("uid") != uid
        or waiting.get("waiting_for") != "gate-a-started"
        or waiting.get("target") != target or waiting.get("issuer") != issuer
        or waiting.get("revision") != revision or waiting.get("operation") != notice_operation
        or waiting.get("control") != str(control) or waiting.get("repo") != str(root / "source")
        or waiting.get("argv") != expected_watcher_argv):
    raise SystemExit("watcher startup/waiting receipt identity mismatch; no service dispatched")
def option(name):
    try:
        return argv[argv.index(name) + 1]
    except (ValueError, IndexError):
        raise SystemExit("service argv is missing " + name + "; no service dispatched")
repo = option("--repo")
if (option("--target") != target or option("--issuer") != issuer
        or pathlib.Path(repo) != root / "source"
        or option("--endpoint") != "unix:///Volumes/T7-repositories/Colima/home/default/docker.sock"
        or option("--docker") != "/opt/homebrew/bin/docker"):
    raise SystemExit("service target/issuer/repository binding mismatch; no service dispatched")
ps = subprocess.run(["/bin/ps", "-p", str(watcher_pid), "-o", "pid=,uid=,stat=,command="],
                    capture_output=True, text=True, timeout=2, check=False)
fields = ps.stdout.strip().split(maxsplit=3)
if ps.returncode != 0 or len(fields) != 4:
    raise SystemExit("watcher is not live; no service dispatched")
pid_text, uid_text, state, command = fields
watcher_argv = shlex.split(command)
if (int(pid_text) != watcher_pid or int(uid_text) != uid or "Z" in state
        or len(watcher_argv) < 2
        or watcher_argv[0] not in ("/usr/bin/python3", "python3")
        or watcher_argv[1:] != expected_watcher_argv):
    raise SystemExit("watcher PID does not match exact run-owned script; no service dispatched")
if (set(os.listdir(control)) != {"arm-a"}
        or (control / "gate-a-started").exists()
        or (control / "release-a").exists()):
    raise SystemExit("watcher no longer waits for the gate marker; no service dispatched")

env_names = ("HOME", "PATH", "DOCKER_HOST", "AETHYME_HOST_STATE_DIR", "TMPDIR")
if any(name not in os.environ for name in env_names):
    raise SystemExit("required clean service environment is incomplete")
env = {name: os.environ[name] for name in env_names}
started = datetime.datetime.now(datetime.timezone.utc).isoformat()
stdout_fd = os.open(receipts / "service.stdout", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
stderr_fd = os.open(receipts / "service.stderr", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(stdout_fd, "wb") as out, os.fdopen(stderr_fd, "wb") as err:
    child = subprocess.Popen(argv, cwd=root, env=env, stdin=subprocess.DEVNULL,
                             stdout=out, stderr=err, start_new_session=True)
    print(json.dumps({"service_pid": child.pid, "watcher_pid": watcher_pid,
                      "started_utc": started, "argv": argv}, sort_keys=True), flush=True)
    write_once(receipts / "service-started.json", {
        "schema": "aethyme.t64-service-process/v1", "pid": child.pid,
        "uid": uid, "process_group": child.pid, "argv": argv,
        "watcher_pid": watcher_pid, "started_utc": started,
        "observation_timeout_seconds": 360
    })
    stop_at = time.monotonic() + 360.0
    while child.poll() is None and time.monotonic() < stop_at:
        time.sleep(0.2)
    if child.poll() is None:
        observation = {"schema": "aethyme.t64-service-observation/v1",
                       "state": "running_unknown", "pid": child.pid,
                       "uid": uid, "observed_utc": datetime.datetime.now(
                           datetime.timezone.utc).isoformat(),
                       "signal_sent": False}
        write_once(receipts / "service-observation.json", observation)
        raise SystemExit(124)
    status = child.wait()
    observation = {"schema": "aethyme.t64-service-observation/v1",
                   "state": "exited", "pid": child.pid, "uid": uid,
                   "exit_status": status,
                   "observed_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                   "signal_sent": False}
    write_once(receipts / "service-observation.json", observation)
raise SystemExit(status)
