#!/usr/bin/env python3
import hashlib
import os
import stat
import sys
import tempfile
from pathlib import Path
from typing import Optional


def replacement(mode: str, path: Path) -> bytes:
    data = path.read_bytes()
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise SystemExit(f"source is not UTF-8: {path}: {exc}")

    def replace_once(old: str, new: str, *, begin: Optional[str] = None, end: Optional[str] = None) -> str:
        nonlocal text
        left, right = 0, len(text)
        if begin is not None:
            left = text.index(begin)
        if end is not None:
            right = text.index(end, left)
        segment = text[left:right]
        count = segment.count(old)
        if count != 1:
            raise SystemExit(f"expected exactly one mutation target, found {count}: {old!r}")
        text = text[:left] + segment.replace(old, new, 1) + text[right:]
        return text

    if mode == "m1-release-without-archive-use":
        replace_once(
            "    let _archive = crate::collaboration_gc::archive_use(store)?;\n",
            "    // MUTATION: archive-use serialization guard removed.\n",
            begin="pub fn release_roots(",
            end="\nstruct Policy",
        )
    elif mode == "m2-release-without-owner-authorization":
        replace_once(
            "    let _authority = target::authorize(store, owner)?;\n",
            "    // MUTATION: target-owner authorization guard removed.\n",
            begin="pub fn release_roots(",
            end="\nstruct Policy",
        )
    elif mode == "m3-disabled-schema23-old-client-trigger":
        replace_once(
            "    WHEN OLD.roots_released_ms IS NOT NULL AND NEW.state<>OLD.state\n",
            "    WHEN 0\n",
        )
    elif mode == "m4-release-dispatching-unknown-roots":
        replace_once(
            '            "dispatching" | "unknown" => RootReleaseStatus::UnknownHeld,\n',
            '            "dispatching" | "unknown" => {\n'
            '                tx.execute(\n'
            '                    "UPDATE local_native_gate_jobs SET roots_released_ms=0 WHERE id=?1 AND roots_released_ms IS NULL",\n'
            '                    [key.as_str()],\n'
            '                )?;\n'
            '                RootReleaseStatus::Released\n'
            '            }\n',
        )
    elif mode == "m5-gc-ignores-retained-root-marker":
        replace_once(
            "JOIN local_native_gate_jobs AS jobs ON jobs.id=roots.job WHERE jobs.roots_released_ms IS NULL UNION",
            "JOIN local_native_gate_jobs AS jobs ON jobs.id=roots.job UNION",
        )
    elif mode == "m6-prepare-ignores-released-marker":
        replace_once(
            "        if job.roots_released_ms.is_some() {\n"
            "            return Err(NativeGateError::RootsReleased);\n"
            "        }\n",
            "        // MUTATION: prepare marker-classification guard removed.\n",
            begin="pub fn prepare(",
            end="\npub fn ",
        )
    else:
        raise SystemExit(f"unknown mutant: {mode}")
    return text.encode("utf-8")


def main() -> None:
    if len(sys.argv) != 4 or sys.argv[1] not in {"preview", "apply"}:
        raise SystemExit("usage: mutate-native-retention-guard.py preview|apply MUTANT SOURCE")
    action, mode, source = sys.argv[1:]
    path = Path(source)
    original = path.read_bytes()
    updated = replacement(mode, path)
    print(f"ORIGINAL_SHA256={hashlib.sha256(original).hexdigest()}")
    print(f"MUTANT_SHA256={hashlib.sha256(updated).hexdigest()}")
    if action == "preview":
        return
    mode_bits = stat.S_IMODE(path.stat().st_mode)
    fd, temp_name = tempfile.mkstemp(prefix=f".{path.name}.mutation.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(updated)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temp_name, mode_bits)
        os.replace(temp_name, path)
    finally:
        try:
            os.unlink(temp_name)
        except FileNotFoundError:
            pass
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual != hashlib.sha256(updated).hexdigest():
        raise SystemExit(f"applied source hash mismatch: {actual}")
    print(f"APPLIED_SHA256={actual}")


if __name__ == "__main__":
    main()
