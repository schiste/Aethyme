#!/usr/bin/env python3
"""Independent stdlib oracle for the written D10 snapshot/record byte rules.

No package imports or calls into Aethyme/Rust. Fixture values are fixed in
inputs.json. ASCII-only record fields make Python codepoint key ordering equal
to the documented UTF-16 order for these vectors.
"""
import hashlib
import json
from pathlib import Path

INPUT = Path(__file__).with_name("inputs.json")
OUTPUT = Path(__file__).with_name("expected.json")
RECORD_DOMAIN = b"aethyme record v0\0"
SNAPSHOT_DOMAIN = b"aethyme source-snapshot v0\0"
PROPOSED_FILE_DOMAIN = b"aethyme proposed file identity v0\0"


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def canonical(value) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def snapshot(files):
    entries = sorted(files, key=lambda item: bytes.fromhex(item["path_hex"]))
    manifest = bytearray(SNAPSHOT_DOMAIN)
    for entry in entries:
        raw_path = bytes.fromhex(entry["path_hex"])
        raw_content = bytes.fromhex(entry["content_hex"])
        manifest.extend(entry["mode"].encode("ascii"))
        manifest.extend(b" ")
        manifest.extend(sha256(raw_content).encode("ascii"))
        manifest.extend(b" ")
        manifest.extend(raw_path)
        manifest.append(0)
    raw_manifest = bytes(manifest)
    return {
        "manifest_hex": raw_manifest.hex(),
        "id": "sha256:" + sha256(raw_manifest),
        "file_content_sha256": [sha256(bytes.fromhex(f["content_hex"])) for f in entries],
    }


def record(fields):
    encoded = canonical(fields)
    preimage = RECORD_DOMAIN + encoded
    return {
        "canonical_json": encoded.decode("utf-8"),
        "canonical_hex": encoded.hex(),
        "record_preimage_hex": preimage.hex(),
        "id": "sha256:" + sha256(preimage),
    }


def main():
    spec = json.loads(INPUT.read_text(encoding="utf-8"))
    project = spec["project_id"]
    contribution = spec["contribution_id"]
    capture = spec["capture_record_id"]
    base = snapshot([spec["source_file"]])
    common = {
        "schema": "aethyme.contribution-revision/experimental-v0",
        "project_id": project,
        "contribution_id": contribution,
        "capture_record_id": capture,
        "base": base["id"],
        "result": base["id"],
        "source_changes": [],
        "depends_on": [],
    }
    no_brief = record(common)
    with_brief_fields = {
        **common,
        "brief_record_id": spec["brief"]["record_id"],
        "brief_object_sha256": spec["brief"]["object_sha256"],
    }
    changed_members = sorted(
        key for key in set(common) | set(with_brief_fields)
        if common.get(key) != with_brief_fields.get(key)
    )
    assert changed_members == ["brief_object_sha256", "brief_record_id"]
    with_brief = record(with_brief_fields)

    empty = snapshot([])
    new_file = spec["minimal_new_file"]
    result = snapshot([new_file])
    path = bytes.fromhex(new_file["path_hex"])
    file_preimage = (
        PROPOSED_FILE_DOMAIN
        + project.encode("ascii") + b"\0"
        + new_file["proposal_id"].encode("ascii") + b"\0"
        + result["id"].encode("ascii") + b"\0"
        + (0).to_bytes(8, "big")
        + len(path).to_bytes(8, "big")
        + path
    )
    file_id = "file:" + sha256(file_preimage)
    after = {
        "path_hex": new_file["path_hex"],
        "mode": new_file["mode"],
        "content_sha256": sha256(bytes.fromhex(new_file["content_hex"])),
        "file_id": file_id,
    }
    change = {
        "after": after,
        "derived_from": [],
        "possible_sources": [],
        "relation": "new",
    }
    file_revision = record({
        **common,
        "base": empty["id"],
        "result": result["id"],
        "source_changes": [change],
    })

    output = {
        "oracle": "independent Python standard library; formulas transcribed from frozen D10 written encodings",
        "input_sha256": sha256(INPUT.read_bytes()),
        "domains": {
            "snapshot_hex": SNAPSHOT_DOMAIN.hex(),
            "record_hex": RECORD_DOMAIN.hex(),
            "proposed_file_hex": PROPOSED_FILE_DOMAIN.hex(),
        },
        "same_snapshot_no_brief": {"snapshot": base, "revision": no_brief},
        "rationale_only_brief_revision": {"snapshot": base, "revision": with_brief},
        "brief_only_changed_record_members": changed_members,
        "minimal_new_file_revision": {
            "base_snapshot": empty,
            "result_snapshot": result,
            "proposed_file_identity_preimage_hex": file_preimage.hex(),
            "file_id": file_id,
            "source_change": {
                "canonical_json": canonical(change).decode("utf-8"),
                "canonical_hex": canonical(change).hex(),
            },
            "revision": file_revision,
        },
    }
    OUTPUT.write_text(json.dumps(output, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({
        "output": str(OUTPUT),
        "output_sha256": sha256(OUTPUT.read_bytes()),
        "input_sha256": sha256(INPUT.read_bytes()),
        "no_brief_record_id": no_brief["id"],
        "brief_record_id": with_brief["id"],
        "minimal_file_revision_id": file_revision["id"],
    }, sort_keys=True))


if __name__ == "__main__":
    main()
