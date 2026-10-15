# T64 public CLI rehearsal runbook

Last Updated: 2026-10-15

Status: **pretrial procedure; no T64 Playground, contributor, verifier, Docker, native-gate, or source-build execution is claimed here.** The path/phase clarification v3 and nonce-status lookup slice are independently CLEAR. Integrated CLI review, remaining source changes, focused validation, and the source-pinned build are still required before execution. The final launch manifest must name the reviewed source commit, binary SHA-256, and runtime authorization.

This is a bounded Local-path rehearsal for two real, independent contributor sessions. Both source captures must exist before target selection. Contributor B's readiness notice is sent while contributor A's named native-gate child is live. It does **not** exercise a fresh capture arriving after the service starts, same-wave joining, model usefulness, reliable autonomy, or throughput.

## Frozen inputs and claim boundary

Use the following byte-frozen, unexecuted inputs. Hash the files again at launch and record them in the append-only launch manifest; do not edit them to fit the implementation.

| Input | SHA-256 | Use |
|---|---|---|
| `run-profile-v3.json` | `c5b6887b680bd136622f6728a9fad430ba3c31cbfa44da2cd72a2efa836eccc1` | Docker identity, limits, flow and service bounds |
| `prelaunch-checklist-v2.json` | `c159fcd6a69c3e76bee44bead99733af6a7ede6702461900b9591231419c1782` | Preflight and teardown order |
| `operator-completion-v1.json` | `6e106d8a9164b0c8cd149d7627657653cca996dcb8ac4ba7148749a254cfddcb` | Exact project/objective caps, headrooms and transport timeout |
| `operator-labels-v1.json` | `fa4df5e8737560d5c52a39f99d1c76648970fd7cfa2edeb18c39fd8d15bd80cb` | Explicit external authorization labels; these are not stored records or permissions |
| zero-exit selection manifest | `2f64314e16e2acb68d2e92b11cb2907f64b00b45ae79bca38615a57beed27516` | Selection rule and source inventory binding |
| selected ten-case inventory bytes | `88f54c1c2cd72a65cb9387d982d79baa8eadcc840e5d055edfeb522ed0adb51c` | Native verifier inventory content SHA-256 |
| original 15-case input manifest | `b4f3c4f1455da88b58924dce2fd8487f4f3afd3d935f584bfcfbffc71f346a9c` | Original scope; retain unchanged |
| operator proposal manifest | `c4e5556cf27d5345efcb1800f923482c3d6d923e3c2567fe96af661b440ff9e2` | Frozen generic operator policy |
| run-profile authorization digest | `97141fc4726634b958525e24142e7b148c97c4c9db8352bca786cf339c4e5629` | External project-budget authorization label |
| objective authorization label | `b4f3c4f1455da88b58924dce2fd8487f4f3afd3d935f584bfcfbffc71f346a9c` | External target label, not permission |
| phase/path clarification v2 | `57e057476b25e7b5bdb2cf797e8794371509fbf373ee20dca3338cb23acd93e5` | Superseded by v3; retained unchanged |
| phase/path clarification v3 (`/private/tmp/aethyme-t64-phase-clarification-v3-8wa1pv3o/clarification.json`) | `7b3e65ab82e53526646483cca2f299d8d8792ef80d05e9ed86afca9c248f990a` | Independently reviewed pretrial ordering and host-state child-path interpretation |
| application `source/app.sh` | `8851e393268aae2a0007acecbdc56050276b3de586a3e79c7236ee2a592a8e2d` | Frozen seed source |
| `source/lib/greeting.sh` | `fb1f2cd7900b03a00648ddbaf8f18dce61b5cff27a1caeec14873de6c2ae409c` | Frozen seed source |
| `source/lib/math.sh` | `e89cb7689486c429f6f0df217d377962f860e3f93f3fb0f4feceb25ebcbe6ac8` | Frozen seed source |
| public brief A | `d644536d7b72906b8a116a6968dcafc481f09bc761ae842bf114c9ba885a8313` | Copy to contributor A only |
| public brief B | `1a235f96bb823a9ce3dc6fa12b5b2d1a2567d900a3d36bab4135f4000b1a6482` | Copy to contributor B only |
| public task profile | `c5d09a86672fb71780d0164b62805e0017d60b1b89b8a87d4f1b683076777f3f` | Frozen task constraints |
| private expected-literals file | `cef37d37e111ca5f0a02312cc867240b2d650a917503e72c989c4d0e78744308` | Operator only; never give to contributors |
| full original 15-case inventory | `da860fc490fc28a48418711d8555708a1789a84b21a1df4e6f93a3a728a5b947` | Retain unchanged in harness repository |
| frozen native gate script | `166674592bb5ea0c17fc57bacdf0bbeae0a91d70747d71475b763e695264bfcf` | Install into baseline before freezing it |
| frozen gate configuration | `c1ada809f9fb3d1c23bd46a8068cbdb5f2badd78498bdeccdd74eae074ca1d20` | Install into baseline before freezing it |

The ten supported inventory entries are every original case with literal `exit_code == 0`, preserving original order. The remaining five original cases have nonzero expected exits and are **unsupported and unexercised** by this native verifier profile. Keep the full original inventory and private expected literals unchanged in the harness repository. Only the application artifact is given to the verifier. This runbook makes no 15-case or general nonzero-exit support claim.

Frozen runtime identity: Docker CLI `/opt/homebrew/bin/docker`; endpoint `unix:///Volumes/T7-repositories/Colima/home/default/docker.sock`; daemon ID `9c943a04-41ca-4943-a8f5-c5734144b14b`; image `sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251`; architecture `aarch64`. No image pull, VM start, VM resize, Docker context switch, or foreign-resource cleanup is allowed. Recheck the endpoint, daemon ID, image availability and capacity at launch; a mismatch is a failed preflight, not permission to adjust the VM.

## Operator isolation and launch manifest

The run root is the exact already-prepared, never-used directory `/private/tmp/aethyme-local-t64-run-hn4e6o_5`; do not create a substitute or clean/reset it. The independently reviewed phase/path clarification v3 explicitly binds this path and its child paths without changing the frozen profile. Its external file is `/private/tmp/aethyme-t64-phase-clarification-v3-8wa1pv3o/clarification.json`; do not expect a second copy in this worktree. At prebootstrap, independently `lstat` the root and each profile-bound payload directory (`source`, `harness`, `state`, `native-host`, `native-temporary`, `artifacts`, `bin`, `receipts`): require current non-root uid, mode `0700`, no symlink, and no entries. Record each status, device, inode, uid, mode, and checked enumeration status. The root itself may contain only those eight payload directories and the nine byte-frozen metadata files listed below; reject missing or extra root entries. Each metadata file must be a regular non-symlink owned by the current uid, mode `0644`, and match its expected SHA-256. In particular, preserve superseded `filesystem-preflight.json` and `prepared-manifest.json` byte-exact; do not overwrite or treat them as current preflight evidence.

| Exact root metadata entry | SHA-256 |
|---|---|
| `filesystem-preflight.json` (superseded) | `c6a30fd6e4aa2d8111c2a2d65bb1bc91dae03e7fa0cbb3b304b04a70e03b2a70` |
| `prepared-manifest.json` (superseded) | `ba0d3f282a58166d59501ce8509541c62ec069f0693a972e3193079b7ee5fe49` |
| `run-profile.json` (superseded) | `c424342e1405286a023072332521b36c94550b26b253404366f8348868f85719` |
| `run-profile-v2.json` (superseded) | `97141fc4726634b958525e24142e7b148c97c4c9db8352bca786cf339c4e5629` |
| `prelaunch-checklist.json` (superseded) | `c08155cdc69819413cfff104e4a438f0ef521930afe7a02be74f829d771d272f` |
| `run-profile-v3.json` | `c5b6887b680bd136622f6728a9fad430ba3c31cbfa44da2cd72a2efa836eccc1` |
| `prelaunch-checklist-v2.json` | `c159fcd6a69c3e76bee44bead99733af6a7ede6702461900b9591231419c1782` |
| `operator-completion-v1.json` | `6e106d8a9164b0c8cd149d7627657653cca996dcb8ac4ba7148749a254cfddcb` |
| `operator-labels-v1.json` | `fa4df5e8737560d5c52a39f99d1c76648970fd7cfa2edeb18c39fd8d15bd80cb` |

The eight payload directories must be empty at prebootstrap. Any error, unexpected root entry, hash mismatch, or existing payload ends the trial; never erase or reuse contents.

The fixed gate-control root is `/private/tmp/aethyme-local-t64-operator-v3-d4cbdvtq/control`; the frozen gate embeds it. The prebootstrap directory checker below records this root's initial empty state before any receipt is written. Only after that receipt succeeds, perform the separate checked control-root inspection and preserve its output/status. Repeat the same check immediately before arming. Do not use the superseded `/private/tmp/aethyme-local-t64-operator-k3igk063/control` path.

Use the independently cleared v3 clarification's append-only phase-manifest chain; never edit an earlier frozen file to add facts learned later:

1. `00-prebootstrap.json`, before any Playground CLI call or contributor launch, binds the reviewed Aethyme source commit/tree/clean status and built binary SHA/recipe/cache limitation; every frozen source, inventory, oracle, public-brief, gate and profile hash; exact prepared run/control paths; runtime/resource/budget bounds; intended repository/carrier names; and operator identity. It contains no generated ID or claim of successful work.
2. `01-precontributor.json`, after the baseline and separate harness commits exist but before either contributor edits, binds both commits/trees/file hashes, both new contributor sessions and their separate worktree paths, the identical source baseline, private bare carrier, public briefs, frozen inventory/oracle/verifier-plan bytes, and task assignments. Session paths/IDs are generated before contributor edits; do not invent them.
3. `02-capture-and-prepare.json`, after both verify-only submits and public verifier prepare, binds their exact output/status, source heads and required-capture receipts/operations/base/result/lineage, unchanged integration baseline, acknowledged harness receipt, and canonical inventory/runtime/policy IDs returned by prepare. Compare all returned values to `01-precontributor.json`; any mismatch fails the trial. Never hand-compute these IDs. Freeze and validate this phase before target selection or availability.
4. `03-predispatch.json`, before B's watcher starts and target-run dispatches, binds target revision/current epoch, project/objective policy IDs, producer generation, returned verifier IDs, exact resource/appointment JSON, carrier session/remote/destination, reviewed watcher/dispatcher hashes and argv, the pre-arm control-root check and runtime/capacity outputs, plus all preceding manifest hashes.
5. `03-arm.json`, after the checked O_EXCL `arm-a` creation but before B's watcher starts, binds the immediate pre-arm check receipt, arm receipt, control-root identity and exact directory contents. Never amend `03-predispatch.json` after it is frozen.
6. `04-results-and-teardown.json`, after reconciliation, records service nonce/status results, all failed or unrun stages, exact revoke/release/freeze outputs and exact-owned-resource absence or unknown/residual state. Earlier phase bytes remain unchanged.

Clarification v3 is a pretrial timing and path interpretation only. Source/harness commits, full/selected inventories, private oracle, public briefs, verifier plan, endpoint identity and all policy/budget bounds are frozen before contributors edit. The first successful required submit may initialize Local state; public prepare then returns canonical runtime and policy IDs. Compare them to the frozen bytes before target selection/dispatch. A mismatch fails closed; it never authorizes adapting the oracle, profile or policy. Never fill a missing ID by calculating an internal RecordId or editing Local state.

Use the freshly built binary explicitly for every new Local command and for contributor commands. Do not resolve `aethyme` from an ambient installed binary. Keep the caller's real `HOME`; set only the run-scoped state and temporary roots. The final launch manifest records the actual values:

```sh
export RUN_ROOT='/private/tmp/aethyme-local-t64-run-hn4e6o_5'
export PLAYGROUND="$RUN_ROOT/source"
export HARNESS_REPO="$RUN_ROOT/harness"
export AETHYME_BIN="$RUN_ROOT/bin/aethyme"
export AETHYME_HOST_STATE_DIR="$RUN_ROOT/state"
export TMPDIR="$RUN_ROOT/native-temporary"
export CONTROL_ROOT='/private/tmp/aethyme-local-t64-operator-v3-d4cbdvtq/control'
export DOCKER_HOST='unix:///Volumes/T7-repositories/Colima/home/default/docker.sock'
export DOCKER_CLI='/opt/homebrew/bin/docker'
export IMAGE='sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251'
export TARGET='t64-two-refactors'
export ISSUER='t64-independent-operator'
export INPUTS='/private/tmp/aethyme-local-t64-inputs-v2-qqslzo4a'
export ZERO_EXIT_ROOT='/private/tmp/aethyme-local-t64-zero-exit-v1-kx19p4p_'
export GATE_ROOT='/private/tmp/aethyme-local-t64-operator-v3-d4cbdvtq'
export CARGO_HOME='/Users/christophehenner/.cargo'
export RUSTUP_HOME='/Users/christophehenner/.rustup'
export PATH="$CARGO_HOME/bin:/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin"
# Copy the literal target directory from the exclusive source-build handoff.
export CARGO_TARGET_DIR='<copy exact shared T7 target path from runtime handoff>'
case "$CARGO_TARGET_DIR" in
  /Volumes/T7-repositories/*) ;;
  *) printf '%s\n' 'CARGO_TARGET_DIR must be the exact authorized T7 path' >&2; exit 1 ;;
esac
# Set from the separately reviewed source-pinned build manifest.
export AETHYME_SOURCE='<reviewed-clean-Aethyme-worktree>'
export OPERATOR_AGENT_IDENTITY='<operator Name <operator@example.com>>'
```

Run the shell blocks in one `/bin/bash` operator session with `set -euo pipefail` and `umask 077`. A block that reports an error ends the attempt; do not repeat a write into the same fixed receipt path.

The profile files are at `$RUN_ROOT/run-profile-v3.json`, `$RUN_ROOT/prelaunch-checklist-v2.json`, and `$RUN_ROOT/operator-completion-v1.json`; the frozen selection inputs are in `/private/tmp/aethyme-local-t64-zero-exit-v1-kx19p4p_`, public briefs and source/oracle inputs are in `/private/tmp/aethyme-local-t64-inputs-v2-qqslzo4a`, and gate inputs are in `/private/tmp/aethyme-local-t64-operator-v3-d4cbdvtq`. Do not copy the binary or either brief into the prepared run root before the prebootstrap checker succeeds. Only the operator reads or copies `harness/expected-literals.json`.

Build the exact reviewed commit before any Playground command. Record `git -C "$AETHYME_SOURCE" rev-parse HEAD`, tree, and full status; require a clean worktree. Do not run this block until the exclusive runtime handoff names a shared T7 `CARGO_TARGET_DIR`; never place a Rust target under `/private/tmp`. The target path must be written literally into `00-prebootstrap.json`. Use four jobs, an empty compiler wrapper and the workspace lockfile:

```sh
export CARGO_BUILD_JOBS=4
export RUSTC_WRAPPER=''
cd "$AETHYME_SOURCE/packages/aethyme/rust"
/usr/bin/caffeinate -i cargo build --locked -j4 -p aethyme-cli --bin aethyme
test -x "$CARGO_TARGET_DIR/debug/aethyme" || exit 1
CLI_SHA256="$(shasum -a 256 "$CARGO_TARGET_DIR/debug/aethyme" | awk '{print $1}')"
printf 'built_cli_sha256=%s\n' "$CLI_SHA256"
"$CARGO_TARGET_DIR/debug/aethyme" --version
git -C "$AETHYME_SOURCE" status --porcelain=v1
```

The final launch manifest records the exact build command, source before/after status, binary SHA-256, and warmed compiler-cache limitation. Do not claim a source-pinned binary until the build and checks are completed. The rehearsal itself uses this binary and does not compile. Keep the real `HOME`; do not copy credentials into the run root. The build writes only to the authorized shared T7 Cargo target; it does not install into `$RUN_ROOT/bin` before prebootstrap.

Before any Playground CLI call, make a checked preflight receipt. Record `id -u`, `/usr/bin/stat -f '%d %i %u %Lp'` for all eight prepared paths and the fixed control root, and checked `find` status/emptiness. Confirm `/private/tmp` and the T7 endpoint are mounted as expected; do not clear or repurpose anything. Record versions of `/usr/bin/python3`, `/usr/bin/git`, `jq`, the pinned Docker CLI, and the Rust toolchain used by the already-reviewed build. Read-only Docker and disk capacity checks use only the pinned CLI/endpoint:

```sh
"$DOCKER_CLI" --host "$DOCKER_HOST" info \
  --format '{{.ID}}|{{.ServerVersion}}|{{.OSType}}|{{.Architecture}}|{{.NCPU}}|{{.MemTotal}}|{{.DockerRootDir}}'
"$DOCKER_CLI" --host "$DOCKER_HOST" image inspect \
  --format '{{.Id}}|{{.Os}}|{{.Architecture}}|{{.Size}}' "$IMAGE"
/bin/df -Pk "$RUN_ROOT/native-temporary" "$RUN_ROOT/source" /private/tmp
```

Require daemon `9c943a04-41ca-4943-a8f5-c5734144b14b`, Docker 29.5.2, Linux/aarch64, four CPUs, daemon `MemTotal >= 2048 * 1024 * 1024`, and the exact cached image ID/architecture. Host disk must have at least `host_floor_bytes + disk_mib * 1,048,576` free (4 GiB) at each actual resource admission on `native-temporary`, the system temporary directory (which must resolve to that same private path), and the Playground store filesystem. Record `df` rows and command statuses before dispatch. The native resource-meter step must independently witness the 16 MiB daemon floor and 2,048 MiB pool; `docker info` does not prove daemon free disk. A missing image, different daemon, insufficient capacity, failed status, or unsupported meter is a failed preflight; do not pull the image, start/resize Colima, change contexts or enlarge a limit.

Run a checked preflight over every prepared directory before writing into any payload directory. This checker is an operator utility, not part of Aethyme. Preserve the exact checker bytes/hash, stdout, and exit status. It checks ownership/mode and enumerates each directory; it writes its result only after the initially empty `receipts` directory has been checked:

```sh
CHECKER_DIR="$(/usr/bin/mktemp -d /private/tmp/aethyme-t64-prebootstrap.XXXXXX)"
CHECKER="$CHECKER_DIR/checker.py"
cat > "$CHECKER" <<'PY'
#!/usr/bin/python3
import hashlib, json, os, pathlib, stat, sys

root = pathlib.Path(sys.argv[1])
expected_root = pathlib.Path("/private/tmp/aethyme-local-t64-run-hn4e6o_5")
if root != expected_root:
    raise SystemExit(f"refuse unexpected run root: {root}")
payloads = ("source", "harness", "state", "native-host", "native-temporary",
            "artifacts", "bin", "receipts")
metadata_sha256 = {
    "filesystem-preflight.json": "c6a30fd6e4aa2d8111c2a2d65bb1bc91dae03e7fa0cbb3b304b04a70e03b2a70",
    "prepared-manifest.json": "ba0d3f282a58166d59501ce8509541c62ec069f0693a972e3193079b7ee5fe49",
    "run-profile.json": "c424342e1405286a023072332521b36c94550b26b253404366f8348868f85719",
    "run-profile-v2.json": "97141fc4726634b958525e24142e7b148c97c4c9db8352bca786cf339c4e5629",
    "prelaunch-checklist.json": "c08155cdc69819413cfff104e4a438f0ef521930afe7a02be74f829d771d272f",
    "run-profile-v3.json": "c5b6887b680bd136622f6728a9fad430ba3c31cbfa44da2cd72a2efa836eccc1",
    "prelaunch-checklist-v2.json": "c159fcd6a69c3e76bee44bead99733af6a7ede6702461900b9591231419c1782",
    "operator-completion-v1.json": "6e106d8a9164b0c8cd149d7627657653cca996dcb8ac4ba7148749a254cfddcb",
    "operator-labels-v1.json": "fa4df5e8737560d5c52a39f99d1c76648970fd7cfa2edeb18c39fd8d15bd80cb",
}
if root.is_symlink() or not root.is_absolute():
    raise SystemExit("refuse symlink or non-absolute run root")
uid = os.getuid()
if uid == 0:
    raise SystemExit("refuse root operator uid")
root_st = os.lstat(root)
if not stat.S_ISDIR(root_st.st_mode) or root_st.st_uid != uid or stat.S_IMODE(root_st.st_mode) != 0o700:
    raise SystemExit(f"refuse run-root owner/mode/type: {root}")
expected_root_entries = set(payloads) | set(metadata_sha256)
actual_root_entries = {entry.name for entry in os.scandir(root)}
if actual_root_entries != expected_root_entries:
    missing = sorted(expected_root_entries - actual_root_entries)
    extra = sorted(actual_root_entries - expected_root_entries)
    raise SystemExit(f"refuse unexpected run-root entries: missing={missing} extra={extra}")
metadata_rows = []
for name, expected_sha256 in sorted(metadata_sha256.items()):
    path = root / name
    st = os.lstat(path)
    if not stat.S_ISREG(st.st_mode) or stat.S_ISLNK(st.st_mode):
        raise SystemExit(f"refuse non-regular metadata or symlink: {path}")
    mode = stat.S_IMODE(st.st_mode)
    if st.st_uid != uid or mode != 0o644:
        raise SystemExit(f"refuse metadata owner/mode mismatch: {path}")
    actual_sha256 = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual_sha256 != expected_sha256:
        raise SystemExit(f"refuse metadata hash mismatch: {path}")
    metadata_rows.append({"name": name, "device": st.st_dev, "inode": st.st_ino,
                          "uid": st.st_uid, "mode": format(mode, "04o"),
                          "sha256": actual_sha256})
paths = [root, *(root / name for name in payloads),
         pathlib.Path("/private/tmp/aethyme-local-t64-operator-v3-d4cbdvtq/control")]
rows = []
for path in paths:
    st = os.lstat(path)
    if not stat.S_ISDIR(st.st_mode) or stat.S_ISLNK(st.st_mode):
        raise SystemExit(f"refuse non-directory or symlink: {path}")
    mode = stat.S_IMODE(st.st_mode)
    if st.st_uid != uid or mode != 0o700:
        raise SystemExit(f"refuse owner/mode mismatch: {path}")
    entries = list(os.scandir(path))
    if path != root and entries:
        raise SystemExit(f"refuse nonempty payload path: {path}")
    rows.append({"path": str(path), "device": st.st_dev, "inode": st.st_ino,
                 "uid": st.st_uid, "mode": format(mode, "04o"),
                 "enumerated": True, "initially_empty": path != root})
out = root / "receipts" / "prebootstrap-paths.json"
fd = os.open(out, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, "w", encoding="utf-8") as stream:
    json.dump({"uid": uid, "paths": rows, "metadata": metadata_rows},
              stream, sort_keys=True, separators=(",", ":"))
    stream.write("\n")
    stream.flush()
    os.fsync(stream.fileno())
dir_fd = os.open(out.parent, os.O_RDONLY)
try:
    os.fsync(dir_fd)
finally:
    os.close(dir_fd)
PY
chmod 0600 "$CHECKER"
CHECKER_SHA256="$(shasum -a 256 "$CHECKER" | awk '{print $1}')"
printf 'checker_sha256=%s\n' "$CHECKER_SHA256"
if /usr/bin/python3 "$CHECKER" "$RUN_ROOT" >"$CHECKER_DIR/stdout.txt" 2>"$CHECKER_DIR/stderr.txt"; then
  CHECKER_STATUS=0
else
  CHECKER_STATUS=$?
fi
printf '%s\n' "$CHECKER_STATUS" >"$CHECKER_DIR/status"
test "$CHECKER_STATUS" -eq 0
```

Record `CHECKER_DIR`, `CHECKER_SHA256`, stdout, stderr, and status in `00-prebootstrap.json`. The checker rejects any missing or extra root entry, validates all nine metadata files against the pinned hashes and metadata, checks all eight payload directories and the fixed gate-control directory empty, and only then creates `receipts/prebootstrap-paths.json`. The receipt file and its parent directory are fsynced before success is reported. The historical `filesystem-preflight.json` and `prepared-manifest.json` remain byte-exact superseded metadata; `run-profile.json`, `run-profile-v2.json`, and `prelaunch-checklist.json` are also superseded. The v3 profile, checklist v2, completion v1 and independently reviewed clarification v3 govern this rehearsal. Any failure ends the trial without cleanup, substitution or reset.

After `receipts/prebootstrap-paths.json` has been durably written, the eight payload directories have been checked empty. Now install the previously built CLI and copy the public briefs into their designated receipt files; verify every copied byte before continuing. These are the first writes into `bin` and the brief files are not needed to run the checker:

```sh
test ! -e "$AETHYME_BIN"
/usr/bin/install -m 0700 "$CARGO_TARGET_DIR/debug/aethyme" "$AETHYME_BIN"
test "$(shasum -a 256 "$AETHYME_BIN" | awk '{print $1}')" = "$CLI_SHA256"
"$AETHYME_BIN" --version
test ! -e "$RUN_ROOT/receipts/public-brief-A.md"
test ! -e "$RUN_ROOT/receipts/public-brief-B.md"
/usr/bin/install -m 0600 "$INPUTS/CONTRIBUTOR-A.md" "$RUN_ROOT/receipts/public-brief-A.md"
/usr/bin/install -m 0600 "$INPUTS/CONTRIBUTOR-B.md" "$RUN_ROOT/receipts/public-brief-B.md"
test "$(shasum -a 256 "$RUN_ROOT/receipts/public-brief-A.md" | awk '{print $1}')" = d644536d7b72906b8a116a6968dcafc481f09bc761ae842bf114c9ba885a8313
test "$(shasum -a 256 "$RUN_ROOT/receipts/public-brief-B.md" | awk '{print $1}')" = 1a235f96bb823a9ce3dc6fa12b5b2d1a2567d900a3d36bab4135f4000b1a6482
```

Then define and run this checked control-root inspection. This ordering keeps `receipts` empty for the prebootstrap checker while retaining the control-root inspection. Run `check_control_root control-prearm` after setup and immediately before freezing `03-predispatch.json`; after that phase is frozen, run `check_control_root control-arm-boundary` immediately before the arm script. Bind the first check into `03-predispatch.json` and the second check's files/status into `03-arm.json`:

```sh
check_control_root() {
  local label="$1"
  test -d "$CONTROL_ROOT" && test ! -L "$CONTROL_ROOT" || return 1
  test "$(/usr/bin/stat -f %u "$CONTROL_ROOT")" = "$(/usr/bin/id -u)" || return 1
  test "$(/usr/bin/stat -f %Lp "$CONTROL_ROOT")" = 700 || return 1
  if /usr/bin/find "$CONTROL_ROOT" -mindepth 1 -maxdepth 1 -print -quit \
    > "$RUN_ROOT/receipts/$label-first-entry.txt"; then
    find_status=0
  else
    find_status=$?
  fi
  printf '%s\n' "$find_status" > "$RUN_ROOT/receipts/$label-first-entry.status"
  /usr/bin/stat -f '%d %i %u %Lp' "$CONTROL_ROOT" \
    > "$RUN_ROOT/receipts/$label.stat"
  test "$find_status" -eq 0 && test ! -s "$RUN_ROOT/receipts/$label-first-entry.txt"
}
check_control_root control-initial
```

If any check fails, stop; do not edit the frozen script, change this path, delete entries, or rerun with a different control root. The second inspection has separate output/status/stat receipts bound by `03-predispatch.json`.

Use the frozen existing directories without replacement: `PLAYGROUND=$RUN_ROOT/source`, `HARNESS_REPO=$RUN_ROOT/harness`, Aethyme state at `$RUN_ROOT/state`, output/artifact and bare carrier under `$RUN_ROOT/artifacts`, native temporary files at `$RUN_ROOT/native-temporary`, and receipts at `$RUN_ROOT/receipts`. Clarification v3 explicitly interprets the frozen `native-host` directory as the parent of `ResourcePolicy.host_state=$RUN_ROOT/native-host/host-resources.db` and journal directory `$RUN_ROOT/native-host/local-execution-journal`; this was independently cleared without changing limits or the parent path. Verify the DB file and journal directory are absent at prebootstrap. Do not create extra top-level profile paths or replace any frozen payload directory. Record the path and parent device/inode identities in `00-prebootstrap.json`.

## Playground and required-capture bootstrap

Use the already-empty `source` directory as a private Git repository. Copy in only the three frozen seed files from `$INPUTS/source`, verify their hashes against the table above, and do not copy an oracle or expected literals into Playground. Use the operator's existing Git identity; stop if it is unset. No remote is configured for this repository.

```sh
git -C "$PLAYGROUND" init -b main
mkdir -p "$PLAYGROUND/lib"
cp "$INPUTS/source/app.sh" "$PLAYGROUND/app.sh"
cp "$INPUTS/source/lib/greeting.sh" "$PLAYGROUND/lib/greeting.sh"
cp "$INPUTS/source/lib/math.sh" "$PLAYGROUND/lib/math.sh"
git -C "$PLAYGROUND" add app.sh lib
git -C "$PLAYGROUND" commit -m 'seed(t64): add frozen shell fixture'
cd "$PLAYGROUND"
"$AETHYME_BIN" broker scaffold --json > "$RUN_ROOT/receipts/scaffold.json"
"$AETHYME_BIN" collab enroll --write --json > "$RUN_ROOT/receipts/enroll.json"
```

Review the emitted `[collaboration]` section, set `capture = "required"` while retaining the returned `project` value, and set promotion to manual. Commit `.aethyme/config.toml` on the baseline. The relevant committed config must have this shape:

```toml
[collaboration]
capture = "required"
project = "<project value returned by enroll>"

[promote]
mode = "manual"
```

Commit this config with the seed before installing the reviewed gate policy:

```sh
git -C "$PLAYGROUND" add .aethyme/config.toml
git -C "$PLAYGROUND" commit -m 'config(t64): require capture and manual promotion'
```

`enroll --write` enables advisory capture; it is not a `required` switch. The operator explicitly changes the committed baseline config as above. Do not issue direct database/CAS writes. Only a successful `broker submit` creates collaboration state; Local lifecycle routes that need that store must refuse before initialization. If the baseline does not parse as required, stop.

Before freezing the baseline, install the independently frozen `gates.toml` and `gate.sh` into `.aethyme/gates.toml` and `.aethyme/t64-native-gate.sh`. Bind config SHA-256 `c1ada809f9fb3d1c23bd46a8068cbdb5f2badd78498bdeccdd74eae074ca1d20` and script SHA-256 `166674592bb5ea0c17fc57bacdf0bbeae0a91d70747d71475b763e695264bfcf` in `00-precontributor.json`. The config contains one uncached `t64-native-contract` gate with a 60-second timeout. The script checks the frozen app's version/greeting and only pauses after writing its own PID marker when `arm-a` exists. It embeds the fixed control root from the profile. Verify that path's owner/mode/emptiness, leave `arm-a` absent, and commit both gate files along with the required-capture config into the final frozen baseline. Do not substitute a different control path without a newly reviewed script and hash.

Install the independently frozen gate bytes at the paths named by the config, then commit them as the final baseline policy:

```sh
cp "$GATE_ROOT/gates.toml" "$PLAYGROUND/.aethyme/gates.toml"
cp "$GATE_ROOT/gate.sh" "$PLAYGROUND/.aethyme/t64-native-gate.sh"
shasum -a 256 "$PLAYGROUND/.aethyme/gates.toml" "$PLAYGROUND/.aethyme/t64-native-gate.sh"
git -C "$PLAYGROUND" add .aethyme/gates.toml .aethyme/t64-native-gate.sh
git -C "$PLAYGROUND" commit -m 'config(t64): install frozen behavioral gate'
export FROZEN_BASE="$(git -C "$PLAYGROUND" rev-parse HEAD)"
git -C "$PLAYGROUND" rev-parse 'HEAD^{tree}'
git -C "$PLAYGROUND" status --porcelain=v1
```

Require exact gate-file hashes from the frozen table and an empty final status; record the base commit/tree in `00-precontributor.json`. Before starting contributors, inspect and approve the committed gate policy on this machine. The public trust command is interactive and requires a TTY; an agent must not approve its own commands. Preserve both status outputs and require the committed baseline policy digest to be trusted:

```sh
"$AETHYME_BIN" broker trust status --repo "$PLAYGROUND" --json \
  > "$RUN_ROOT/receipts/gate-trust-before.json"
# Run in the operator's terminal, after reviewing the exact printed command.
"$AETHYME_BIN" broker trust --repo "$PLAYGROUND"
"$AETHYME_BIN" broker trust status --repo "$PLAYGROUND" --json \
  > "$RUN_ROOT/receipts/gate-trust-after.json"
```

Check every exit status and the exact policy digest in the final status. A missing, changed, or untrusted policy stops the rehearsal. Do not use `broker advanced trust`, the test-only noninteractive escape, or an agent session to approve it.

Freeze the baseline commit only after required-capture config and gate files are committed. Freeze the external behavior inventory and both bounded task briefs before contributor edits; no task brief contains private expected literals. Start both sessions from the same exact base before either submits. Give each a separate, disjoint, behavior-preserving source task. Each contributor works in its own broker worktree and records the prepared HEAD/tree.

Launch two distinct new agents through the collaboration agent tool, each with `model="gpt-6-luna"`, `reasoning_effort="max"`, and `fork_turns="none"`. `broker start --agent` records attribution; it does not create an agent. Give A only the frozen public brief A (SHA above) and B only public brief B (SHA above), with the pinned binary, private Playground path and run-scoped state environment. Each agent creates its own session from the same `$FROZEN_BASE`, reports session ID/worktree/prepared HEAD/tree, and waits. Freeze `01-precontributor.json` after both sessions and the harness commit exist but before sending either agent a START instruction. Then authorize each only for its own brief. Do not give either agent the private literal oracle or let it edit gates, config, inventory or task briefs.

For each separate agent-tool call, use the following fixed launch fields and substitute only the A/B task label and matching public brief path/hash. The initial message tells the agent to report the session details, make no edits, and wait for the later START instruction:

```text
model: gpt-6-luna
reasoning_effort: max
fork_turns: none
task A: t64_contributor_a; public brief: $INPUTS/CONTRIBUTOR-A.md; SHA-256 d644536d7b72906b8a116a6968dcafc481f09bc761ae842bf114c9ba885a8313
task B: t64_contributor_b; public brief: $INPUTS/CONTRIBUTOR-B.md; SHA-256 1a235f96bb823a9ce3dc6fa12b5b2d1a2567d900a3d36bab4135f4000b1a6482
initial instruction: Read only your assigned public brief. Do not edit, commit, submit, run Cargo/Docker, or access the private oracle. In the private Playground, use the pinned Aethyme binary, exact shared FROZEN_BASE, and your actual AGENTS.md agent identity to create one separate broker session. Report its session ID, worktree path, requested base, prepared HEAD/tree, and actual --agent identity. Wait for an explicit START after 01-precontributor.json is frozen.
```

```sh
cd "$PLAYGROUND"
"$AETHYME_BIN" broker start --base "$FROZEN_BASE" \
  --task "T64 public brief A behavior-preserving refactor" --short-name "t64-source-a" \
  --agent "$AGENT_IDENTITY" --json
"$AETHYME_BIN" broker start --base "$FROZEN_BASE" \
  --task "T64 public brief B behavior-preserving refactor" --short-name "t64-source-b" \
  --agent "$AGENT_IDENTITY" --json
```

Each agent executes exactly one command above with its own actual identity and its distinct short name; the operator does not execute both commands under one identity. The new session must report the requested `base` exactly equal to `$FROZEN_BASE`, and its prepared worktree must start at that commit before edits. If either starts from another commit, stop and keep its failed session record. After the explicit START, each contributor makes only its brief's change, commits it in its own worktree, and reports the final submitted HEAD/tree, changed paths, and clean status to the operator. The operator records those values before invoking verify-only submit for each exact session.

Create the private bare-Git carrier as a separate operator-owned session before freezing `01-precontributor.json`. It is only a transport destination for the Local run; it is not a public remote or an integration promotion:

```sh
export CARRIER_BARE="$RUN_ROOT/artifacts/carrier.git"
export CARRIER_REMOTE='t64-private'
export CARRIER_DEST='refs/heads/t64-private'
git init --bare "$CARRIER_BARE"
"$AETHYME_BIN" broker start --base "$FROZEN_BASE" \
  --task "T64 private publication carrier" --short-name "t64-carrier" \
  --agent "$OPERATOR_AGENT_IDENTITY" --json \
  > "$RUN_ROOT/receipts/carrier-session.json"
```

Extract the returned carrier session ID and worktree path from that JSON. In only that session worktree, add the private bare URL under the `t64-private` alias. Record the exact path, session, remote URL, destination ref and status:

```sh
git -C "$CARRIER_WORKTREE" remote add "$CARRIER_REMOTE" "$CARRIER_BARE"
git -C "$CARRIER_WORKTREE" remote get-url "$CARRIER_REMOTE"
git -C "$CARRIER_WORKTREE" remote get-url --push "$CARRIER_REMOTE"
git --git-dir="$CARRIER_BARE" rev-parse --is-bare-repository
git -C "$CARRIER_WORKTREE" status --porcelain=v1
```

Require fetch and push URLs to be the exact local path, the bare-repository result to be `true`, and the carrier worktree to remain clean at `$FROZEN_BASE`. Do not add an origin or any network URL. Record all actual paths and IDs in `01-precontributor.json`.

The actual contributor identities and task assignments go in `01-precontributor.json`. Each session uses its actual assigned `AGENTS.md` identity; do not claim the example identity above unless that is the identity reported by the agent. Do not run either submission with the gate arm file present. The configured behavioral native gate still runs at each ordinary verify-only submit; with `arm-a` absent it does not pause. Do **not** describe these submissions as having no native checks. The gate handshake is at most 450 iterations × 100 ms (45 seconds); the gate's own execution timeout is 60 seconds.

Submit each session in verify-only mode and preserve full JSON/stdout and status:

```sh
"$AETHYME_BIN" broker submit --session "$SESSION_A" --verify-only --json
"$AETHYME_BIN" broker submit --session "$SESSION_B" --verify-only --json
```

Before target selection, require for each submission: exit 0; the exact final HEAD/tree reported by that contributor before submit; a `Verified` queue entry; every mandatory gate passed; `promoted: false`; integration still exactly at `FROZEN_BASE`; and a required-capture receipt with `status: retained_local`, acknowledged durability, `retention: until_released`, exact source base/result snapshots, changed-path lineage and capture operation. The two receipt `base` values must be identical. A rejected/failed submission or unsuccessful capture is retained as a failed attempt and is never made available. Reconcile each operation through the public receipt route:

```sh
"$AETHYME_BIN" collab capture receipt --operation "$CAPTURE_OPERATION_A" \
  --repo "$PLAYGROUND" --json
"$AETHYME_BIN" collab capture receipt --operation "$CAPTURE_OPERATION_B" \
  --repo "$PLAYGROUND" --json
```

Read `operation_id`, `receipt_record_id`, `contribution`, `base`, `result`, `durability`, and `retention` from each actual receipt JSON. Preserve those values verbatim; do not hand-construct RecordIds. Append them and the exact verified submit outputs to `02-capture-and-prepare.json`; both receipts must be acknowledged before proposing or selecting the target.

## Retain the verifier harness through public CLI

Freeze a separate private harness Git repository before contributor edits. Preserve the exact source inputs under a provenance directory; put the original inventory and private oracle next to the selected `harness/inventory.json`. The verifier command below binds only that selected inventory path. Record the full commit/tree and every copied hash. Do not share the private oracle with contributors.

```sh
git -C "$HARNESS_REPO" init -b main
mkdir -p "$HARNESS_REPO/harness" "$HARNESS_REPO/selection"
cp "$INPUTS/harness/inventory.json" "$HARNESS_REPO/harness/original-inventory.json"
cp "$INPUTS/harness/expected-literals.json" "$HARNESS_REPO/harness/expected-literals.json"
cp "$ZERO_EXIT_ROOT/supported-inventory.json" "$HARNESS_REPO/harness/inventory.json"
cp "$ZERO_EXIT_ROOT/inventory-manifest.json" "$HARNESS_REPO/selection/inventory-manifest.json"
cp "$ZERO_EXIT_ROOT/profile.json" "$ZERO_EXIT_ROOT/excluded-cases.json" \
  "$ZERO_EXIT_ROOT/derive-supported-inventory.py" "$HARNESS_REPO/selection/"
git -C "$HARNESS_REPO" add harness selection
git -C "$HARNESS_REPO" commit -m 'seed(t64): freeze verifier inventory and oracle'
export HARNESS_COMMIT="$(git -C "$HARNESS_REPO" rev-parse HEAD)"
git -C "$HARNESS_REPO" rev-parse 'HEAD^{tree}'
git -C "$HARNESS_REPO" status --porcelain=v1
shasum -a 256 "$HARNESS_REPO/harness/inventory.json" \
  "$HARNESS_REPO/harness/original-inventory.json" \
  "$HARNESS_REPO/harness/expected-literals.json" \
  "$HARNESS_REPO/selection/inventory-manifest.json"
```

Require the selected inventory SHA `88f54c1c2cd72a65cb9387d982d79baa8eadcc840e5d055edfeb522ed0adb51c`, original inventory SHA `da860fc490fc28a48418711d8555708a1789a84b21a1df4e6f93a3a728a5b947`, oracle SHA `cef37d37e111ca5f0a02312cc867240b2d650a917503e72c989c4d0e78744308`, and selection-manifest SHA `2f64314e16e2acb68d2e92b11cb2907f64b00b45ae79bca38615a57beed27516`. The manifest hash differs from the selected inventory bytes hash; record both accurately. Stop if the worktree is not clean.

The plan file passed to `verifier prepare` is:

```json
{
  "image": "sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251",
  "command": {
    "program": "/bin/sh",
    "arguments": ["/artifact/app.sh"],
    "environment": {}
  },
  "max_age_ms": 300000
}
```

Use the public route to retain the exact commit/inventory and derive the explicit endpoint's runtime identity. This neither configures a producer nor starts a job:

```sh
/usr/bin/python3 - "$RUN_ROOT/receipts/harness-operation.txt" <<'PY'
import os, sys, uuid

path = sys.argv[1]
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, "w", encoding="ascii") as stream:
    stream.write(str(uuid.uuid4()) + "\n")
    stream.flush()
    os.fsync(stream.fileno())
PY
test "$?" -eq 0 || exit 1
IFS= read -r HARNESS_CAPTURE_OPERATION < "$RUN_ROOT/receipts/harness-operation.txt"
test -n "$HARNESS_CAPTURE_OPERATION" || exit 1
"$AETHYME_BIN" collab target verifier prepare \
  --harness-repo "$HARNESS_REPO" --commit "$HARNESS_COMMIT" \
  --inventory "$HARNESS_REPO/harness/inventory.json" \
  --operation "$HARNESS_CAPTURE_OPERATION" \
  --docker /opt/homebrew/bin/docker \
  --endpoint unix:///Volumes/T7-repositories/Colima/home/default/docker.sock \
  --file "$RUN_ROOT/verifier-plan.json" --repo "$PLAYGROUND" --json \
  > "$RUN_ROOT/receipts/verifier-prepare.json"
```

Require the prepared capture to be acknowledged, exact commit and inventory bindings to match, `producer_configured == false`, and no dispatch to have started. Verify exactly ten cases and the selected inventory byte hash. Extract the returned `policy` object without editing it into `verifier-policy.json`; the configure route accepts that object, not the enclosing prepare response. Use only returned snapshot, receipt, inventory, runtime and policy IDs. If prepare reports an unsupported inventory or endpoint mismatch, stop before target selection.

The prepare response has no receipt-record field. Use the public capture receipt lookup for the operation it returned; do not infer or hand-build a receipt ID:

```sh
"$AETHYME_BIN" collab capture receipt --operation "$HARNESS_CAPTURE_OPERATION" \
  --repo "$PLAYGROUND" --json > "$RUN_ROOT/receipts/harness-capture-receipt.json"
```

Require `status=retained_local`, acknowledged durability, the exact harness source commit, `retention=until_released`, and the returned `receipt_record_id`. Preserve both the prepare and lookup outputs and append them to `02-capture-and-prepare.json` before target selection. Generate `HARNESS_CAPTURE_OPERATION` as a new checked UUID operation value before prepare; the operation value is caller input, not an internal record identifier.

## Target, policy and budget files

Create `target-spec.json` from receipt output, the frozen operator labels and the returned verifier policy ID. Use exact accepted JSON fields; the example below has placeholders only for values returned by public commands:

```json
{
  "objective_authorization": "sha256:b4f3c4f1455da88b58924dce2fd8487f4f3afd3d935f584bfcfbffc71f346a9c",
  "objective": "Two behavior-preserving source refactors of the frozen Playground only",
  "baseline": "<shared base from both receipts>",
  "contributions": [
    {"id":"A","lineage":"<A contribution RecordId>","requires":[],"atomic_group":null,"revision_of":null,"derived_from":[]},
    {"id":"B","lineage":"<B contribution RecordId>","requires":[],"atomic_group":null,"revision_of":null,"derived_from":[]}
  ],
  "selected": ["A","B"],
  "requirements": [],
  "acceptance_policy": "<policy_id from verifier-prepare.json>",
  "composer_profile": "aethyme.compose.git-merge-file/provisional-v0"
}
```

These authorization strings are explicit operator labels, not signed records, proofs, provider prices, or permission to exceed the manifest. `objective_authorization` comes from the frozen objective label; project `authorization` and `price_profile` come from the operator completion file and operator-labels file. Do not substitute the verifier policy ID for the objective label.

`project-budget.json` is exactly `operator-completion-v1.json`'s `project_budget` object:

```json
{
  "authorization":"sha256:97141fc4726634b958525e24142e7b148c97c4c9db8352bca786cf339c4e5629",
  "price_profile":"sha256:c4e5556cf27d5345efcb1800f923482c3d6d923e3c2567fe96af661b440ff9e2",
  "cap":{"time_ms":22000000,"memory_bytes":8589934592,"disk_bytes":8589934592,"model_requests":0,"input_tokens":0,"output_tokens":0,"spend_microunits":0,"slots":64,"previews":0},
  "recovery_headroom":{"time_ms":1000000,"memory_bytes":536870912,"disk_bytes":536870912,"model_requests":0,"input_tokens":0,"output_tokens":0,"spend_microunits":0,"slots":2,"previews":0},
  "mandatory_headroom":{"time_ms":1000000,"memory_bytes":536870912,"disk_bytes":536870912,"model_requests":0,"input_tokens":0,"output_tokens":0,"spend_microunits":0,"slots":2,"previews":0},
  "max_root_reservations":64
}
```

`objective-cap.json` is the exact `objective_cap` object in operator-completion v1 (same nine `Units` values as the cap above). The two-wave envelope is 20,100,000 ms under the 22,000,000 ms cap. Recovery headroom is 1,000,000 ms; this leaves 900,000 ms over the declared two-wave envelope in the non-recovery lane. Mandatory headroom is reserved against optional analysis; this rehearsal schedules no optional analysis. These are cumulative accounting envelopes, not invoices, measured use, arbitrary retry capacity, or a hard whole-wave kill deadline.

Propose and record the returned revision ID, then configure in this order:

```sh
"$AETHYME_BIN" collab target propose --target "$TARGET" --issuer "$ISSUER" \
  --file "$RUN_ROOT/target-spec.json" --repo "$PLAYGROUND" --json
"$AETHYME_BIN" collab target budget project --target "$TARGET" --issuer "$ISSUER" \
  --file "$RUN_ROOT/project-budget.json" --repo "$PLAYGROUND" --json
"$AETHYME_BIN" collab target select --target "$TARGET" --issuer "$ISSUER" \
  --revision "$REVISION" --repo "$PLAYGROUND" --json
"$AETHYME_BIN" collab target budget objective --target "$TARGET" --issuer "$ISSUER" \
  --file "$RUN_ROOT/objective-cap.json" --repo "$PLAYGROUND" --json
"$AETHYME_BIN" collab target verifier configure --target "$TARGET" --issuer "$ISSUER" \
  --producer verifier-t64 --expected-generation none \
  --file "$RUN_ROOT/verifier-policy.json" --repo "$PLAYGROUND" --json
```

Before the first target-run owner tick, reconcile target/revision, authority epoch, project and objective budget policy IDs, producer generation, selected verifier policy/runtime/inventory IDs, exact retained source roots, both required receipts and current integration baseline. There is no separate public target-availability command in this profile. Do not continue if a status is truncated without an exact-ID lookup or if a policy is stale/frozen.

## Native resource and appointment files

`resources.json` is frozen from run-profile v3 plus the exact meter image in operator-completion v1. Replace only the two paths with the newly created empty owned directories:

```json
{
  "host_state":"<RUN_ROOT>/native-host/host-resources.db",
  "temporary_root":"<RUN_ROOT>/native-temporary",
  "meter_image":"sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251",
  "memory_mib":2048,
  "disk_mib":2048,
  "process_slots":1024,
  "job_slots":2,
  "host_floor_bytes":2147483648,
  "daemon_floor_bytes":16777216
}
```

The v3 profile serializes resources: every build or verifier envelope consumes both `job_slots=2`; `batch_width=1`; one resource job at a time. Do not claim two jobs, compute overlap or a larger capacity. Each build and verifier has 30-second timeout, 64 MiB memory, 1 MiB output and 32 pids. The host pool is 2,048 MiB memory / 2,048 MiB disk / 1,024 process slots. Recheck daemon and host floors; refusal is a failed preflight, not a reason to enlarge the profile or alter the VM.

Create `appointment.json` from the frozen run profile, prepare output, and launch manifest. Its `ServicePolicy` fields must be `nonce`, `drive`, `max_ticks`, `max_completions`, `poll_ms`, `max_elapsed_ms`; the selected bounds are:

```json
{
  "nonce":"t64-two-refactors-v1",
  "drive":{
    "execution":{
      "candidate_source":"<shared baseline snapshot>",
      "harness":"<returned harness snapshot>",
      "assertion_inventory":"<returned inventory ID>",
      "materialization_profile":"aethyme.raw-linux-inputs/provisional-v0",
      "profile":{
        "build_image":"sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251",
        "verifier_image":"sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251",
        "architecture":"aarch64",
        "runtime_profile":"<returned runtime ID>",
        "build_limits":{"timeout_ms":30000,"memory_bytes":67108864,"output_bytes":1048576,"pids":32},
        "verifier_limits":{"timeout_ms":30000,"memory_bytes":67108864,"output_bytes":1048576,"pids":32}
      },
      "build":{"program":"/bin/sh","arguments":["-c","set -eu; cp /source/app.sh /outputs/app.sh; mkdir /outputs/lib; cp /source/lib/greeting.sh /source/lib/math.sh /outputs/lib/"],"environment":{}},
      "verify":{"program":"/bin/sh","arguments":["/artifact/app.sh"],"environment":{}}
    },
    "wave_policy":{"batch_width":1,"max_group":2,"debounce_ms":0,"age_promotion_ms":5000,"drift_rechecks":2},
    "max_wall_ms":180000,
    "allow_empty_native_gates":false
  },
  "max_ticks":256,
  "max_completions":2,
  "poll_ms":100,
  "max_elapsed_ms":300000
}
```

The 180,000 ms wave and 300,000 ms service bounds are checked between stages; they are not hard in-flight process kills. Ten sequential verifier cases and per-build timeouts are individually bounded. Preserve overruns and fail closed; do not retry or extend limits.

## Notice, live-child witness, service and lost-output recovery

Both capture receipts must precede target selection. After selection/configuration, generate unique operation IDs in new O_EXCL receipt files, then append A's notice. Preserve the generated IDs as caller inputs and the returned notice IDs separately:

```sh
/usr/bin/python3 - "$RUN_ROOT/receipts/notice-operation-a.txt" \
  "$RUN_ROOT/receipts/notice-operation-b.txt" <<'PY'
import os, sys, uuid

for path in sys.argv[1:]:
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="ascii") as stream:
        stream.write(str(uuid.uuid4()) + "\n")
        stream.flush()
        os.fsync(stream.fileno())
PY
test "$?" -eq 0 || exit 1
IFS= read -r NOTICE_OPERATION_A < "$RUN_ROOT/receipts/notice-operation-a.txt"
IFS= read -r NOTICE_OPERATION_B < "$RUN_ROOT/receipts/notice-operation-b.txt"
test -n "$NOTICE_OPERATION_A" && test -n "$NOTICE_OPERATION_B" || exit 1
test "$NOTICE_OPERATION_A" != "$NOTICE_OPERATION_B" || exit 1
```

Record target `t64-two-refactors`, issuer `t64-independent-operator`, service nonce `t64-two-refactors-v1`, the returned revision/current epoch/producer generation, and both operation IDs before ingesting A's notice:

```sh
"$AETHYME_BIN" collab target capture-notice ingest \
  --target "$TARGET" --issuer "$ISSUER" --revision "$REVISION" \
  --alias A --operation "$NOTICE_OPERATION_A" --repo "$PLAYGROUND" --json
```

The notice receipt is not availability. The owner tick must revalidate the exact alias's complete retained input (base/result snapshot and lineage), current/tombstone state and selected revision before admission. The launch manifest records the notice ID and the subsequent owner status. After all predispatch setup is fixed, run the checked control inspection, then freeze `03-predispatch.json` with its status/stat/empty-output receipt hashes:

```sh
check_control_root control-prearm
```

After all submit gates have completed, verify and arm the control root with a checked, exclusive create. Run this only after both captures, target configuration, A notice, and the reviewed `03-predispatch.json` are frozen. Immediately before the script, run the second checked inspection; the script then creates the separate immutable arm receipt. If either creation fails, do not dispatch or retry:

```sh
check_control_root control-arm-boundary
/usr/bin/python3 - "$CONTROL_ROOT" "$RUN_ROOT/receipts/03-arm.json" <<'PY'
import hashlib, json, os, pathlib, stat, sys

control = pathlib.Path(sys.argv[1])
receipt = pathlib.Path(sys.argv[2])
os.umask(0o077)
st = os.lstat(control)
if not stat.S_ISDIR(st.st_mode) or stat.S_ISLNK(st.st_mode):
    raise SystemExit("refuse control root type")
if st.st_uid != os.getuid() or stat.S_IMODE(st.st_mode) != 0o700:
    raise SystemExit("refuse control root owner/mode")
if list(os.scandir(control)):
    raise SystemExit("refuse nonempty control root")
flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
fd = os.open(control / "arm-a", flags, 0o600)
with os.fdopen(fd, "wb") as stream:
    stream.write(b"armed\n")
    stream.flush()
    os.fsync(stream.fileno())
dirfd = os.open(control, os.O_RDONLY)
try:
    os.fsync(dirfd)
finally:
    os.close(dirfd)
armed = os.lstat(control / "arm-a")
if (not stat.S_ISREG(armed.st_mode) or armed.st_uid != os.getuid()
        or stat.S_IMODE(armed.st_mode) != 0o600):
    raise SystemExit("arm-a post-create identity check failed")
if set(os.listdir(control)) != {"arm-a"}:
    raise SystemExit("control root changed during arm")
receipt_fd = os.open(receipt, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
with os.fdopen(receipt_fd, "w", encoding="utf-8") as stream:
    arm_bytes = b"armed\n"
    json.dump({"schema":"aethyme.t64-control-arm/v1", "path":str(control / "arm-a"),
               "device":armed.st_dev, "inode":armed.st_ino, "uid":armed.st_uid,
               "mode":format(stat.S_IMODE(armed.st_mode), "04o"),
               "bytes":arm_bytes.decode("ascii"),
               "contents_sha256":hashlib.sha256(arm_bytes).hexdigest()},
              stream, sort_keys=True, separators=(",", ":"))
    stream.write("\n")
    stream.flush()
    os.fsync(stream.fileno())
receipt_dirfd = os.open(receipt.parent, os.O_RDONLY)
try:
    os.fsync(receipt_dirfd)
finally:
    os.close(receipt_dirfd)
PY
test "$?" -eq 0 || exit 1
```

Freeze `watch-b.py` below into `$RUN_ROOT/bin` and record its SHA-256 and exact argv in `03-predispatch.json`. The B contributor process runs this watcher itself after both submits and target setup. It writes a startup receipt followed by a separate, durable waiting-state receipt immediately before polling for the gate marker. It waits up to 305 seconds: the service's 300-second elapsed horizon plus the dispatcher's maximum five-second ready/waiting receipt window. An absent marker at expiry is unresolved and leaves `release-a` absent. After observing and validating the exact live A gate PID, it invokes the B notice command once with a five-second subprocess deadline and records timeout as unknown. It terminates only its exact owned CLI process group on notice timeout. Its `finally` path creates and syncs `release-a` only after exact gate-PID validation and only while that same child remains live. It never retries the notice or signals A.

```python
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
```

The watcher itself is a process launched by contributor B, not by the operator. After receiving an explicit START, B runs this exact command once from its contributor tool process. It uses the reviewed source-pinned CLI and run-scoped state, creates `watch-b-ready.json` and then `watch-b-waiting.json` immediately before polling for the gate marker. Do not wait for this command to finish before starting the service; the operator dispatcher waits for both receipts, then verifies the waiting receipt, exact live PID/argv, and control-root contents. B reports the eventual process exit and does not retry:

```sh
env HOME="$HOME" PATH="/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin" \
  DOCKER_HOST="$DOCKER_HOST" AETHYME_HOST_STATE_DIR="$AETHYME_HOST_STATE_DIR" \
  TMPDIR="$TMPDIR" /usr/bin/python3 "$RUN_ROOT/bin/watch-b.py" \
  "$CONTROL_ROOT" "$AETHYME_BIN" "$PLAYGROUND" "$TARGET" "$ISSUER" \
  "$REVISION" "$NOTICE_OPERATION_B" "$RUN_ROOT/receipts" \
  > "$RUN_ROOT/receipts/watch-b.stdout" \
  2> "$RUN_ROOT/receipts/watch-b.stderr"
```

After arming, the operator starts one bounded dispatcher. It waits at most five seconds for both watcher receipts, then verifies that the waiting receipt has the exact process, argv, revision and operation bindings, checks the exact watcher PID remains live, and immediately rechecks that the control root contains only `arm-a` and neither `gate-a-started` nor `release-a` exists. Only a watcher proven to be waiting for the marker permits service launch. It records the exact argv and child PID before waiting. The 360-second observation deadline does not kill the child: if it is still running, record `running_unknown`, leave it and its resources alone, and use the read-only nonce status route. Never rerun a timed-out or output-lost service command.

Freeze `dispatch-service.py` under `$RUN_ROOT/bin` and record its hash with the phase manifest. The script accepts the service argv after `--`, uses only the listed environment variables, creates logs exclusively, and never signals the service:

```python
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
```

Invoke the dispatcher with the complete explicit-endpoint command; do not wrap it in shell `&`, `wait`, `timeout`, or a retry loop:

```sh
/usr/bin/python3 "$RUN_ROOT/bin/dispatch-service.py" \
  "$RUN_ROOT" "$TARGET" "$ISSUER" "$REVISION" "$NOTICE_OPERATION_B" -- \
  "$AETHYME_BIN" collab target run \
  --target "$TARGET" --issuer "$ISSUER" \
  --appointment "$RUN_ROOT/appointment.json" --resources "$RUN_ROOT/resources.json" \
  --producer verifier-t64 --generation "$PRODUCER_GENERATION" \
  --session "$CARRIER_SESSION" --remote "$CARRIER_REMOTE" \
  --destination refs/heads/t64-private \
  --docker /opt/homebrew/bin/docker \
  --endpoint unix:///Volumes/T7-repositories/Colima/home/default/docker.sock \
  --timeout 30 --authorization "T64 private finite Local publication" \
  --allow-replacement no --repo "$PLAYGROUND" --json \
  > "$RUN_ROOT/receipts/dispatcher.stdout" \
  2> "$RUN_ROOT/receipts/dispatcher.stderr"
```

Preserve the dispatcher exit status, `service-started.json`, `service-observation.json` when present, service stdout/stderr, and B's watcher report/status. A missing file or still-running PID is unresolved, not a successful empty result. After the exact gate child has been observed and validated, the watcher attempts at most one `release-a` create even if the B notice fails; before that validation it withholds release. If release could not be durably created or the exact child changed, preserve the gate/job roots and report the result as unknown. Never kill by process name or touch another run's control root. No process or resource is declared absent based only on an empty global Docker list.

The service's own 300-second elapsed bound and this operator's 360-second observation deadline have different purposes. If the operator deadline wins, do not kill or settle; inspect the exact nonce/job state read-only and keep all unknown roots. All files remain private raw evidence.

The carrier session, remote alias, private bare repository, destination, source-pinned binary and exact report/receipt files are frozen in the launch manifest before dispatch. The authorization string is audit text, not a secret or authority token. No public remote, provider API, merge promotion or replacement is used.

If target-run stdout is lost, do not rerun the command. Recover read-only by the unique appointment nonce:

```sh
"$AETHYME_BIN" collab target run status \
  --target "$TARGET" --issuer "$ISSUER" --nonce t64-two-refactors-v1 \
  --repo "$PLAYGROUND" --json
```

The status lookup returns at most two nonce matches and searches the exact target/issuer/nonce across revisions and epochs. It must resolve exactly one durable appointment; missing or ambiguous results refuse. This bounds returned matches, not historical scan latency. A `dispatching` appointment is `unknown` and is never replayed. The output reports `automatic_replay: false`; this route does not mint authority or start work. An exact known appointment ID may instead be queried with `--appointment ID`, but `--appointment` and `--nonce` are mutually exclusive.

Run the public, read-only status routes and save each output/status before and after dispatch. Use the exact IDs returned by the preceding commands:

```sh
"$AETHYME_BIN" collab authority status --repo "$PLAYGROUND" --json
"$AETHYME_BIN" collab target status --target "$TARGET" --repo "$PLAYGROUND" --json
"$AETHYME_BIN" collab target budget status --target "$TARGET" --issuer "$ISSUER" \
  --repo "$PLAYGROUND" --json
"$AETHYME_BIN" collab target verifier status --target "$TARGET" --issuer "$ISSUER" \
  --producer verifier-t64 --repo "$PLAYGROUND" --json
"$AETHYME_BIN" collab target run status --target "$TARGET" --issuer "$ISSUER" \
  --nonce t64-two-refactors-v1 --repo "$PLAYGROUND" --json
"$AETHYME_BIN" collab target capture-notice list --target "$TARGET" \
  --limit 512 --repo "$PLAYGROUND" --json
"$AETHYME_BIN" collab target native-gates list --target "$TARGET" --issuer "$ISSUER" \
  --repo "$PLAYGROUND" --json
"$AETHYME_BIN" collab target native-gates status --target "$TARGET" --issuer "$ISSUER" \
  --job "$JOB_ID" --repo "$PLAYGROUND" --json
```

For budget reconciliation use the exact reservation ID when known, or the exact budget nonce returned by the relevant appointment/ledger output, adding exactly one of `--reservation ID` or `--nonce NONCE`. Budget status pages at most 500 newest reservation rows; it reports `reservation_rows_omitted` and exact ID/nonce lookup independent of that page. Root totals cover the complete ledger, but the displayed page is not a complete historical row listing. A status label does not itself grant dispatch. Preserve full private command transcripts and statuses; do not publish raw global Docker event streams, host inventories, grant databases, journals, or environment dumps.

## Exact release and teardown

While the authority is still Active:

1. Read service nonce/appointment, authority, target, budget and native-job status. Preserve unknown jobs as rooted and unresolved.
2. Revoke only the exact `verifier-t64` generation from status:

   ```sh
   "$AETHYME_BIN" collab target verifier revoke --target "$TARGET" --issuer "$ISSUER" \
     --producer verifier-t64 --generation "$PRODUCER_GENERATION" \
     --repo "$PLAYGROUND" --json
   ```

3. Release only exact completed native-job roots using `native-gates release`; prepared jobs may be abandoned, while dispatching/unknown jobs remain held:

   ```sh
   "$AETHYME_BIN" collab target native-gates release \
     --target "$TARGET" --issuer "$ISSUER" --job "$COMPLETED_JOB_ID" \
     --repo "$PLAYGROUND" --json
   ```

   Query `native-gates status --job ID` first for each ID. Do not release an unknown/dispatching job.

4. Release each acknowledged source/harness capture by its exact operation and receipt record returned by public capture output:

   ```sh
   "$AETHYME_BIN" collab capture release --operation "$CAPTURE_OPERATION_A" \
     --confirm "$RECEIPT_RECORD_A" --repo "$PLAYGROUND" --json
   "$AETHYME_BIN" collab capture release --operation "$CAPTURE_OPERATION_B" \
     --confirm "$RECEIPT_RECORD_B" --repo "$PLAYGROUND" --json
   "$AETHYME_BIN" collab capture release --operation "$HARNESS_CAPTURE_OPERATION" \
     --confirm "$HARNESS_RECEIPT_RECORD" --repo "$PLAYGROUND" --json
   ```

   The confirmation must be the exact `receipt_record_id` returned by the public `collab capture receipt --operation ID --repo PATH --json` lookup for that same committed operation. Neither verify-only submit nor verifier prepare is the receipt-record source. This command releases only that contribution root; other target/job roots may still retain source, and source erasure is not claimed.
5. Freeze the exact current authority epoch **last**, after all status/revoke/release work that requires an Active owner:

   ```sh
   "$AETHYME_BIN" collab stop --epoch "$CURRENT_EPOCH" \
     --reason "T64 rehearsal teardown complete" --repo "$PLAYGROUND" --json
   ```

   Obtain `CURRENT_EPOCH` from the immediately preceding authority status and bind it in the teardown record. Freeze does not prove process cancellation.
6. Reconcile only this run's control children, private host state, native temporary root, and exact objects/containers named by this run. Never prune, stop, delete, or inspect-clean foreign resources by global name. Unknown process/jobs, residual roots and journals stay preserved and reported.

For each gate PID from `watch-b.json`, save this exact-PID observation and exit status; do not signal it:

```sh
if /bin/ps -p "$GATE_PID" -o pid=,uid=,stat=,command= \
  > "$RUN_ROOT/receipts/gate-pid-$GATE_PID.stdout" \
  2> "$RUN_ROOT/receipts/gate-pid-$GATE_PID.stderr"; then
  ps_status=0
else
  ps_status=$?
fi
printf '%s\n' "$ps_status" > "$RUN_ROOT/receipts/gate-pid-$GATE_PID.status"
```

An exit 0 must still match the exact PID, current uid, and frozen `sh .aethyme/t64-native-gate.sh` command; it means the process remains live. Exit 1 with empty stdout is only a point-in-time no-row observation. Any other status/output is unknown. Pair it with the gate marker/release receipts and `native-gates status --job ID`; do not infer completion from process absence alone.

For each exact container or volume ID returned in this run's native-job status, use only ID-scoped read-only inspect calls and preserve stdout, stderr and status:

```sh
if "$DOCKER_CLI" --host "$DOCKER_HOST" inspect --type container "$OWNED_CONTAINER_ID" \
  > "$RUN_ROOT/receipts/container-$OWNED_CONTAINER_ID.stdout" \
  2> "$RUN_ROOT/receipts/container-$OWNED_CONTAINER_ID.stderr"; then
  container_inspect_status=0
else
  container_inspect_status=$?
fi
printf '%s\n' "$container_inspect_status" > \
  "$RUN_ROOT/receipts/container-$OWNED_CONTAINER_ID.status"
if "$DOCKER_CLI" --host "$DOCKER_HOST" volume inspect "$OWNED_VOLUME_ID" \
  > "$RUN_ROOT/receipts/volume-$OWNED_VOLUME_ID.stdout" \
  2> "$RUN_ROOT/receipts/volume-$OWNED_VOLUME_ID.stderr"; then
  volume_inspect_status=0
else
  volume_inspect_status=$?
fi
printf '%s\n' "$volume_inspect_status" > \
  "$RUN_ROOT/receipts/volume-$OWNED_VOLUME_ID.status"
```

Recheck `docker info` against the exact pinned endpoint before interpreting an inspect failure. Exit 0 means that exact object exists. A nonzero result means absent only when the stderr is the daemon's exact no-such-object response and the endpoint/daemon recheck succeeds; every other failure is unresolved. Never use a global container/volume list as proof that this run's objects are absent, and never manually remove an object based on a status failure.

Retain full original JSON, statuses, command logs, source and harness snapshots, and the final launch manifest privately. Public evidence may include only a reviewed, value-minimized derived summary plus hashes for excluded raw global streams. A user-facing runbook or report must disclose any intentional absolute-path/account-name exposure in byte-exact raw logs; do not call raw files path-free.

## Stop conditions and interpretation

Stop without modifying frozen files if any source/binary hash, config, baseline, receipt, target revision, policy/runtime/inventory ID, budget, endpoint, daemon, image, host capacity, gate hash, ownership/mode/emptiness, carrier identity, or predispatch status differs from the launch manifest. Preserve the failed attempt and exact status; no silent reset, DB edits, GC substitution, profile resize, image pull, automatic retry, or hidden credential transfer.

This procedure can at most support a bounded two-wave Local lifecycle rehearsal over the ten supported zero-exit cases. It does not establish reliable autonomy, task usefulness, model quality, provider accuracy/cost, throughput, compute overlap, host sandboxing, broad application acceptance, or full 15-case verifier support. Contributor/model/tool cost is not included in the Local native budget and must be disclosed separately if known; unknown cost remains unknown. No E1, FX08, FX17, held-out or paid model campaign is authorized by this runbook.
