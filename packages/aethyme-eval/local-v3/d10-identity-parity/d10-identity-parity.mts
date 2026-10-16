import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import {
  canonicalizeValue,
  type CanonicalValue,
} from "../t01-canonical-parity/canonical-json.mts";
import {
  advanceFileIdentities,
  contributionRevisionRecord,
  decodeLowerHex,
  enrolledFileId,
  getHashCallCount,
  immutableRecordId,
  isCanonicalFileId,
  parseContributionId,
  parseFileId,
  parseProjectId,
  resetHashCallCount,
  sourceSnapshot,
  type FileOperation,
  type FileVector,
} from "./project-identity.mts";

type Pin = { path: string; sha256: string };
type PinManifest = {
  schema: string;
  fixture_source_commit: string;
  qualification_source_commit: string;
  rust_sources: Pin[];
  typescript_sources: Pin[];
  fixture_files: Pin[];
};
type DescriptorInput = {
  project_id: string;
  contribution_id: string;
  anchor_commit: string;
  descriptor_fixture_source_file: FileVector;
  descriptor_fixture_expected: {
    anchor_snapshot_id: string;
    enrolled_file_id: string;
    descriptor_record_id: string;
  };
  generic_new_file: FileVector & { proposal_seed: string };
  bound_profile_existing_file: FileVector;
  mixed_base_files: FileVector[];
  mixed_result_files: FileVector[];
  copy_operation: { from_path_hex: string; to_path_hex: string };
  split_operation: {
    from_path_hex: string;
    to_path_hexes_input_order: string[];
  };
  plain_new_path_hex: string;
};
type V2Inputs = {
  fixture_schema: string;
  project_id: string;
  contribution_id: string;
  capture_record_id: string;
  brief: { record_id: string; object_sha256: string };
};
type RevisionGolden = {
  canonical_hex: string;
  canonical_json: string;
  id: string;
  record_preimage_hex: string;
};
type V2Expected = {
  source_snapshots: { baseline: { id: string } };
  contribution_revisions: {
    baseline_immutable: RevisionGolden;
    rationale_only_immutable: RevisionGolden;
    rationale_only_changed_members: string[];
    bound_profile_new_file: { canonical_json: string };
  };
};

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "../../../../");
const EXPECTED_ORDINAL_MANIFEST_SHA256 =
  "69ac3502ed87470275922c27a5098c06d0272049b53f4141c354346f66ca978f";
const REQUIRED_FIXTURE_PATHS = [
  "vectors/ordinal-supplement/manifest.json",
  "vectors/ordinal-supplement/inputs.json",
  "vectors/ordinal-supplement/expected.json",
  "vectors/ordinal-supplement/oracle.py",
  "vectors/prior-v1-invalid/manifest.json",
  "vectors/prior-v1-invalid/inputs.json",
  "vectors/prior-v1-invalid/expected.json",
  "vectors/prior-v1-invalid/oracle.py",
  "vectors/prior-v2-partially-invalid/manifest.json",
  "vectors/prior-v2-partially-invalid/inputs.json",
  "vectors/prior-v2-partially-invalid/expected.json",
  "vectors/prior-v2-partially-invalid/oracle.py",
] as const;
let assertionCount = 0;

function check(condition: boolean, message: string): asserts condition {
  assertionCount += 1;
  if (!condition) throw new Error(message);
}

function objectAt(value: unknown, label: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${label} is not an object`);
  }
  return value as Record<string, unknown>;
}

function stringAt(value: unknown, label: string): string {
  if (typeof value !== "string") throw new Error(`${label} is not a string`);
  return value;
}

function arrayAt(value: unknown, label: string): unknown[] {
  if (!Array.isArray(value)) throw new Error(`${label} is not an array`);
  return value;
}

function toCanonical(value: unknown): CanonicalValue {
  if (typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value) || Object.is(value, -0)) {
      throw new Error("fixture contains a non-canonical number");
    }
    return value;
  }
  if (Array.isArray(value)) return value.map(toCanonical);
  if (value instanceof Map) {
    return new Map(
      [...value.entries()].map(([key, member]) => [key, toCanonical(member)]),
    );
  }
  if (typeof value === "object" && value !== null) {
    return new Map(
      Object.entries(value as Record<string, unknown>).map(([key, member]) => [
        key,
        toCanonical(member),
      ]),
    );
  }
  throw new Error("fixture contains a value outside the frozen canonical profile");
}

function canonicalBytes(value: unknown): Buffer {
  return canonicalizeValue(toCanonical(value));
}

function checkCanonicalValue(actual: unknown, expected: unknown, label: string): void {
  check(
    canonicalBytes(actual).equals(canonicalBytes(expected)),
    `${label}: canonical value differs from frozen expected data`,
  );
}

function checkCanonicalText(
  actual: CanonicalValue,
  expectedJson: string,
  expectedHex: string,
  label: string,
): Buffer {
  const actualBytes = canonicalizeValue(actual);
  check(
    actualBytes.toString("utf8") === expectedJson,
    `${label}: canonical JSON bytes differ from frozen expected text`,
  );
  check(
    actualBytes.toString("hex") === expectedHex,
    `${label}: canonical byte hex differs from frozen expected hex`,
  );
  return actualBytes;
}

function expectProfileError(
  expectedCode: string,
  operation: () => unknown,
  label: string,
): void {
  try {
    operation();
  } catch (error) {
    const code =
      typeof error === "object" && error !== null && "code" in error
        ? (error as { code?: unknown }).code
        : undefined;
    check(code === expectedCode, `${label}: expected ${expectedCode}, got ${String(code)}`);
    return;
  }
  check(false, `${label}: expected ${expectedCode}, operation succeeded`);
}

function directSha256(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

function readJson<T>(relativePath: string): T {
  return JSON.parse(readFileSync(resolve(HERE, relativePath), "utf8")) as T;
}

function validateRelativePath(path: string): void {
  if (path.length === 0 || path.startsWith("/") || path.split("/").includes("..")) {
    throw new Error(`pin path is not a safe relative path: ${path}`);
  }
}

function verifyPins(pins: PinManifest): Array<{ path: string; sha256: string }> {
  check(
    pins.schema === "aethyme.local-d10-typescript-parity-pins/v1",
    "unsupported D10 pin manifest schema",
  );
  check(
    pins.fixture_source_commit === "fcd5ed940b4d906452e4ad1d3aa90eb767f1af45",
    "independent fixture source commit changed",
  );
  check(
    pins.fixture_files.length === REQUIRED_FIXTURE_PATHS.length,
    "pin manifest does not cover the corrected and prior independent packages",
  );
  const fixturePaths = pins.fixture_files.map(({ path }) => path).sort();
  check(
    fixturePaths.join("\n") === [...REQUIRED_FIXTURE_PATHS].sort().join("\n"),
    "pin manifest fixture file set changed",
  );
  const seen = new Set<string>();
  const hashes: Array<{ path: string; sha256: string }> = [];
  for (const [kind, entries] of [
    ["rust", pins.rust_sources],
    ["typescript", pins.typescript_sources],
  ] as const) {
    for (const pin of entries) {
      validateRelativePath(pin.path);
      if (seen.has(pin.path)) throw new Error(`duplicate pinned source ${pin.path}`);
      seen.add(pin.path);
      const content = readFileSync(resolve(REPO_ROOT, pin.path));
      const actual = directSha256(content);
      check(
        actual === pin.sha256,
        `${kind} source pin drift at ${pin.path}: expected ${pin.sha256}, found ${actual}`,
      );
      hashes.push({ path: pin.path, sha256: actual });
    }
  }
  for (const pin of pins.fixture_files) {
    validateRelativePath(pin.path);
    if (seen.has(pin.path)) throw new Error(`duplicate pinned fixture ${pin.path}`);
    seen.add(pin.path);
    const content = readFileSync(resolve(HERE, pin.path));
    const actual = directSha256(content);
    check(
      actual === pin.sha256,
      `frozen fixture pin drift at ${pin.path}: expected ${pin.sha256}, found ${actual}`,
    );
    hashes.push({ path: pin.path, sha256: actual });
  }
  return hashes;
}

function checkExpectedSnapshot(actual: unknown, expected: unknown, label: string): void {
  checkCanonicalValue(actual, expected, label);
}

function fileStateFiles(value: unknown, label: string): unknown[] {
  const object = objectAt(value, label);
  return arrayAt(object.files_sorted_by_raw_path, `${label}.files_sorted_by_raw_path`);
}

function run(): void {
  const pins = readJson<PinManifest>("source-pins.json");
  const ordinalManifest = readJson<Record<string, unknown>>(
    "vectors/ordinal-supplement/manifest.json",
  );
  const ordinalInputs = readJson<DescriptorInput>(
    "vectors/ordinal-supplement/inputs.json",
  );
  const ordinalExpected = readJson<Record<string, unknown>>(
    "vectors/ordinal-supplement/expected.json",
  );
  const priorV1Input = readJson<Record<string, unknown>>(
    "vectors/prior-v1-invalid/inputs.json",
  );
  const priorV2Input = readJson<V2Inputs>(
    "vectors/prior-v2-partially-invalid/inputs.json",
  );
  const priorV2Expected = readJson<V2Expected>(
    "vectors/prior-v2-partially-invalid/expected.json",
  );

  parseProjectId(ordinalInputs.project_id);
  parseContributionId(ordinalInputs.contribution_id);
  check(
    ordinalInputs.project_id === "proj:aaaaaaaaaaaaaaaaaaaaaaaaaa",
    "ordinal supplement changed its pinned ProjectId",
  );
  check(
    ordinalInputs.contribution_id === "contrib:bbbbbbbbbbbbbbbbbbbbbbbbbe",
    "ordinal supplement changed its pinned ContributionId",
  );
  resetHashCallCount();
  const oldContributionId = stringAt(priorV1Input.contribution_id, "v1 contribution_id");
  check(
    oldContributionId.slice("contrib:".length).length === 25,
    "preserved v1 attempt no longer identifies its 25-character suffix failure",
  );
  expectProfileError(
    "invalid_contribution_id",
    () => parseContributionId(oldContributionId),
    "prior v1 ContributionId refusal",
  );
  check(
    getHashCallCount() === 0,
    "invalid v1 ContributionId was rejected before identity hashing",
  );
  const priorBoundRecord = JSON.parse(
    priorV2Expected.contribution_revisions.bound_profile_new_file.canonical_json,
  ) as Record<string, unknown>;
  const priorBoundChanges = arrayAt(priorBoundRecord.source_changes, "v2 source_changes");
  const priorBoundChange = objectAt(priorBoundChanges[0], "v2 first source change");
  const priorBoundAfter = objectAt(priorBoundChange.after, "v2 first source-change after");
  const oldFileId = stringAt(priorBoundAfter.file_id, "v2 FileId");
  check(
    oldFileId.startsWith("file:") && !oldFileId.startsWith("file:sha256:"),
    "preserved v2 attempt no longer identifies its missing sha256: component",
  );
  check(!isCanonicalFileId(oldFileId), "unqualified v2 FileId unexpectedly became canonical");
  expectProfileError(
    "invalid_file_id",
    () => parseFileId(oldFileId),
    "prior v2 FileId refusal",
  );
  check(
    getHashCallCount() === 0,
    "unqualified v2 FileId was rejected before identity hashing",
  );

  const verifiedHashes = verifyPins(pins);
  const ordinalManifestSha = directSha256(
    readFileSync(resolve(HERE, "vectors/ordinal-supplement/manifest.json")),
  );
  check(
    ordinalManifestSha === EXPECTED_ORDINAL_MANIFEST_SHA256,
    "corrected ordinal supplement manifest pin changed",
  );
  check(
    ordinalManifest.package === "independent Local D10 ordinal and FileId-format supplement v1",
    "unexpected ordinal supplement identity",
  );
  check(
    ordinalManifest.status === "frozen expected-data supplement; earlier v1 and v2 directories remain byte-exact",
    "ordinal supplement does not preserve the prior failed attempts",
  );
  check(
    pins.qualification_source_commit.length === 40 &&
      /^[0-9a-f]{40}$/.test(pins.qualification_source_commit),
    "qualification source commit pin is not a full lowercase SHA",
  );

  const descriptorVector = objectAt(
    ordinalExpected.descriptor_fixture_vector,
    "descriptor_fixture_vector",
  );
  const descriptorSource = ordinalInputs.descriptor_fixture_source_file;
  const descriptorSnapshot = sourceSnapshot([descriptorSource]);
  const descriptorPath = decodeLowerHex(descriptorSource.path_hex);
  const descriptorFileId = enrolledFileId(
    ordinalInputs.project_id,
    descriptorSnapshot.id,
    descriptorPath,
  );
  const descriptorEntry = descriptorSnapshot.files_sorted_by_raw_path[0];
  if (descriptorEntry === undefined) throw new Error("descriptor snapshot has no file");
  const descriptorFile = new Map<string, CanonicalValue>([
    ["path_hex", descriptorEntry.path_hex],
    ["mode", descriptorEntry.mode],
    ["content_sha256", descriptorEntry.content_sha256],
    ["file_id", descriptorFileId.file_id],
  ]);
  const descriptor = new Map<string, CanonicalValue>([
    ["schema", "aethyme.project-descriptor/experimental-v0"],
    ["project_id", ordinalInputs.project_id],
    ["anchor_commit", ordinalInputs.anchor_commit],
    ["anchor_snapshot", descriptorSnapshot.id],
    ["source_binding", `git-commit:${ordinalInputs.anchor_commit}`],
    ["policy_location", ".aethyme/config.toml"],
    ["acceptance_target", "unbound"],
    ["visibility_boundary", "local"],
    ["files", [descriptorFile]],
  ]);
  const descriptorBytes = canonicalizeValue(descriptor);
  checkExpectedSnapshot(
    descriptorSnapshot,
    descriptorVector.anchor_snapshot,
    "descriptor anchor snapshot",
  );
  checkExpectedSnapshot(
    descriptorFileId,
    descriptorVector.enrolled_file,
    "descriptor enrolled FileId preimage",
  );
  check(
    descriptorFileId.file_id === ordinalInputs.descriptor_fixture_expected.enrolled_file_id,
    "descriptor enrolled FileId differs from committed Rust fixture expectation",
  );
  check(
    descriptorSnapshot.id === ordinalInputs.descriptor_fixture_expected.anchor_snapshot_id,
    "descriptor snapshot differs from committed Rust fixture expectation",
  );
  checkCanonicalValue(descriptor, descriptorVector.descriptor, "ProjectDescriptor fields");
  checkCanonicalText(
    descriptor,
    stringAt(descriptorVector.canonical_json, "descriptor canonical_json"),
    stringAt(descriptorVector.canonical_hex, "descriptor canonical_hex"),
    "ProjectDescriptor record",
  );
  const descriptorRecordId = immutableRecordId(descriptorBytes);
  check(
    descriptorRecordId === descriptorVector.record_id &&
      descriptorRecordId === ordinalInputs.descriptor_fixture_expected.descriptor_record_id,
    "ProjectDescriptor immutable RecordId mismatch",
  );

  const priorBaseline = priorV2Expected.contribution_revisions.baseline_immutable;
  const baseSnapshotId = priorV2Expected.source_snapshots.baseline.id;
  const baselineRevision = contributionRevisionRecord({
    project_id: priorV2Input.project_id,
    contribution_id: priorV2Input.contribution_id,
    capture_record_id: priorV2Input.capture_record_id,
    base: baseSnapshotId,
    result: baseSnapshotId,
    source_changes: [],
    depends_on: [],
  });
  checkCanonicalText(
    baselineRevision.value,
    priorBaseline.canonical_json,
    priorBaseline.canonical_hex,
    "baseline immutable ContributionRevision",
  );
  check(
    baselineRevision.id === priorBaseline.id,
    "baseline immutable ContributionRevision RecordId mismatch",
  );
  const baselinePreimage = Buffer.concat([
    Buffer.from("aethyme record v0\0", "utf8"),
    baselineRevision.canonical_bytes,
  ]);
  check(
    baselinePreimage.toString("hex") === priorBaseline.record_preimage_hex,
    "baseline immutable ContributionRevision preimage mismatch",
  );

  const priorRationale = priorV2Expected.contribution_revisions.rationale_only_immutable;
  const rationaleRevision = contributionRevisionRecord({
    project_id: priorV2Input.project_id,
    contribution_id: priorV2Input.contribution_id,
    capture_record_id: priorV2Input.capture_record_id,
    base: baseSnapshotId,
    result: baseSnapshotId,
    source_changes: [],
    depends_on: [],
    brief_record_id: priorV2Input.brief.record_id,
    brief_object_sha256: priorV2Input.brief.object_sha256,
  });
  checkCanonicalText(
    rationaleRevision.value,
    priorRationale.canonical_json,
    priorRationale.canonical_hex,
    "rationale-only immutable ContributionRevision",
  );
  check(
    rationaleRevision.id === priorRationale.id,
    "rationale-only immutable ContributionRevision RecordId mismatch",
  );
  const rationalePreimage = Buffer.concat([
    Buffer.from("aethyme record v0\0", "utf8"),
    rationaleRevision.canonical_bytes,
  ]);
  check(
    rationalePreimage.toString("hex") === priorRationale.record_preimage_hex,
    "rationale-only immutable ContributionRevision preimage mismatch",
  );
  check(
    baselineRevision.id !== rationaleRevision.id &&
      !baselineRevision.canonical_bytes.equals(rationaleRevision.canonical_bytes),
    "adding the rationale reference did not produce an immutable revision change",
  );
  check(
    priorV2Expected.contribution_revisions.rationale_only_changed_members.join(",") ===
      "brief_object_sha256,brief_record_id",
    "rationale-only golden changed fields unexpectedly",
  );

  const boundExpected = objectAt(
    ordinalExpected.bound_profile_new_file,
    "bound_profile_new_file",
  );
  const boundResultFiles = [
    ordinalInputs.bound_profile_existing_file,
    ordinalInputs.generic_new_file,
  ];
  const bound = advanceFileIdentities(
    ordinalInputs.project_id,
    ordinalInputs.contribution_id,
    [ordinalInputs.bound_profile_existing_file],
    boundResultFiles,
    [],
  );
  checkExpectedSnapshot(bound.base_snapshot, boundExpected.base_snapshot, "bound base snapshot");
  checkExpectedSnapshot(
    bound.result_snapshot,
    boundExpected.result_snapshot,
    "bound result snapshot",
  );
  checkExpectedSnapshot(
    bound.result_files,
    boundExpected.result_files,
    "bound result FileIds",
  );
  const boundFileId = stringAt(boundExpected.file_id, "bound new FileId");
  const boundState = bound.result_files.find(
    (file) => file.path_hex === ordinalInputs.generic_new_file.path_hex,
  );
  if (boundState === undefined) throw new Error("bound new file is absent from result");
  check(
    boundState.file_id === boundFileId && isCanonicalFileId(boundState.file_id),
    "ContributionId-bound new FileId mismatch or non-canonical prefix",
  );
  const boundIdentity = advanceFileIdentities(
    ordinalInputs.project_id,
    ordinalInputs.contribution_id,
    [ordinalInputs.bound_profile_existing_file],
    boundResultFiles,
    [],
  ).proposed_preimages.get(ordinalInputs.generic_new_file.path_hex);
  if (boundIdentity === undefined) throw new Error("bound new FileId preimage is absent");
  check(
    boundIdentity.preimage_hex === boundExpected.preimage_hex &&
      boundIdentity.ordinal === boundExpected.ordinal,
    "ContributionId-bound FileId preimage or ordinal mismatch",
  );
  check(
    boundExpected.proposal_seed === ordinalInputs.contribution_id &&
      boundExpected.proposal_seed_source === "ContributionId.as_str()",
    "bound FileId did not use the frozen opaque ContributionId seed",
  );
  check(
    boundExpected.new_paths_sorted_raw_hex instanceof Array &&
      (boundExpected.new_paths_sorted_raw_hex as string[]).join(",") ===
        ordinalInputs.generic_new_file.path_hex,
    "bound new path raw-byte order mismatch",
  );
  const boundChange = bound.source_changes[0];
  if (boundChange === undefined) throw new Error("bound source change is absent");
  const boundChangeJson = stringAt(
    boundExpected.source_change_canonical_json,
    "bound source change JSON",
  );
  const boundChangeHex = stringAt(
    boundExpected.source_change_canonical_hex,
    "bound source change hex",
  );
  checkCanonicalText(boundChange, boundChangeJson, boundChangeHex, "bound source change");
  checkCanonicalValue(boundChange, boundExpected.source_change, "bound source change fields");

  const mixedExpected = objectAt(
    ordinalExpected.mixed_copy_split_plain_new,
    "mixed_copy_split_plain_new",
  );
  const copyOperation: FileOperation = {
    kind: "copy",
    from_path_hex: ordinalInputs.copy_operation.from_path_hex,
    to_path_hex: ordinalInputs.copy_operation.to_path_hex,
  };
  const splitOperation: FileOperation = {
    kind: "split",
    from_path_hex: ordinalInputs.split_operation.from_path_hex,
    to_path_hexes_input_order: ordinalInputs.split_operation.to_path_hexes_input_order,
  };
  const mixed = advanceFileIdentities(
    ordinalInputs.project_id,
    ordinalInputs.contribution_id,
    ordinalInputs.mixed_base_files,
    ordinalInputs.mixed_result_files,
    [copyOperation, splitOperation],
  );
  checkExpectedSnapshot(mixed.base_snapshot, mixedExpected.base_snapshot, "mixed base snapshot");
  checkExpectedSnapshot(
    mixed.result_snapshot,
    mixedExpected.result_snapshot,
    "mixed result snapshot",
  );
  checkExpectedSnapshot(
    mixed.result_files,
    mixedExpected.result_files_sorted_by_raw_path,
    "mixed result FileIds sorted by raw path",
  );
  checkExpectedSnapshot(
    mixed.source_changes,
    mixedExpected.changes_sorted_by_after_or_before_raw_path,
    "mixed source changes",
  );
  const mixedJson = stringAt(mixedExpected.transition_canonical_json, "mixed transition JSON");
  const mixedHex = stringAt(mixedExpected.transition_canonical_hex, "mixed transition hex");
  check(
    mixed.canonical_bytes.toString("utf8") === mixedJson,
    "mixed transition canonical JSON differs from frozen expected bytes",
  );
  check(
    mixed.canonical_bytes.toString("hex") === mixedHex,
    "mixed transition canonical byte hex differs from frozen expected bytes",
  );
  checkCanonicalValue(mixed.transition, mixedExpected.transition, "mixed transition fields");
  const mixedOrdinalFacts = objectAt(mixedExpected.ordinal_facts, "mixed ordinal facts");
  const globalNewPaths = mixed.result_files
    .filter((file) => !mixed.base_files.some((base) => base.path_hex === file.path_hex))
    .map((file) => file.path_hex);
  check(
    globalNewPaths.join(",") ===
      arrayAt(
        mixedOrdinalFacts.global_new_paths_sorted_raw_hex,
        "global new path order",
      ).join(","),
    "mixed global new-path ordinal order mismatch",
  );
  const mixedPreimages = objectAt(
    mixedExpected.proposed_identity_preimages,
    "mixed proposed preimages",
  );
  check(
    mixed.proposed_preimages.size === Object.keys(mixedPreimages).length,
    "mixed proposed FileId preimage count mismatch",
  );
  for (const [pathHex, identity] of mixed.proposed_preimages) {
    const expectedIdentity = objectAt(mixedPreimages[pathHex], `preimage ${pathHex}`);
    checkCanonicalValue(identity, expectedIdentity, `proposed FileId preimage ${pathHex}`);
  }
  const permutedSplit: FileOperation = {
    kind: "split",
    from_path_hex: ordinalInputs.split_operation.from_path_hex,
    to_path_hexes_input_order: [...ordinalInputs.split_operation.to_path_hexes_input_order].reverse(),
  };
  const mixedPermuted = advanceFileIdentities(
    ordinalInputs.project_id,
    ordinalInputs.contribution_id,
    ordinalInputs.mixed_base_files,
    ordinalInputs.mixed_result_files,
    [permutedSplit, copyOperation],
  );
  check(
    mixedPermuted.canonical_bytes.equals(mixed.canonical_bytes),
    "reordering Copy/Split operations or Split destinations changed canonical transition bytes",
  );
  check(
    mixedPermuted.proposed_preimages.size === mixed.proposed_preimages.size,
    "operation permutation changed proposed FileId count",
  );
  for (const [pathHex, identity] of mixed.proposed_preimages) {
    const permutedIdentity = mixedPermuted.proposed_preimages.get(pathHex);
    check(
      permutedIdentity !== undefined &&
        permutedIdentity.file_id === identity.file_id &&
        permutedIdentity.ordinal === identity.ordinal &&
        permutedIdentity.preimage_hex === identity.preimage_hex,
      `operation permutation changed FileId at ${pathHex}`,
    );
  }

  check(fileStateFiles(mixed.result_snapshot, "mixed result snapshot").length === 5,
    "mixed result snapshot did not retain all five raw-path entries");
  const sourcePinsBytes = readFileSync(resolve(HERE, "source-pins.json"));
  const sourcePinsSha = directSha256(sourcePinsBytes);
  const report = {
    schema: "aethyme.local-d10-typescript-parity-report/v1",
    qualification_source_commit: pins.qualification_source_commit,
    fixture_source_commit: pins.fixture_source_commit,
    ordinal_manifest_sha256: ordinalManifestSha,
    source_pins_sha256: sourcePinsSha,
    verified_files: verifiedHashes,
    assertions: assertionCount,
    vectors: {
      descriptor: 1,
      contribution_revision_records: 2,
      bound_profile_new_file: 1,
      mixed_copy_split_plain_new: 1,
      retained_invalid_attempts: { v1_contribution_id: "rejected", v2_file_id: "rejected" },
    },
    runtime_boundary: "Node built-ins only; no network, Cargo, native product, or generated vector edits",
  };
  console.log(JSON.stringify(report, null, 2));
}

run();
