#!/usr/bin/env python3
"""Independent Local D10 identity vectors, stdlib only; no product runtime."""
import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent
INPUTS_PATH = ROOT / "inputs.json"
INPUT_SCHEMA = "aethyme.local-d10-ordinal-supplement-inputs/v1"
BASE32 = set("abcdefghijklmnopqrstuvwxyz234567")
MODES = {"100644", "100755", "120000"}


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def is_sha_id(value):
    return isinstance(value, str) and re.fullmatch(r"sha256:[0-9a-f]{64}", value) is not None


def is_file_id(value):
    return isinstance(value, str) and re.fullmatch(r"file:sha256:[0-9a-f]{64}", value) is not None


def validate_random_id(value, prefix, field):
    require(isinstance(value, str) and value.startswith(prefix), f"{field}: missing {prefix} prefix")
    suffix = value[len(prefix):]
    require(len(suffix) == 26, f"{field}: suffix length {len(suffix)}, expected 26")
    require(all(c in BASE32 for c in suffix), f"{field}: alphabet must be lowercase RFC 4648 base32")
    tail_value = ord(suffix[-1]) - ord("a") if suffix[-1] <= "z" else ord(suffix[-1]) - ord("2") + 26
    require(tail_value % 4 == 0, f"{field}: final symbol has nonzero low two bits")
    return {"suffix_length": len(suffix), "alphabet": "lowercase RFC 4648 base32 a-z, 2-7", "tail_symbol": suffix[-1], "tail_value": tail_value, "tail_low_two_bits": tail_value % 4}


def validate_old_invalid_id():
    value = "contrib:" + "b" * 24 + "e"
    suffix = value[len("contrib:"):]
    require(len(suffix) == 25, "old invalid contribution input must remain 25 symbols")
    require(all(c in BASE32 for c in suffix), "old invalid contribution alphabet changed")
    tail_value = ord(suffix[-1]) - ord("a")
    require(tail_value % 4 == 0, "old invalid contribution tail must independently be valid")
    try:
        validate_random_id(value, "contrib:", "old invalid contribution_id")
    except AssertionError as error:
        require("suffix length 25" in str(error), "old invalid input rejected for unexpected reason")
        return {"contribution_id": value, "suffix_length": 25, "alphabet_valid": True, "tail_symbol": "e", "tail_value": 4, "accepted": False, "rejection": str(error)}
    raise AssertionError("old invalid 25-symbol ContributionId was accepted")


def validate_hex(value, field, allow_empty=False):
    require(isinstance(value, str), f"{field}: expected hex string")
    require(value == value.lower(), f"{field}: hex must be lowercase")
    require(len(value) % 2 == 0, f"{field}: hex length must be even")
    require(allow_empty or bool(value), f"{field}: empty bytes are not allowed")
    require(re.fullmatch(r"[0-9a-f]*", value) is not None, f"{field}: invalid lowercase hex")
    raw = bytes.fromhex(value)
    return raw


def validate_path_hex(value, field):
    raw = validate_hex(value, field)
    require(b"\0" not in raw, f"{field}: NUL is not allowed in paths")
    require(not raw.startswith(b"/"), f"{field}: absolute path not allowed")
    require(all(part not in (b"", b".", b"..") for part in raw.split(b"/")), f"{field}: invalid path component")
    return raw


def validate_file_input(file, field):
    require(set(file) == {"path_hex", "mode", "content_hex"}, f"{field}: unexpected file keys")
    path = validate_path_hex(file["path_hex"], field + ".path_hex")
    require(file["mode"] in MODES, f"{field}: unsupported mode")
    content = validate_hex(file["content_hex"], field + ".content_hex", allow_empty=True)
    return path, file["mode"], content


def validate_inputs(data):
    require(set(data) == {
        "fixture_schema", "project_id", "contribution_id", "anchor_commit",
        "descriptor_fixture_source_file", "descriptor_fixture_expected",
        "generic_new_file", "bound_profile_existing_file", "mixed_base_files",
        "mixed_result_files", "copy_operation", "split_operation", "plain_new_path_hex",
    }, "input schema keys changed")
    require(data["fixture_schema"] == INPUT_SCHEMA, "unsupported input schema")
    project_check = validate_random_id(data["project_id"], "proj:", "project_id")
    contribution_check = validate_random_id(data["contribution_id"], "contrib:", "contribution_id")
    commit = data["anchor_commit"]
    require(isinstance(commit, str) and re.fullmatch(r"[0-9a-f]{40}", commit) is not None, "anchor_commit must be 40 lowercase hex")
    descriptor_file = validate_file_input(data["descriptor_fixture_source_file"], "descriptor_fixture_source_file")
    generic = data["generic_new_file"]
    require(set(generic) == {"path_hex", "mode", "content_hex", "proposal_seed"}, "generic_new_file keys changed")
    generic_file = validate_file_input({k: generic[k] for k in ("path_hex", "mode", "content_hex")}, "generic_new_file")
    require(generic["proposal_seed"] == "proposal-new-file", "generic proposal seed differs from existing minimal test")
    bound = data["bound_profile_existing_file"]
    require(set(bound) == {"path_hex", "mode", "content_hex"}, "bound_profile_existing_file keys changed")
    bound_file = validate_file_input(bound, "bound_profile_existing_file")
    require(len(data["mixed_base_files"]) == 2 and len(data["mixed_result_files"]) == 5, "mixed file counts changed")
    base_files = [validate_file_input(f, f"mixed_base_files[{i}]") for i, f in enumerate(data["mixed_base_files"])]
    result_files = [validate_file_input(f, f"mixed_result_files[{i}]") for i, f in enumerate(data["mixed_result_files"])]
    require(len({p for p, _, _ in base_files}) == len(base_files), "duplicate mixed base path")
    require(len({p for p, _, _ in result_files}) == len(result_files), "duplicate mixed result path")
    copy = data["copy_operation"]
    require(set(copy) == {"from_path_hex", "to_path_hex"}, "copy_operation keys changed")
    copy_from = validate_path_hex(copy["from_path_hex"], "copy_operation.from_path_hex")
    copy_to = validate_path_hex(copy["to_path_hex"], "copy_operation.to_path_hex")
    split = data["split_operation"]
    require(set(split) == {"from_path_hex", "to_path_hexes_input_order"}, "split_operation keys changed")
    split_from = validate_path_hex(split["from_path_hex"], "split_operation.from_path_hex")
    require(len(split["to_path_hexes_input_order"]) >= 2, "split needs at least two destinations")
    split_to = [validate_path_hex(p, f"split_operation.to_path_hexes_input_order[{i}]") for i, p in enumerate(split["to_path_hexes_input_order"])]
    plain_path = validate_path_hex(data["plain_new_path_hex"], "plain_new_path_hex")
    expected = data["descriptor_fixture_expected"]
    require(set(expected) == {"anchor_snapshot_id", "enrolled_file_id", "descriptor_record_id"}, "descriptor fixture expected keys changed")
    require(is_sha_id(expected["anchor_snapshot_id"]), "fixture anchor_snapshot_id schema invalid")
    require(is_file_id(expected["enrolled_file_id"]), "fixture enrolled_file_id must use file:sha256:<64 lowercase hex>")
    require(is_sha_id(expected["descriptor_record_id"]), "fixture descriptor_record_id schema invalid")
    return {
        "project_check": project_check,
        "contribution_check": contribution_check,
        "old_invalid": validate_old_invalid_id(),
        "commit": commit,
        "descriptor_file": descriptor_file,
        "generic_file": generic_file,
        "bound_file": bound_file,
        "base_files": base_files,
        "result_files": result_files,
        "copy": (copy_from, copy_to),
        "split": (split_from, split_to),
        "plain_path": plain_path,
    }


def h(raw):
    return hashlib.sha256(raw).digest()


def hex_bytes(raw):
    return raw.hex()


def sha_id(raw):
    return "sha256:" + h(raw).hex()


def validate_sha(value, field):
    require(is_sha_id(value), f"{field}: expected sha256:<64 lowercase hex>")


def validate_file_id(value, field):
    require(is_file_id(value), f"{field}: expected file:sha256:<64 lowercase hex>")
    # Mirrors the constructor's format("file:{}", digest::encode(...)) and
    # parser's strip_prefix("file:") followed by canonical sha256 validation.
    inner = value.removeprefix("file:")
    require(is_sha_id(inner), f"{field}: parsed suffix must be sha256:<64 lowercase hex>")


def snapshot(files):
    """files is a list of (raw path, git mode, raw content), independently sorted."""
    ordered = sorted(files, key=lambda item: item[0])
    require(len({p for p, _, _ in ordered}) == len(ordered), "duplicate snapshot path")
    for path, mode, _content in ordered:
        require(path and b"\0" not in path and mode in MODES, "invalid snapshot path or mode")
    manifest = bytearray(b"aethyme source-snapshot v0\0")
    entries = []
    for path, mode, content in ordered:
        digest_hex = h(content).hex()
        manifest.extend(mode.encode("ascii") + b" " + digest_hex.encode("ascii") + b" " + path + b"\0")
        entries.append({"path_hex": path.hex(), "mode": mode, "content_sha256": digest_hex})
    manifest = bytes(manifest)
    identity = sha_id(manifest)
    validate_sha(identity, "source snapshot id")
    return {"id": identity, "manifest_hex": manifest.hex(), "files_sorted_by_raw_path": entries}, ordered


def enrolled_file_id(project_id, snapshot_id, path):
    preimage = b"aethyme file identity v0\0" + project_id.encode("ascii") + b"\0" + snapshot_id.encode("ascii") + b"\0" + len(path).to_bytes(8, "big") + path
    value = "file:" + sha_id(preimage)
    validate_file_id(value, "enrolled FileId constructor output")
    return {"file_id": value, "preimage_hex": preimage.hex()}


def proposed_file_id(project_id, proposal_seed, result_snapshot_id, ordinal, path):
    require(0 <= ordinal < 2**64, "ordinal outside u64")
    preimage = (b"aethyme proposed file identity v0\0" + project_id.encode("ascii") + b"\0" + proposal_seed.encode("ascii") + b"\0" + result_snapshot_id.encode("ascii") + b"\0" + ordinal.to_bytes(8, "big") + len(path).to_bytes(8, "big") + path)
    value = "file:" + sha_id(preimage)
    validate_file_id(value, "proposed FileId constructor output")
    return {"file_id": value, "preimage_hex": preimage.hex(), "ordinal": ordinal, "raw_path_hex": path.hex()}


def canonical_json(value):
    def check(node):
        if isinstance(node, dict):
            require(all(isinstance(k, str) and k.isascii() for k in node), "fixture uses non-ASCII object key")
            for item in node.values():
                check(item)
        elif isinstance(node, list):
            for item in node:
                check(item)
        elif isinstance(node, float):
            raise AssertionError("fixture must not use floats")
    check(value)
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")


def record_id(canonical_bytes):
    preimage = b"aethyme record v0\0" + canonical_bytes
    return {"id": sha_id(preimage), "preimage_hex": preimage.hex()}


def state(path, mode, content, file_id):
    validate_file_id(file_id, "FileState.file_id")
    require(mode in MODES, "FileState mode invalid")
    validate_path_hex(path.hex(), "FileState.path_hex")
    content_sha = h(content).hex()
    require(re.fullmatch(r"[0-9a-f]{64}", content_sha) is not None, "content hash schema invalid")
    return {"path_hex": path.hex(), "mode": mode, "content_sha256": content_sha, "file_id": file_id}


def source_change(relation, before, after, derived_from=(), possible_sources=()):
    for file_id in list(derived_from) + list(possible_sources):
        validate_file_id(file_id, "source change lineage FileId")
    change = {"relation": relation, "derived_from": list(derived_from), "possible_sources": list(possible_sources)}
    if before is not None:
        change["before"] = before
    if after is not None:
        change["after"] = after
    return change


def transition(project_id, contribution_id, base_files, result_files, operations, plain_path):
    base_snapshot, sorted_base = snapshot(base_files)
    result_snapshot, sorted_result = snapshot(result_files)
    base_by_path = {p: (mode, content) for p, mode, content in sorted_base}
    result_by_path = {p: (mode, content) for p, mode, content in sorted_result}
    base_ids = {p: enrolled_file_id(project_id, base_snapshot["id"], p)["file_id"] for p in base_by_path}
    require(len(set(base_ids.values())) == len(base_ids), "base FileIds must be unique")
    operation_by_destination = {}
    operation_by_source = {}
    used = set()
    for operation in operations:
        if operation["kind"] == "copy":
            source, dest = operation["from"], operation["to"]
            require(source in base_by_path and source in result_by_path, "copy source missing from exact base or result")
            require(dest in result_by_path and dest not in base_by_path, "copy destination must be new and present")
            require(base_by_path[source] == result_by_path[source] == result_by_path[dest], "copy must preserve source and exact mode/content")
            require(source not in used and dest not in used, "copy operation path reused")
            used.update((source, dest))
            identity = proposed_file_id(project_id, contribution_id, result_snapshot["id"], 0, dest)
            operation_by_destination[dest] = ("explicit_copy", identity["file_id"], [base_ids[source]], identity)
        elif operation["kind"] == "split":
            source, destinations = operation["from"], sorted(operation["to"])
            require(source in base_by_path and source not in result_by_path, "split source must be removed from result")
            require(len(destinations) >= 2 and len(set(destinations)) == len(destinations), "split needs unique destinations")
            require(source not in used, "split source path reused")
            used.add(source)
            for ordinal, dest in enumerate(destinations):
                require(dest in result_by_path and dest not in base_by_path, "split destination must be new and present")
                require(dest not in used, "split destination path reused")
                used.add(dest)
                identity = proposed_file_id(project_id, contribution_id, result_snapshot["id"], ordinal, dest)
                operation_by_destination[dest] = ("explicit_split", identity["file_id"], [base_ids[source]], identity)
            operation_by_source[source] = "split"
        else:
            raise AssertionError("unknown operation type")
    require(plain_path in result_by_path and plain_path not in base_by_path and plain_path not in operation_by_destination, "plain new path must be unspecified and new")
    new_paths = sorted(p for p in result_by_path if p not in base_by_path)
    global_ordinals = {path: index for index, path in enumerate(new_paths)}
    result_states = []
    identity_details = {}
    for path, mode, content in sorted_result:
        if path in base_ids:
            file_id = base_ids[path]
        elif path in operation_by_destination:
            relation, file_id, _derived, detail = operation_by_destination[path]
            identity_details[path.hex()] = {"relation": relation, **detail}
        else:
            detail = proposed_file_id(project_id, contribution_id, result_snapshot["id"], global_ordinals[path], path)
            file_id = detail["file_id"]
            identity_details[path.hex()] = {"relation": "new", **detail}
        result_states.append(state(path, mode, content, file_id))
    result_state_by_path = {bytes.fromhex(item["path_hex"]): item for item in result_states}
    changes = []
    for path, mode, content in sorted_base:
        before = state(path, mode, content, base_ids[path])
        if operation_by_source.get(path) == "split":
            changes.append(source_change("removed", before, None))
        elif path in result_by_path:
            after_mode, after_content = result_by_path[path]
            if (mode, content) != (after_mode, after_content):
                changes.append(source_change("same_path", before, result_state_by_path[path]))
        else:
            changes.append(source_change("removed", before, None))
    for path, mode, content in sorted_result:
        if path in base_by_path:
            continue
        after = result_state_by_path[path]
        operation_identity = operation_by_destination.get(path)
        if operation_identity:
            relation, _file_id, derived, _detail = operation_identity
            changes.append(source_change(relation, None, after, derived_from=derived))
        else:
            possible = [base_ids[source] for source, _base_mode, _base_content in sorted_base
                        if source not in result_by_path and base_by_path[source] == result_by_path[path]]
            changes.append(source_change("uncertain_rename" if possible else "new", None, after, possible_sources=possible))
    changes.sort(key=lambda change: (bytes.fromhex((change.get("after") or change.get("before"))["path_hex"]), change["relation"]))
    transition_value = {
        "project_id": project_id,
        "proposal_id": contribution_id,
        "base_snapshot_id": base_snapshot["id"],
        "result_snapshot_id": result_snapshot["id"],
        "result_files": result_states,
        "changes": changes,
    }
    validate_transition_output(transition_value)
    canonical = canonical_json(transition_value)
    return {
        "base_snapshot": base_snapshot,
        "result_snapshot": result_snapshot,
        "base_enrolled_file_ids": [{"path_hex": p.hex(), "file_id": base_ids[p]} for p in sorted(base_ids)],
        "new_path_global_ordinals_raw_hex": [{"path_hex": p.hex(), "ordinal": i} for p, i in global_ordinals.items()],
        "proposed_identity_preimages": identity_details,
        "transition": transition_value,
        "transition_canonical_json": canonical.decode("utf-8"),
        "transition_canonical_hex": canonical.hex(),
    }


def validate_state_output(state_value, field):
    require(set(state_value) == {"path_hex", "mode", "content_sha256", "file_id"}, f"{field}: unexpected state keys")
    validate_path_hex(state_value["path_hex"], field + ".path_hex")
    require(state_value["mode"] in MODES, f"{field}: mode invalid")
    require(re.fullmatch(r"[0-9a-f]{64}", state_value["content_sha256"]) is not None, f"{field}: content SHA invalid")
    validate_file_id(state_value["file_id"], field + ".file_id")


def validate_transition_output(value):
    require(set(value) == {"project_id", "proposal_id", "base_snapshot_id", "result_snapshot_id", "result_files", "changes"}, "transition output schema keys changed")
    require(value["project_id"].startswith("proj:") and value["proposal_id"].startswith("contrib:"), "transition IDs lost exact format")
    validate_sha(value["base_snapshot_id"], "transition base_snapshot_id")
    validate_sha(value["result_snapshot_id"], "transition result_snapshot_id")
    prior = None
    for i, file_value in enumerate(value["result_files"]):
        validate_state_output(file_value, f"transition.result_files[{i}]")
        path = bytes.fromhex(file_value["path_hex"])
        require(prior is None or prior < path, "result files not strict raw-path ordered")
        prior = path
    prior_key = None
    for i, change in enumerate(value["changes"]):
        require(set(change).issubset({"relation", "derived_from", "possible_sources", "before", "after"}), f"change[{i}]: unknown keys")
        require(set(change).issuperset({"relation", "derived_from", "possible_sources"}), f"change[{i}]: missing common keys")
        require(change["relation"] in {"same_path", "explicit_rename", "explicit_copy", "explicit_split", "new", "removed", "uncertain_rename"}, f"change[{i}]: invalid relation")
        for side in ("before", "after"):
            if side in change:
                validate_state_output(change[side], f"change[{i}].{side}")
        for list_name in ("derived_from", "possible_sources"):
            require(isinstance(change[list_name], list), f"change[{i}].{list_name}: expected list")
            for j, fid in enumerate(change[list_name]):
                validate_file_id(fid, f"change[{i}].{list_name}[{j}]")
        require("before" in change or "after" in change, f"change[{i}]: no state")
        path_hex = (change.get("after") or change.get("before"))["path_hex"]
        key = (bytes.fromhex(path_hex), change["relation"])
        require(prior_key is None or prior_key <= key, "changes not sorted by raw path/relation")
        prior_key = key


def main():
    data = json.loads(INPUTS_PATH.read_text(encoding="utf-8"))
    checks = validate_inputs(data)  # Every input ID, path, mode, and hex is checked before the first hash.
    project_id = data["project_id"]
    contribution_id = data["contribution_id"]
    commit = data["anchor_commit"]

    # Existing independent Rust descriptor vector from project_identity_vectors.rs.
    dpath, dmode, dcontent = checks["descriptor_file"]
    descriptor_snapshot, descriptor_order = snapshot([(dpath, dmode, dcontent)])
    descriptor_fid = enrolled_file_id(project_id, descriptor_snapshot["id"], dpath)
    descriptor = {
        "schema": "aethyme.project-descriptor/experimental-v0",
        "project_id": project_id,
        "anchor_commit": commit,
        "anchor_snapshot": descriptor_snapshot["id"],
        "source_binding": "git-commit:" + commit,
        "policy_location": ".aethyme/config.toml",
        "acceptance_target": "unbound",
        "visibility_boundary": "local",
        "files": [{"path_hex": dpath.hex(), "mode": dmode, "content_sha256": h(dcontent).hex(), "file_id": descriptor_fid["file_id"]}],
    }
    descriptor_bytes = canonical_json(descriptor)
    descriptor_record = record_id(descriptor_bytes)
    validate_sha(descriptor_snapshot["id"], "descriptor anchor snapshot")
    validate_file_id(descriptor["files"][0]["file_id"], "descriptor enrolled FileId")
    validate_sha(descriptor_record["id"], "descriptor record id")
    fixture_expected = data["descriptor_fixture_expected"]
    require(descriptor_snapshot["id"] == fixture_expected["anchor_snapshot_id"], "independent descriptor anchor snapshot disagrees with committed fixture")
    require(descriptor_fid["file_id"] == fixture_expected["enrolled_file_id"], "independent enrolled FileId disagrees with committed fixture")
    require(descriptor_record["id"] == fixture_expected["descriptor_record_id"], "independent ProjectDescriptor RecordId disagrees with committed fixture")

    # Existing generic minimal new-file proposal seed and bound-profile contribution seed.
    gpath, gmode, gcontent = checks["generic_file"]
    generic_snapshot, _ = snapshot([(gpath, gmode, gcontent)])
    generic_identity = proposed_file_id(project_id, data["generic_new_file"]["proposal_seed"], generic_snapshot["id"], 0, gpath)
    generic_change = source_change("new", None, state(gpath, gmode, gcontent, generic_identity["file_id"]))
    generic_change_bytes = canonical_json(generic_change)
    generic = {
        "base_snapshot_id": sha_id(b"aethyme source-snapshot v0\0"),
        "result_snapshot": generic_snapshot,
        "proposal_seed": data["generic_new_file"]["proposal_seed"],
        "ordinal": 0,
        "raw_path_hex": gpath.hex(),
        "file_id": generic_identity["file_id"],
        "preimage_hex": generic_identity["preimage_hex"],
        "source_change": generic_change,
        "source_change_canonical_json": generic_change_bytes.decode("utf-8"),
        "source_change_canonical_hex": generic_change_bytes.hex(),
    }
    bpath, bmode, bcontent = checks["bound_file"]
    bound_existing = [(dpath, dmode, dcontent), (bpath, bmode, bcontent)]
    bound_base_snapshot, bound_base_order = snapshot([(dpath, dmode, dcontent)])
    bound_result_snapshot, _ = snapshot(bound_existing)
    bound_seed = contribution_id
    bound_identity = proposed_file_id(project_id, bound_seed, bound_result_snapshot["id"], 0, bpath)
    bound_existing_id = enrolled_file_id(project_id, bound_base_snapshot["id"], dpath)
    bound_existing_state = state(dpath, dmode, dcontent, bound_existing_id["file_id"])
    bound_new_state = state(bpath, bmode, bcontent, bound_identity["file_id"])
    bound_result_states = sorted([bound_existing_state, bound_new_state], key=lambda item: bytes.fromhex(item["path_hex"]))
    bound_change = source_change("new", None, bound_new_state)
    bound_change_bytes = canonical_json(bound_change)
    bound = {
        "base_snapshot": bound_base_snapshot,
        "base_enrolled_file": {"path_hex": dpath.hex(), "file_id": bound_existing_id["file_id"], "preimage_hex": bound_existing_id["preimage_hex"]},
        "result_snapshot": bound_result_snapshot,
        "result_files": bound_result_states,
        "proposal_seed": bound_seed,
        "proposal_seed_source": "ContributionId.as_str()",
        "new_paths_sorted_raw_hex": [bpath.hex()],
        "ordinal": 0,
        "raw_path_hex": bpath.hex(),
        "file_id": bound_identity["file_id"],
        "preimage_hex": bound_identity["preimage_hex"],
        "source_change": bound_change,
        "source_change_canonical_json": bound_change_bytes.decode("utf-8"),
        "source_change_canonical_hex": bound_change_bytes.hex(),
    }

    # Mixed explicit Copy + Split + ordinary new path. Split input order intentionally reversed.
    copy_from, copy_to = checks["copy"]
    split_from, split_to_input = checks["split"]
    plain_path = checks["plain_path"]
    mixed_base = checks["base_files"]
    mixed_result = checks["result_files"]
    ops_a = [
        {"kind": "copy", "from": copy_from, "to": copy_to},
        {"kind": "split", "from": split_from, "to": split_to_input},
    ]
    ops_b = [
        {"kind": "split", "from": split_from, "to": sorted(split_to_input)},
        {"kind": "copy", "from": copy_from, "to": copy_to},
    ]
    mixed_a = transition(project_id, contribution_id, mixed_base, mixed_result, ops_a, plain_path)
    mixed_b = transition(project_id, contribution_id, mixed_base, mixed_result, ops_b, plain_path)
    require(mixed_a["transition"] == mixed_b["transition"], "operation permutation changed transition values/identities")
    require(mixed_a["transition_canonical_hex"] == mixed_b["transition_canonical_hex"], "operation permutation changed canonical transition bytes")

    # Assert exact intended ordinal classes rather than merely comparing output strings.
    ordinal_facts = {
        "explicit_copy": {"path_hex": copy_to.hex(), "ordinal": 0, "ordinal_scope": "fixed copy destination ordinal"},
        "explicit_split": [{"path_hex": path.hex(), "ordinal": ordinal, "ordinal_scope": "split-local after sorting this operation's destinations"} for ordinal, path in enumerate(sorted(split_to_input))],
        "plain_new": {"path_hex": plain_path.hex(), "ordinal": next(i for i, path in enumerate(sorted(p for p, _, _ in mixed_result if p not in {base_path for base_path, _, _ in mixed_base})) if sorted(p for p, _, _ in mixed_result if p not in {base_path for base_path, _, _ in mixed_base})[i] == plain_path), "ordinal_scope": "global raw-path-sorted new result paths"},
        "global_new_paths_sorted_raw_hex": [p.hex() for p in sorted(p for p, _, _ in mixed_result if p not in {base_path for base_path, _, _ in mixed_base})],
        "split_input_order_raw_hex": [p.hex() for p in split_to_input],
        "split_sorted_order_raw_hex": [p.hex() for p in sorted(split_to_input)],
    }
    new_paths = sorted(p for p, _, _ in mixed_result if p not in {base_path for base_path, _, _ in mixed_base})
    require(new_paths == [b"a-split.rs", b"m-plain.rs", b"z-copy.rs", b"z-split.rs"], "mixed global new-path order fixture changed")
    require(ordinal_facts["plain_new"]["ordinal"] == 1, "plain new path should use global ordinal one")
    require(ordinal_facts["explicit_copy"]["ordinal"] == 0 and [v["ordinal"] for v in ordinal_facts["explicit_split"]] == [0, 1], "operation-local ordinal policy changed")

    output = {
        "schema": "aethyme.local-d10-ordinal-supplement-expected/v1",
        "input_checks_before_hashing": {
            "project_id": checks["project_check"],
            "contribution_id": checks["contribution_check"],
            "prior_invalid_25_symbol_contribution": checks["old_invalid"],
            "all_input_modes_paths_and_hex": "validated before first SHA-256 operation",
        },
        "constructor_parse_format_contract": {
            "source_contract": "FileId::enrolled/proposed format file:<digest::encode(...)>; digest::encode yields sha256:<64 lowercase hex>; FileId::parse strips file: and accepts only canonical sha256:<64 lowercase hex>",
            "derived_format": "file:sha256:<64 lowercase hex>",
            "production_rust_executed": False,
            "checks": ["every emitted FileId checked against exact file:sha256:[0-9a-f]{64}", "every snapshot/record id checked against exact sha256:[0-9a-f]{64}"],
        },
        "descriptor_fixture_vector": {
            "source": "committed project_identity_vectors.rs independent descriptor fixture",
            "anchor_snapshot": descriptor_snapshot,
            "enrolled_file": {"path_hex": dpath.hex(), "mode": dmode, "content_sha256": h(dcontent).hex(), **descriptor_fid},
            "descriptor": descriptor,
            "canonical_json": descriptor_bytes.decode("utf-8"),
            "canonical_hex": descriptor_bytes.hex(),
            "record_id": descriptor_record["id"],
            "record_preimage_hex": descriptor_record["preimage_hex"],
            "committed_fixture_expected": fixture_expected,
            "matches_committed_fixture": True,
        },
        "generic_minimal_new_file": generic,
        "bound_profile_new_file": bound,
        "mixed_copy_split_plain_new": {
            "proposal_seed": contribution_id,
            "proposal_seed_source": "ContributionId.as_str()",
            "base_files_input_order": [f["path_hex"] for f in data["mixed_base_files"]],
            "result_files_input_order": [f["path_hex"] for f in data["mixed_result_files"]],
            "ordinal_facts": ordinal_facts,
            "result_files_sorted_by_raw_path": mixed_a["transition"]["result_files"],
            "changes_sorted_by_after_or_before_raw_path": mixed_a["transition"]["changes"],
            "base_snapshot": mixed_a["base_snapshot"],
            "result_snapshot": mixed_a["result_snapshot"],
            "base_enrolled_file_ids": mixed_a["base_enrolled_file_ids"],
            "proposed_identity_preimages": mixed_a["proposed_identity_preimages"],
            "transition": mixed_a["transition"],
            "transition_canonical_json": mixed_a["transition_canonical_json"],
            "transition_canonical_hex": mixed_a["transition_canonical_hex"],
            "permuted_operation_order": {
                "first": ["Copy", "Split(input destinations z-split then a-split)"],
                "second": ["Split(input destinations a-split then z-split)", "Copy"],
                "transition_values_identical": True,
                "canonical_bytes_identical": True,
            },
        },
        "independent_consistency_checks": [
            "ProjectId and ContributionId validated for 26-symbol lowercase base32 length/alphabet and zero tail bits before any hash; old 25-symbol value rejected for length",
            "descriptor source snapshot, enrolled FileId, canonical bytes and immutable RecordId reproduce committed independent fixture values",
            "every output FileId uses exact file:sha256:<64 lowercase hex> constructor/parse canonical format",
            "all output path_hex values decode as canonical lowercase hex and all modes are supported Git modes",
            "canonical descriptor/change/transition bytes reparse to the same JSON value",
            "generic proposal-new-file seed retained and contribution ID seed yields separate bound-profile FileId",
            "mixed explicit Copy uses ordinal zero; Split uses sorted operation-local zero/one; ordinary new path uses global sorted new-path ordinal one",
            "mixed operation reordering and split destination input reordering produce identical transition values, FileIds, and canonical bytes",
            "copy/split-derived paths cite exactly their respective base enrolled FileId; plain new path has empty derivation and possible-source lists",
        ],
        "execution_boundary": {"python_stdlib_only": True, "rust_or_node_or_cargo_or_native_or_model_runtime": False, "network": False},
    }
    validate_all_outputs(output)
    encoded = canonical_json(output) + b"\n"
    (ROOT / "expected.json").write_bytes(encoded)
    print("independent D10 ordinal supplement: all input/output schema and consistency assertions passed")


def validate_preimage_file_id(preimage_hex, file_id, field):
    require(isinstance(preimage_hex, str) and re.fullmatch(r"[0-9a-f]+", preimage_hex) is not None and len(preimage_hex) % 2 == 0, f"{field}: invalid preimage hex")
    require("file:" + sha_id(bytes.fromhex(preimage_hex)) == file_id, f"{field}: digest does not produce FileId")


def validate_snapshot_output(value, field):
    require(set(value) == {"id", "manifest_hex", "files_sorted_by_raw_path"}, f"{field}: snapshot output schema changed")
    validate_sha(value["id"], field + ".id")
    manifest_hex = value["manifest_hex"]
    require(isinstance(manifest_hex, str) and re.fullmatch(r"[0-9a-f]*", manifest_hex) is not None and len(manifest_hex) % 2 == 0, f"{field}: manifest hex invalid")
    manifest = bytes.fromhex(manifest_hex)
    require(sha_id(manifest) == value["id"], f"{field}: manifest does not hash to snapshot ID")
    prior = None
    rebuilt = bytearray(b"aethyme source-snapshot v0\0")
    for i, entry in enumerate(value["files_sorted_by_raw_path"]):
        require(set(entry) == {"path_hex", "mode", "content_sha256"}, f"{field}.files[{i}]: unexpected keys")
        path = validate_path_hex(entry["path_hex"], f"{field}.files[{i}].path_hex")
        require(entry["mode"] in MODES, f"{field}.files[{i}].mode invalid")
        require(re.fullmatch(r"[0-9a-f]{64}", entry["content_sha256"]) is not None, f"{field}.files[{i}].content_sha256 invalid")
        require(prior is None or prior < path, f"{field}: file entries not sorted by raw path")
        prior = path
        rebuilt.extend(entry["mode"].encode("ascii") + b" " + entry["content_sha256"].encode("ascii") + b" " + path + b"\0")
    require(bytes(rebuilt) == manifest, f"{field}: manifest bytes do not encode the asserted files")


def validate_all_outputs(output):
    require(output["schema"] == "aethyme.local-d10-ordinal-supplement-expected/v1", "expected output schema invalid")
    for key in ("anchor_snapshot_id", "descriptor_record_id"):
        validate_sha(output["descriptor_fixture_vector"]["committed_fixture_expected"][key], "fixture." + key)
    descriptor_vector = output["descriptor_fixture_vector"]
    validate_snapshot_output(descriptor_vector["anchor_snapshot"], "descriptor snapshot")
    validate_file_id(descriptor_vector["enrolled_file"]["file_id"], "descriptor enrolled FileId")
    validate_sha(descriptor_vector["record_id"], "descriptor RecordId")
    descriptor_bytes = descriptor_vector["canonical_json"].encode("utf-8")
    require(descriptor_bytes.hex() == descriptor_vector["canonical_hex"], "descriptor canonical hex mismatch")
    require(canonical_json(json.loads(descriptor_bytes)) == descriptor_bytes, "descriptor bytes are not canonical")
    require(record_id(descriptor_bytes)["id"] == descriptor_vector["record_id"], "descriptor record preimage/id mismatch")
    descriptor_obj = descriptor_vector["descriptor"]
    require(canonical_json(descriptor_obj) == descriptor_bytes, "descriptor canonical JSON differs from structured descriptor")
    for i, file_value in enumerate(descriptor_obj["files"]):
        validate_state_output(file_value, f"descriptor.files[{i}]")
    validate_state_output({k: descriptor_vector["enrolled_file"][k] for k in ("path_hex", "mode", "content_sha256", "file_id")}, "descriptor.enrolled_file")
    validate_preimage_file_id(descriptor_vector["enrolled_file"]["preimage_hex"], descriptor_vector["enrolled_file"]["file_id"], "descriptor enrolled preimage")
    for vector_name in ("generic_minimal_new_file", "bound_profile_new_file"):
        vector = output[vector_name]
        validate_snapshot_output(vector["result_snapshot"], vector_name + ".result_snapshot")
        validate_file_id(vector["file_id"], vector_name + ".file_id")
        validate_preimage_file_id(vector["preimage_hex"], vector["file_id"], vector_name + ".preimage")
    generic = output["generic_minimal_new_file"]
    validate_sha(generic["base_snapshot_id"], "generic base snapshot id")
    change_bytes = generic["source_change_canonical_json"].encode("utf-8")
    require(change_bytes.hex() == generic["source_change_canonical_hex"], "generic source change canonical hex mismatch")
    require(canonical_json(json.loads(change_bytes)) == change_bytes, "generic change not canonical")
    validate_state_output(generic["source_change"]["after"], "generic source_change.after")
    bound = output["bound_profile_new_file"]
    validate_snapshot_output(bound["base_snapshot"], "bound base snapshot")
    validate_file_id(bound["base_enrolled_file"]["file_id"], "bound base enrolled FileId")
    validate_preimage_file_id(bound["base_enrolled_file"]["preimage_hex"], bound["base_enrolled_file"]["file_id"], "bound base enrolled preimage")
    for i, file_value in enumerate(bound["result_files"]):
        validate_state_output(file_value, f"bound.result_files[{i}]")
    require([item["path_hex"] for item in bound["result_files"]] == sorted(item["path_hex"] for item in bound["result_files"]), "bound result files not path sorted")
    bound_change_bytes = bound["source_change_canonical_json"].encode("utf-8")
    require(bound_change_bytes.hex() == bound["source_change_canonical_hex"], "bound source change canonical hex mismatch")
    require(canonical_json(json.loads(bound_change_bytes)) == bound_change_bytes, "bound source change not canonical")
    validate_state_output(bound["source_change"]["after"], "bound source_change.after")
    mixed = output["mixed_copy_split_plain_new"]
    validate_transition_output(mixed["transition"])
    require(mixed["transition"]["result_files"] == mixed["result_files_sorted_by_raw_path"], "mixed result file summaries disagree")
    require(mixed["transition"]["changes"] == mixed["changes_sorted_by_after_or_before_raw_path"], "mixed change summaries disagree")
    validate_snapshot_output(mixed["base_snapshot"], "mixed base snapshot")
    validate_snapshot_output(mixed["result_snapshot"], "mixed result snapshot")
    for i, item in enumerate(mixed["base_enrolled_file_ids"]):
        validate_path_hex(item["path_hex"], f"mixed.base_enrolled_file_ids[{i}].path_hex")
        validate_file_id(item["file_id"], f"mixed.base_enrolled_file_ids[{i}].file_id")
    transition_bytes = mixed["transition_canonical_json"].encode("utf-8")
    require(transition_bytes.hex() == mixed["transition_canonical_hex"], "mixed transition canonical hex mismatch")
    require(canonical_json(json.loads(transition_bytes)) == transition_bytes, "mixed transition not canonical")
    for path_hex, detail in mixed["proposed_identity_preimages"].items():
        validate_path_hex(path_hex, "proposed_identity_preimages key")
        validate_file_id(detail["file_id"], "proposed_identity_preimages.file_id")
        validate_preimage_file_id(detail["preimage_hex"], detail["file_id"], "proposed identity preimage")


if __name__ == "__main__":
    main()
