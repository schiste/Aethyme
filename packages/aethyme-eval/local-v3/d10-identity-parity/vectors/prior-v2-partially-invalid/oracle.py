#!/usr/bin/env python3
"""Independent stdlib-only Local D10 oracle for the frozen fcd5ed9 vectors.

This transcribes the v0 byte encodings read from the archived source files.
It imports no Aethyme code and does not execute Rust, Node, Cargo, or a runtime
product component. The fixture values are the exact integration-test values;
input schema and ID invariants are checked before the first digest operation.
"""
from __future__ import annotations

import hashlib
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
INPUT = HERE / "inputs.json"
OUTPUT = HERE / "expected.json"
PRIOR_DIR = Path("/private/tmp/review-d10-independent.rP4ZAU")
PRIOR_INPUT = PRIOR_DIR / "inputs.json"
PRIOR_FILES = ("manifest.json", "inputs.json", "oracle.py", "expected.json")
PRIOR_EXPECTED_SHA256 = {
    "manifest.json": "dcd046d68020c580fe3f5fa1598696ef864746240d7fc99e982d5e774fabc68f",
    "inputs.json": "be5e1daa719a1ae076c58e1728f330900af06aeb63cbb3253fa1c97c4aacb2dd",
    "oracle.py": "bd6685735a17a72014a05e1319b16d62983fef5381f8b2d8d4795ba39f032c95",
    "expected.json": "08b1d82960efa01461b4d1cdbf19d9da36a8a89bf2ed46e5918501ae3af78998",
}
SOURCE_COMMIT = "fcd5ed940b4d906452e4ad1d3aa90eb767f1af45"
RECORD_DOMAIN = b"aethyme record v0\0"
SNAPSHOT_DOMAIN = b"aethyme source-snapshot v0\0"
PROPOSED_FILE_DOMAIN = b"aethyme proposed file identity v0\0"
BASE32_ALPHABET = "abcdefghijklmnopqrstuvwxyz234567"


class InputValidationError(ValueError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def exact_keys(value: object, expected: set[str], where: str) -> dict:
    if type(value) is not dict:
        raise InputValidationError(f"{where} must be an object")
    actual = set(value)
    if actual != expected:
        raise InputValidationError(
            f"{where} keys mismatch: missing={sorted(expected - actual)}, "
            f"extra={sorted(actual - expected)}"
        )
    return value


def base32_value(char: str) -> int | None:
    if char in "abcdefghijklmnopqrstuvwxyz":
        return ord(char) - ord("a")
    if char in "234567":
        return ord(char) - ord("2") + 26
    return None


def validate_random_id(value: object, prefix: str, name: str) -> dict:
    if type(value) is not str or not value.startswith(prefix):
        raise InputValidationError(f"{name} must start with {prefix!r}")
    suffix = value[len(prefix):]
    if len(suffix) != 26:
        raise InputValidationError(
            f"{name} suffix length {len(suffix)}; expected 26"
        )
    values = [base32_value(char) for char in suffix]
    if any(item is None for item in values):
        raise InputValidationError(f"{name} suffix must use lowercase base32 alphabet")
    tail_value = values[-1]
    if tail_value % 4 != 0:
        raise InputValidationError(
            f"{name} final base32 value {tail_value} has nonzero low two bits"
        )
    return {
        "suffix_length": len(suffix),
        "alphabet": "lowercase RFC 4648 base32 symbols a-z, 2-7",
        "tail_symbol": suffix[-1],
        "tail_value": tail_value,
        "tail_low_two_bits": tail_value % 4,
    }


def validate_lower_hex(value: object, length: int, name: str) -> None:
    if (
        type(value) is not str
        or len(value) != length
        or any(char not in "0123456789abcdef" for char in value)
    ):
        raise InputValidationError(f"{name} must be {length} lowercase hex characters")


def validate_record_id(value: object, name: str) -> None:
    if type(value) is not str or not value.startswith("sha256:"):
        raise InputValidationError(f"{name} must use the sha256: prefix")
    validate_lower_hex(value[7:], 64, name + " digest")


def validate_file_vector(value: object, where: str, with_seed: bool) -> dict:
    expected = {"path_hex", "mode", "content_hex"}
    if with_seed:
        expected.add("proposal_seed")
    entry = exact_keys(value, expected, where)
    validate_lower_hex(entry["path_hex"], len(entry["path_hex"]), where + ".path_hex")
    validate_lower_hex(entry["content_hex"], len(entry["content_hex"]), where + ".content_hex")
    if len(entry["path_hex"]) % 2 or len(entry["content_hex"]) % 2:
        raise InputValidationError(f"{where} hex fields must contain whole bytes")
    path = bytes.fromhex(entry["path_hex"])
    if not path or b"\0" in path or path.startswith(b"/") or path.endswith(b"/"):
        raise InputValidationError(f"{where}.path_hex is not a valid relative file path")
    if any(component in (b"", b".", b"..", b".git") for component in path.split(b"/")):
        raise InputValidationError(f"{where}.path_hex contains a rejected path component")
    if entry["mode"] not in ("100644", "100755", "120000"):
        raise InputValidationError(f"{where}.mode is not a supported source mode")
    if with_seed:
        seed = entry["proposal_seed"]
        if (
            type(seed) is not str
            or not 1 <= len(seed) <= 128
            or not seed.isascii()
            or any(not (char.isalnum() or char in "._:-") for char in seed)
        ):
            raise InputValidationError(f"{where}.proposal_seed is invalid")
    return entry


def validate_inputs(spec: object) -> dict:
    values = exact_keys(
        spec,
        {
            "fixture_schema",
            "project_id",
            "contribution_id",
            "capture_record_id",
            "source_file",
            "brief",
            "minimal_new_file",
        },
        "inputs",
    )
    if values["fixture_schema"] != "aethyme.local-d10-independent-oracle-inputs/v2":
        raise InputValidationError("inputs.fixture_schema is not the v2 schema")

    # Required identity assertions are deliberately completed before any call
    # to sha256(): both suffixes are exactly 26 lowercase base32 symbols, and
    # their final symbols carry no nonzero low tail bits.
    project_check = validate_random_id(values["project_id"], "proj:", "project_id")
    contribution_check = validate_random_id(
        values["contribution_id"], "contrib:", "contribution_id"
    )
    validate_record_id(values["capture_record_id"], "capture_record_id")

    source_file = validate_file_vector(values["source_file"], "source_file", False)
    brief = exact_keys(values["brief"], {"record_id", "object_sha256"}, "brief")
    validate_record_id(brief["record_id"], "brief.record_id")
    validate_lower_hex(brief["object_sha256"], 64, "brief.object_sha256")
    new_file = validate_file_vector(
        values["minimal_new_file"], "minimal_new_file", True
    )

    # These are literal values transcribed from the archived Rust test, not
    # generated from production code or inferred from production output.
    require(values["project_id"] == "proj:" + "a" * 26, "project test input changed")
    require(
        values["contribution_id"] == "contrib:" + "b" * 25 + "e",
        "ContributionId test input must be 25 b symbols followed by e",
    )
    require(
        values["capture_record_id"] == "sha256:" + "1" * 64,
        "capture RecordId test input changed",
    )
    require(
        source_file == {
            "path_hex": "7372632f6c69622e7273",
            "mode": "100644",
            "content_hex": "666e206d61696e2829207b7d0a",
        },
        "baseline source test input changed",
    )
    require(
        brief == {
            "record_id": "sha256:" + "2" * 64,
            "object_sha256": "3" * 64,
        },
        "brief reference test input changed",
    )
    require(
        new_file == {
            "path_hex": "6e65772e7273",
            "mode": "100644",
            "content_hex": "6e65772066696c65",
            "proposal_seed": "proposal-new-file",
        },
        "minimal new-file test input changed",
    )
    return {
        "project_id": project_check,
        "contribution_id": contribution_check,
        "input_schema": "exact keys and exact Rust test values accepted",
    }


def reject_prior_bad_input(prior: object) -> dict:
    if type(prior) is not dict or type(prior.get("contribution_id")) is not str:
        raise AssertionError("prior invalid package has no string contribution_id")
    bad_id = prior["contribution_id"]
    try:
        validate_random_id(bad_id, "contrib:", "prior.contribution_id")
    except InputValidationError as exc:
        expected = "prior.contribution_id suffix length 25; expected 26"
        require(str(exc) == expected, f"prior input rejected for unexpected reason: {exc}")
        suffix = bad_id.removeprefix("contrib:")
        require(len(suffix) == 25, "prior suffix is not the reported 25 characters")
        require(all(base32_value(char) is not None for char in suffix), "prior alphabet differs")
        return {
            "path": str(PRIOR_INPUT),
            "contribution_id": bad_id,
            "suffix_length": len(suffix),
            "required_suffix_length": 26,
            "alphabet_valid": True,
            "tail_symbol": suffix[-1],
            "rejection": "contribution_id suffix length 25; expected 26",
            "accepted": False,
        }
    raise AssertionError("the old invalid ContributionId was unexpectedly accepted")


def sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def canonical(value: object) -> bytes:
    # All fixture record keys and string values are ASCII. For these values,
    # Python's code-point key ordering is identical to canonical UTF-16 order.
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")


def snapshot(files: list[dict]) -> dict:
    ordered = sorted(files, key=lambda entry: bytes.fromhex(entry["path_hex"]))
    manifest = bytearray(SNAPSHOT_DOMAIN)
    encoded_files = []
    for entry in ordered:
        path = bytes.fromhex(entry["path_hex"])
        content = bytes.fromhex(entry["content_hex"])
        content_digest = sha256(content)
        manifest.extend(entry["mode"].encode("ascii"))
        manifest.extend(b" ")
        manifest.extend(content_digest.encode("ascii"))
        manifest.extend(b" ")
        manifest.extend(path)
        manifest.append(0)
        encoded_files.append(
            {
                "path_hex": entry["path_hex"],
                "mode": entry["mode"],
                "content_sha256": content_digest,
            }
        )
    raw_manifest = bytes(manifest)
    return {
        "manifest_hex": raw_manifest.hex(),
        "id": "sha256:" + sha256(raw_manifest),
        "files_sorted_by_raw_path": encoded_files,
    }


def record(fields: dict) -> dict:
    encoded = canonical(fields)
    preimage = RECORD_DOMAIN + encoded
    decoded = json.loads(encoded.decode("utf-8"))
    require(decoded == fields, "record canonical JSON round trip differs")
    return {
        "canonical_json": encoded.decode("utf-8"),
        "canonical_hex": encoded.hex(),
        "record_preimage_hex": preimage.hex(),
        "id": "sha256:" + sha256(preimage),
    }


def proposed_file(project_id: str, seed: str, result_id: str, ordinal: int, path: bytes) -> dict:
    preimage = (
        PROPOSED_FILE_DOMAIN
        + project_id.encode("ascii") + b"\0"
        + seed.encode("ascii") + b"\0"
        + result_id.encode("ascii") + b"\0"
        + ordinal.to_bytes(8, "big")
        + len(path).to_bytes(8, "big")
        + path
    )
    return {
        "preimage_hex": preimage.hex(),
        "file_id": "file:" + sha256(preimage),
    }


def source_change(entry: dict, file_id: str) -> dict:
    content_digest = sha256(bytes.fromhex(entry["content_hex"]))
    return {
        "after": {
            "path_hex": entry["path_hex"],
            "mode": entry["mode"],
            "content_sha256": content_digest,
            "file_id": file_id,
        },
        "derived_from": [],
        "possible_sources": [],
        "relation": "new",
    }


def main() -> None:
    # Read/parse and validate the complete new input before the first digest.
    input_bytes = INPUT.read_bytes()
    spec = json.loads(input_bytes.decode("utf-8"))
    input_validation = validate_inputs(spec)

    prior_bytes = PRIOR_INPUT.read_bytes()
    prior_spec = json.loads(prior_bytes.decode("utf-8"))
    prior_rejection = reject_prior_bad_input(prior_spec)

    # Verify all four frozen bytes of the superseded package before using its
    # invalid fixture as the negative case. No file in that package is edited.
    prior_file_hashes = {
        name: sha256((PRIOR_DIR / name).read_bytes()) for name in PRIOR_FILES
    }
    require(
        prior_file_hashes == PRIOR_EXPECTED_SHA256,
        "prior invalid oracle package changed from its recorded four-file state",
    )

    project_id = spec["project_id"]
    contribution_id = spec["contribution_id"]
    capture_id = spec["capture_record_id"]
    source_file = spec["source_file"]
    brief = spec["brief"]
    new_file = spec["minimal_new_file"]

    baseline_snapshot = snapshot([source_file])
    empty_snapshot = snapshot([])
    generic_result = snapshot([new_file])
    bound_result = snapshot([source_file, new_file])

    common = {
        "schema": "aethyme.contribution-revision/experimental-v0",
        "project_id": project_id,
        "contribution_id": contribution_id,
        "capture_record_id": capture_id,
        "base": baseline_snapshot["id"],
        "result": baseline_snapshot["id"],
        "source_changes": [],
        "depends_on": [],
    }
    baseline_revision = record(common)
    rationale_fields = {
        **common,
        "brief_record_id": brief["record_id"],
        "brief_object_sha256": brief["object_sha256"],
    }
    rationale_revision = record(rationale_fields)
    changed_members = sorted(
        name for name in set(common) | set(rationale_fields)
        if common.get(name) != rationale_fields.get(name)
    )

    generic_path = bytes.fromhex(new_file["path_hex"])
    generic_new_paths = sorted([generic_path])
    generic_ordinal = generic_new_paths.index(generic_path)
    generic_file = proposed_file(
        project_id,
        new_file["proposal_seed"],
        generic_result["id"],
        generic_ordinal,
        generic_path,
    )
    generic_change = source_change(new_file, generic_file["file_id"])
    generic_revision = record({
        **common,
        "base": empty_snapshot["id"],
        "result": generic_result["id"],
        "source_changes": [generic_change],
    })

    # Bounded profile: retain the exact tested baseline file, add the exact
    # tested new.rs bytes, and seed the proposal with ContributionId.as_str().
    # Ordinals enumerate only new paths in raw-byte order after sorting.
    base_paths = {bytes.fromhex(source_file["path_hex"])}
    bound_result_paths = {
        bytes.fromhex(source_file["path_hex"]), generic_path
    }
    bound_new_paths = sorted(bound_result_paths - base_paths)
    bound_ordinal = bound_new_paths.index(generic_path)
    bound_file = proposed_file(
        project_id,
        contribution_id,
        bound_result["id"],
        bound_ordinal,
        generic_path,
    )
    bound_change = source_change(new_file, bound_file["file_id"])
    bound_revision = record({
        **common,
        "base": baseline_snapshot["id"],
        "result": bound_result["id"],
        "source_changes": [bound_change],
    })

    # Independent structural and identity consistency checks for these exact
    # small vectors; no product decoder or executable is involved.
    require(baseline_revision["id"] != rationale_revision["id"], "brief-only edit must change revision ID")
    require(baseline_revision["canonical_json"] != rationale_revision["canonical_json"], "brief-only edit must change canonical bytes")
    require(changed_members == ["brief_object_sha256", "brief_record_id"], "brief-only changed members differ")
    require(common["base"] == common["result"], "baseline record snapshot IDs differ")
    require(empty_snapshot["id"] != generic_result["id"], "empty and one-file snapshots must differ")
    require(generic_file["file_id"] != bound_file["file_id"], "generic and contribution-seeded FileIds must differ")
    require(bound_new_paths == [generic_path], "bound new-path set/order differs")
    require(bound_ordinal == 0, "single added path must have sorted new-path ordinal zero")
    require(generic_change.get("before") is None and "before" not in generic_change, "new change must omit before")
    require(bound_change.get("before") is None and "before" not in bound_change, "bound new change must omit before")
    require(bound_result["files_sorted_by_raw_path"][0]["path_hex"] == new_file["path_hex"], "result files not in raw-byte order")
    require(
        sha256(bytes.fromhex(baseline_snapshot["manifest_hex"]))
        == baseline_snapshot["id"][7:],
        "baseline snapshot manifest digest mismatch",
    )
    require(
        sha256(bytes.fromhex(generic_revision["record_preimage_hex"]))
        == generic_revision["id"][7:],
        "generic revision preimage digest mismatch",
    )

    output = {
        "oracle": {
            "implementation": "independent Python standard library only",
            "product_runtime_executed": False,
            "source_commit": SOURCE_COMMIT,
            "input_sha256": sha256(input_bytes),
            "oracle_sha256": sha256(Path(__file__).read_bytes()),
            "input_validation": input_validation,
            "prior_invalid_package": {
                **prior_rejection,
                "manifest_sha256": prior_file_hashes["manifest.json"],
                "four_file_sha256": prior_file_hashes,
            },
        },
        "domains": {
            "record_ascii": "aethyme record v0\\0",
            "record_hex": RECORD_DOMAIN.hex(),
            "source_snapshot_ascii": "aethyme source-snapshot v0\\0",
            "source_snapshot_hex": SNAPSHOT_DOMAIN.hex(),
            "proposed_file_identity_ascii": "aethyme proposed file identity v0\\0",
            "proposed_file_identity_hex": PROPOSED_FILE_DOMAIN.hex(),
        },
        "source_snapshots": {
            "baseline": baseline_snapshot,
            "empty": empty_snapshot,
            "generic_minimal_result": generic_result,
            "bound_profile_result": bound_result,
        },
        "contribution_revisions": {
            "baseline_immutable": baseline_revision,
            "rationale_only_immutable": rationale_revision,
            "rationale_only_changed_members": changed_members,
            "generic_minimal_new_file": generic_revision,
            "bound_profile_new_file": bound_revision,
        },
        "file_identity_vectors": {
            "generic_minimal": {
                "proposal_seed": new_file["proposal_seed"],
                "result_snapshot_id": generic_result["id"],
                "new_paths_sorted_raw_hex": [path.hex() for path in generic_new_paths],
                "ordinal": generic_ordinal,
                "raw_path_hex": generic_path.hex(),
                **generic_file,
            },
            "bound_profile": {
                "proposal_seed": contribution_id,
                "proposal_seed_source": "ContributionId.as_str()",
                "base_snapshot_id": baseline_snapshot["id"],
                "result_snapshot_id": bound_result["id"],
                "new_paths_sorted_raw_hex": [path.hex() for path in bound_new_paths],
                "ordinal": bound_ordinal,
                "raw_path_hex": generic_path.hex(),
                **bound_file,
            },
        },
        "source_changes": {
            "generic_minimal": {
                "canonical_json": canonical(generic_change).decode("utf-8"),
                "canonical_hex": canonical(generic_change).hex(),
            },
            "bound_profile": {
                "canonical_json": canonical(bound_change).decode("utf-8"),
                "canonical_hex": canonical(bound_change).hex(),
            },
        },
        "independent_consistency_checks": [
            "valid project and contribution IDs satisfy 26-symbol lowercase base32 length/alphabet/tail rules before digest calculation",
            "old 25-symbol contribution suffix is rejected before digest calculation",
            "all four prior package file hashes match the pre-recorded invalid package",
            "baseline and rationale-only records differ only by both optional brief fields",
            "baseline and rationale-only canonical bytes and immutable record IDs differ",
            "snapshot manifest and record preimage digests reproduce their reported IDs",
            "new-file changes omit before and use the proposal-derived after FileId",
            "bound result snapshot is sorted by raw path bytes and its one new path has ordinal zero",
            "generic seed and ContributionId.as_str() seed produce distinct FileIds",
        ],
    }
    OUTPUT.write_text(json.dumps(output, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({
        "output": str(OUTPUT),
        "output_sha256": sha256(OUTPUT.read_bytes()),
        "input_sha256": sha256(input_bytes),
        "oracle_sha256": sha256(Path(__file__).read_bytes()),
        "project_id_suffix_length": input_validation["project_id"]["suffix_length"],
        "contribution_id_suffix_length": input_validation["contribution_id"]["suffix_length"],
        "prior_rejection": prior_rejection["rejection"],
        "baseline_revision_id": baseline_revision["id"],
        "rationale_only_revision_id": rationale_revision["id"],
        "generic_new_file_id": generic_file["file_id"],
        "bound_profile_new_file_id": bound_file["file_id"],
    }, sort_keys=True))


if __name__ == "__main__":
    main()
