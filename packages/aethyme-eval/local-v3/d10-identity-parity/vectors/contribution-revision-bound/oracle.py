#!/usr/bin/env python3
"""Independent stdlib-only D10 bound-file ContributionRevision oracle."""
import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent
INPUT_SCHEMA = "aethyme.local-d10-bound-revision-golden-inputs/v1"
RECORD_SCHEMA = "aethyme.contribution-revision/experimental-v0"
BASE32 = "abcdefghijklmnopqrstuvwxyz234567"
HEX = re.compile(r"[0-9a-f]*\Z")
SHA256_ID = re.compile(r"sha256:[0-9a-f]{64}\Z")
FILE_ID = re.compile(r"file:sha256:[0-9a-f]{64}\Z")
MODES = {"100644", "100755", "120000"}


def valid_logical_id(value, prefix):
    suffix = value.removeprefix(prefix)
    return (
        value.startswith(prefix)
        and len(suffix) == 26
        and all(ch in BASE32 for ch in suffix)
        and BASE32.index(suffix[-1]) % 4 == 0
    )


def valid_record_id(value):
    return bool(SHA256_ID.fullmatch(value))


def validate_path_hex(value):
    assert isinstance(value, str) and len(value) > 0 and len(value) % 2 == 0
    assert HEX.fullmatch(value)
    raw = bytes.fromhex(value)
    assert raw and b"\x00" not in raw and not raw.startswith(b"/")
    assert all(part not in (b"", b".", b"..") for part in raw.split(b"/"))
    return raw


def validate_file_shape(file):
    assert set(file) == {"path_hex", "mode", "content_hex"}
    raw_path = validate_path_hex(file["path_hex"])
    assert file["mode"] in MODES
    content_hex = file["content_hex"]
    assert isinstance(content_hex, str) and len(content_hex) % 2 == 0
    assert HEX.fullmatch(content_hex)
    return raw_path, file["mode"], bytes.fromhex(content_hex)


def validate_inputs(data):
    # All input/schema/identity assertions run before the first SHA-256 call.
    assert set(data) == {
        "fixture_schema", "project_id", "contribution_id", "proposal_seed",
        "capture_record_id", "anchor_commit", "base_files", "new_file",
        "validation_controls",
    }
    assert data["fixture_schema"] == INPUT_SCHEMA
    assert valid_logical_id(data["project_id"], "proj:")
    assert valid_logical_id(data["contribution_id"], "contrib:")
    assert data["proposal_seed"] == data["contribution_id"]
    assert valid_record_id(data["capture_record_id"])
    assert re.fullmatch(r"[0-9a-f]{40}", data["anchor_commit"])
    assert isinstance(data["base_files"], list) and len(data["base_files"]) == 1
    base_paths = [validate_file_shape(item)[0] for item in data["base_files"]]
    new_path, _, _ = validate_file_shape(data["new_file"])
    all_paths = base_paths + [new_path]
    assert len(set(all_paths)) == len(all_paths)
    controls = data["validation_controls"]
    assert set(controls) == {"invalid_contribution_id"}
    invalid = controls["invalid_contribution_id"]
    assert invalid.startswith("contrib:")
    invalid_suffix = invalid[len("contrib:"):]
    assert len(invalid_suffix) == 25
    assert all(ch in BASE32 for ch in invalid_suffix)
    assert BASE32.index(invalid_suffix[-1]) % 4 == 0
    assert not valid_logical_id(invalid, "contrib:")
    return [validate_file_shape(item) for item in data["base_files"]], validate_file_shape(data["new_file"])


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def snapshot(files):
    sorted_files = sorted(files, key=lambda item: item[0])
    manifest = bytearray(b"aethyme source-snapshot v0\x00")
    entries = []
    for path, mode, contents in sorted_files:
        content_sha = digest(contents)
        manifest.extend(mode.encode("ascii") + b" " + content_sha.encode("ascii") + b" " + path + b"\x00")
        entries.append({
            "content_sha256": content_sha,
            "mode": mode,
            "path_hex": path.hex(),
        })
    manifest_bytes = bytes(manifest)
    return {
        "entries": entries,
        "manifest_hex": manifest_bytes.hex(),
        "id": "sha256:" + digest(manifest_bytes),
    }


def main():
    data = json.loads((ROOT / "inputs.json").read_text(encoding="utf-8"))
    base_files, new_file = validate_inputs(data)

    base = snapshot(base_files)
    combined_files = base_files + [new_file]
    result = snapshot(combined_files)
    new_path, new_mode, new_contents = new_file
    content_sha = digest(new_contents)

    file_preimage = (
        b"aethyme proposed file identity v0\x00"
        + data["project_id"].encode("ascii") + b"\x00"
        + data["proposal_seed"].encode("ascii") + b"\x00"
        + result["id"].encode("ascii") + b"\x00"
        + (0).to_bytes(8, "big")
        + len(new_path).to_bytes(8, "big")
        + new_path
    )
    file_id = "file:sha256:" + digest(file_preimage)
    assert FILE_ID.fullmatch(file_id)

    change = {
        "after": {
            "content_sha256": content_sha,
            "file_id": file_id,
            "mode": new_mode,
            "path_hex": new_path.hex(),
        },
        "derived_from": [],
        "possible_sources": [],
        "relation": "new",
    }
    change_bytes = canonical(change)
    assert json.loads(change_bytes) == change

    revision = {
        "schema": RECORD_SCHEMA,
        "project_id": data["project_id"],
        "contribution_id": data["contribution_id"],
        "capture_record_id": data["capture_record_id"],
        "base": base["id"],
        "result": result["id"],
        "source_changes": [change],
        "depends_on": [],
    }
    revision_bytes = canonical(revision)
    assert json.loads(revision_bytes) == revision
    record_preimage = b"aethyme record v0\x00" + revision_bytes
    record_id = "sha256:" + digest(record_preimage)
    assert SHA256_ID.fullmatch(base["id"])
    assert SHA256_ID.fullmatch(result["id"])
    assert SHA256_ID.fullmatch(record_id)

    out = {
        "schema": "aethyme.local-d10-bound-revision-golden/v1",
        "input_schema": INPUT_SCHEMA,
        "source_change": {
            "value": change,
            "canonical_json": change_bytes.decode("utf-8"),
            "canonical_hex": change_bytes.hex(),
        },
        "contribution_revision": {
            "canonical_json": revision_bytes.decode("utf-8"),
            "canonical_hex": revision_bytes.hex(),
            "record_preimage_hex": record_preimage.hex(),
            "record_id": record_id,
        },
        "identity_derivations": {
            "base_snapshot": base,
            "result_snapshot": result,
            "new_file_content_sha256": content_sha,
            "proposal_ordinal": 0,
            "proposal_preimage_hex": file_preimage.hex(),
            "proposed_file_id": file_id,
        },
        "checks": {
            "all_input_assertions_precede_first_hash": True,
            "valid_contribution_suffix_length": 26,
            "valid_contribution_tail_symbol": data["contribution_id"][-1],
            "old_25_symbol_contribution_id_rejected": True,
            "file_id_format": bool(FILE_ID.fullmatch(file_id)),
            "record_id_format": bool(SHA256_ID.fullmatch(record_id)),
            "canonical_bytes_round_trip": True,
            "runtime_scope": "Python standard library only; no product code executed",
        },
    }
    print(json.dumps(out, ensure_ascii=False, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
