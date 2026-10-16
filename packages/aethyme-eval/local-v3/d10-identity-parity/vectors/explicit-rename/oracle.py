#!/usr/bin/env python3
"""Independent stdlib-only D10 explicit-rename transition and revision oracle."""
import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent
INPUT_SCHEMA = "aethyme.local-d10-explicit-rename-inputs/v1"
REVISION_SCHEMA = "aethyme.contribution-revision/experimental-v0"
SNAPSHOT_DOMAIN = b"aethyme source-snapshot v0\x00"
FILE_ID_DOMAIN = b"aethyme file identity v0\x00"
RECORD_DOMAIN = b"aethyme record v0\x00"
BASE32 = "abcdefghijklmnopqrstuvwxyz234567"
HEX = re.compile(r"[0-9a-f]*\Z")
SHA_ID = re.compile(r"sha256:[0-9a-f]{64}\Z")
FILE_ID = re.compile(r"file:sha256:[0-9a-f]{64}\Z")
MODES = {"100644", "100755", "120000"}


def valid_logical_id(value, prefix):
    suffix = value.removeprefix(prefix)
    return value.startswith(prefix) and len(suffix) == 26 and all(c in BASE32 for c in suffix) and BASE32.index(suffix[-1]) % 4 == 0


def valid_sha_id(value):
    return bool(SHA_ID.fullmatch(value))


def path_bytes(value):
    assert isinstance(value, str) and len(value) > 0 and len(value) % 2 == 0 and HEX.fullmatch(value)
    raw = bytes.fromhex(value)
    assert raw and b"\x00" not in raw and not raw.startswith(b"/")
    assert all(part not in (b"", b".", b"..") for part in raw.split(b"/"))
    return raw


def file_shape(value):
    assert set(value) == {"path_hex", "mode", "content_hex"}
    raw_path = path_bytes(value["path_hex"])
    assert value["mode"] in MODES
    content_hex = value["content_hex"]
    assert isinstance(content_hex, str) and len(content_hex) % 2 == 0 and HEX.fullmatch(content_hex)
    return raw_path, value["mode"], bytes.fromhex(content_hex)


def validate_input(data):
    # Exact schema, IDs, paths, content, modes, operation, and invalid control
    # are checked before the first SHA-256 call.
    assert set(data) == {"fixture_schema", "project_id", "contribution_id", "proposal_id", "capture_record_id", "base_files", "result_files", "operation", "validation_controls"}
    assert data["fixture_schema"] == INPUT_SCHEMA
    assert valid_logical_id(data["project_id"], "proj:")
    assert valid_logical_id(data["contribution_id"], "contrib:")
    proposal = data["proposal_id"]
    assert isinstance(proposal, str) and 1 <= len(proposal.encode("ascii")) <= 128
    assert all(byte in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._:-" for byte in proposal.encode("ascii"))
    assert valid_sha_id(data["capture_record_id"])
    assert isinstance(data["base_files"], list) and len(data["base_files"]) == 2
    assert isinstance(data["result_files"], list) and len(data["result_files"]) == 2
    base = [file_shape(file) for file in data["base_files"]]
    result = [file_shape(file) for file in data["result_files"]]
    assert len({entry[0] for entry in base}) == len(base)
    assert len({entry[0] for entry in result}) == len(result)
    base_map = {path: (mode, content) for path, mode, content in base}
    result_map = {path: (mode, content) for path, mode, content in result}
    operation = data["operation"]
    assert set(operation) == {"kind", "from_path_hex", "to_path_hex"}
    assert operation["kind"] == "rename"
    from_path = path_bytes(operation["from_path_hex"])
    to_path = path_bytes(operation["to_path_hex"])
    assert from_path != to_path and from_path in base_map and from_path not in result_map
    assert to_path not in base_map and to_path in result_map
    assert base_map[from_path] == result_map[to_path]
    remaining_base = {p: v for p, v in base_map.items() if p != from_path}
    remaining_result = {p: v for p, v in result_map.items() if p != to_path}
    assert remaining_base == remaining_result
    control = data["validation_controls"]
    assert set(control) == {"invalid_contribution_id"}
    invalid = control["invalid_contribution_id"]
    suffix = invalid.removeprefix("contrib:")
    assert invalid.startswith("contrib:") and len(suffix) == 25
    assert all(ch in BASE32 for ch in suffix) and BASE32.index(suffix[-1]) % 4 == 0
    assert not valid_logical_id(invalid, "contrib:")
    return base, result, from_path, to_path


def sha256(raw):
    return hashlib.sha256(raw).hexdigest()


def snapshot(entries):
    ordered = sorted(entries, key=lambda file: file[0])
    manifest = bytearray(SNAPSHOT_DOMAIN)
    files = []
    for path, mode, content in ordered:
        content_sha = sha256(content)
        manifest.extend(mode.encode("ascii") + b" " + content_sha.encode("ascii") + b" " + path + b"\x00")
        files.append({"content_sha256": content_sha, "mode": mode, "path_hex": path.hex()})
    manifest = bytes(manifest)
    return {"id": "sha256:" + sha256(manifest), "manifest_hex": manifest.hex(), "files_sorted_by_raw_path": files}


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def main():
    data = json.loads((ROOT / "inputs.json").read_text(encoding="utf-8"))
    base_files, result_files, from_path, to_path = validate_input(data)
    base = snapshot(base_files)
    result = snapshot(result_files)
    base_content_hashes = {path: sha256(content) for path, _, content in base_files}

    def enrolled_file_id(path):
        preimage = (
            FILE_ID_DOMAIN + data["project_id"].encode("ascii") + b"\x00" + base["id"].encode("ascii") + b"\x00"
            + len(path).to_bytes(8, "big") + path
        )
        file_id = "file:sha256:" + sha256(preimage)
        assert FILE_ID.fullmatch(file_id)
        return file_id, preimage

    base_identities = {path: enrolled_file_id(path) for path, _, _ in base_files}
    rename_file_id, rename_file_preimage = base_identities[from_path]
    keep_paths = sorted(set(path for path, _, _ in base_files) - {from_path})
    result_states = []
    for path, mode, content in sorted(result_files, key=lambda file: file[0]):
        identity_path = from_path if path == to_path else path
        file_id, _ = base_identities[identity_path]
        result_states.append({"content_sha256": sha256(content), "mode": mode, "path_hex": path.hex(), "file_id": file_id})
    content_hash = base_content_hashes[from_path]
    before = {"content_sha256": content_hash, "file_id": rename_file_id, "mode": next(mode for path, mode, _ in base_files if path == from_path), "path_hex": from_path.hex()}
    after = {"content_sha256": content_hash, "file_id": rename_file_id, "mode": next(mode for path, mode, _ in result_files if path == to_path), "path_hex": to_path.hex()}
    change = {"before": before, "after": after, "derived_from": [], "possible_sources": [], "relation": "explicit_rename"}
    change_bytes = canonical(change)
    assert json.loads(change_bytes) == change
    transition = {
        "base_snapshot_id": base["id"],
        "changes": [change],
        "project_id": data["project_id"],
        "proposal_id": data["proposal_id"],
        "result_files": result_states,
        "result_snapshot_id": result["id"],
    }
    transition_bytes = canonical(transition)
    assert json.loads(transition_bytes) == transition
    revision = {
        "schema": REVISION_SCHEMA,
        "project_id": data["project_id"],
        "contribution_id": data["contribution_id"],
        "capture_record_id": data["capture_record_id"],
        "base": base["id"],
        "result": result["id"],
        "source_changes": [change],
        "depends_on": [],
    }
    revision_bytes = canonical(revision)
    record_preimage = RECORD_DOMAIN + revision_bytes
    record_id = "sha256:" + sha256(record_preimage)
    assert json.loads(revision_bytes) == revision
    assert SHA_ID.fullmatch(base["id"]) and SHA_ID.fullmatch(result["id"]) and SHA_ID.fullmatch(record_id)
    out = {
        "schema": "aethyme.local-d10-explicit-rename-golden/v1",
        "source_change": {"value": change, "canonical_json": change_bytes.decode(), "canonical_hex": change_bytes.hex()},
        "transition": {"value": transition, "canonical_json": transition_bytes.decode(), "canonical_hex": transition_bytes.hex()},
        "contribution_revision": {"value": revision, "canonical_json": revision_bytes.decode(), "canonical_hex": revision_bytes.hex(), "record_preimage_hex": record_preimage.hex(), "record_id": record_id},
        "identity": {"base_snapshot": base, "result_snapshot": result, "base_enrolled_file_ids": {path.hex(): {"file_id": file_id, "preimage_hex": preimage.hex()} for path, (file_id, preimage) in base_identities.items()}, "renamed_file_id_preserved": rename_file_id},
        "checks": {"input_schema_and_ids_validated_before_first_hash": True, "contribution_id_suffix_length": 26, "invalid_old_25_symbol_id_rejected": True, "rename_preserves_file_id": before["file_id"] == after["file_id"], "source_and_destination_content_mode_equal": True, "proposal_id_uses_rust_generic_safe_token_form": True, "file_id_format": bool(FILE_ID.fullmatch(rename_file_id)), "canonical_bytes_round_trip": True, "runtime_scope": "Python standard library only; no production code executed"},
    }
    print(json.dumps(out, ensure_ascii=False, sort_keys=True, indent=2))

if __name__ == "__main__":
    main()
