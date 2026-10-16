import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
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
  validateCommit,
  validateProposalId,
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
type BoundRevisionInput = {
  fixture_schema: string;
  project_id: string;
  contribution_id: string;
  proposal_seed: string;
  capture_record_id: string;
  anchor_commit: string;
  base_files: FileVector[];
  new_file: FileVector;
  validation_controls: { invalid_contribution_id: string };
};
type RenameInput = {
  fixture_schema: string;
  project_id: string;
  contribution_id: string;
  proposal_id: string;
  capture_record_id: string;
  base_files: FileVector[];
  result_files: FileVector[];
  operation: { kind: "rename"; from_path_hex: string; to_path_hex: string };
  validation_controls: { invalid_contribution_id: string };
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
const EXPECTED_BOUND_REVISION_MANIFEST_SHA256 =
  "ab85554b8b16543b2b356acaa29fcfbfcea8456292baa705d167daab5a14c9d1";
const EXPECTED_RENAME_MANIFEST_SHA256 =
  "6fb13ac0e523d65efcaece433353e3fe787aaf09eb90cebab35804bafcc287e0";
const EXPECTED_QUALIFICATION_SOURCE_COMMIT =
  "324a493208517ba12f537c9e6a4ec7f608af322f";
const EXPECTED_FIXTURE_SOURCE_COMMIT =
  "fcd5ed940b4d906452e4ad1d3aa90eb767f1af45";
const REQUIRED_RUST_SOURCES = [
  "packages/aethyme/rust/crates/aethyme-contracts/src/experimental_v0/canonical_json.rs",
  "packages/aethyme/rust/crates/aethyme-contracts/src/experimental_v0/record.rs",
  "packages/aethyme/rust/crates/aethyme-contracts/src/experimental_v0/source_snapshot.rs",
  "packages/aethyme/rust/crates/aethyme-contracts/src/experimental_v0/digest.rs",
  "packages/aethyme/rust/crates/aethyme-contracts/src/experimental_v0/project_identity.rs",
  "packages/aethyme/rust/crates/aethyme-contracts/src/experimental_v0/contribution_revision.rs",
  "packages/aethyme/rust/crates/aethyme-contracts/tests/canonical_json_vectors.rs",
  "packages/aethyme/rust/crates/aethyme-contracts/tests/record_vectors.rs",
  "packages/aethyme/rust/crates/aethyme-contracts/tests/source_snapshot_vectors.rs",
  "packages/aethyme/rust/crates/aethyme-contracts/tests/project_identity_vectors.rs",
  "packages/aethyme/rust/crates/aethyme-contracts/tests/contribution_revision.rs",
  "packages/aethyme/rust/crates/aethyme-contracts/fixtures/experimental-v0/project_identity.json",
] as const;
const REQUIRED_TYPESCRIPT_SOURCES = [
  "packages/aethyme-eval/local-v3/t01-canonical-parity/canonical-json.mts",
  "packages/aethyme-eval/local-v3/d10-identity-parity/project-identity.mts",
  "packages/aethyme-eval/local-v3/d10-identity-parity/d10-identity-parity.mts",
] as const;
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
  "vectors/contribution-revision-bound/manifest.json",
  "vectors/contribution-revision-bound/inputs.json",
  "vectors/contribution-revision-bound/expected.json",
  "vectors/contribution-revision-bound/oracle.py",
  "vectors/explicit-rename/manifest.json",
  "vectors/explicit-rename/inputs.json",
  "vectors/explicit-rename/expected.json",
  "vectors/explicit-rename/oracle.py",
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
    return new Map<string, CanonicalValue>(
      [...value.entries()].map(
        ([key, member]): [string, CanonicalValue] => [key, toCanonical(member)],
      ),
    );
  }
  if (typeof value === "object" && value !== null) {
    return new Map<string, CanonicalValue>(
      Object.entries(value as Record<string, unknown>).map(
        ([key, member]): [string, CanonicalValue] => [key, toCanonical(member)],
      ),
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
    pins.schema === "aethyme.local-d10-typescript-parity-pins/v2",
    "unsupported D10 pin manifest schema",
  );
  check(
    pins.fixture_source_commit === EXPECTED_FIXTURE_SOURCE_COMMIT,
    "independent fixture source commit changed",
  );
  check(
    pins.qualification_source_commit === EXPECTED_QUALIFICATION_SOURCE_COMMIT,
    "qualification source commit is not bound to the reviewed Rust contract source",
  );
  for (const [kind, entries, required] of [
    ["Rust", pins.rust_sources, REQUIRED_RUST_SOURCES],
    ["TypeScript", pins.typescript_sources, REQUIRED_TYPESCRIPT_SOURCES],
    ["fixture", pins.fixture_files, REQUIRED_FIXTURE_PATHS],
  ] as const) {
    if (!Array.isArray(entries)) throw new Error(`${kind} pins are not an array`);
    check(
      entries.length === required.length,
      `${kind} pin count differs from the reviewed required count`,
    );
    const paths = entries.map(({ path }) => path).sort();
    check(
      paths.join("\n") === [...required].sort().join("\n"),
      `${kind} pin path set differs from the reviewed required set`,
    );
    for (const pin of entries) {
      if (!/^[0-9a-f]{64}$/.test(pin.sha256)) {
        throw new Error(`${kind} pin is not a lowercase SHA-256: ${pin.path}`);
      }
    }
  }
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
  const boundRevisionManifest = readJson<Record<string, unknown>>(
    "vectors/contribution-revision-bound/manifest.json",
  );
  const boundRevisionInputs = readJson<BoundRevisionInput>(
    "vectors/contribution-revision-bound/inputs.json",
  );
  const boundRevisionExpected = readJson<Record<string, unknown>>(
    "vectors/contribution-revision-bound/expected.json",
  );
  const renameManifest = readJson<Record<string, unknown>>(
    "vectors/explicit-rename/manifest.json",
  );
  const renameInputs = readJson<RenameInput>("vectors/explicit-rename/inputs.json");
  const renameExpected = readJson<Record<string, unknown>>(
    "vectors/explicit-rename/expected.json",
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
  check(
    validateCommit(ordinalInputs.anchor_commit) === ordinalInputs.anchor_commit,
    "descriptor anchor commit is not a canonical Rust commit ID",
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

  parseProjectId(renameInputs.project_id);
  parseContributionId(renameInputs.contribution_id);
  check(
    renameInputs.project_id === ordinalInputs.project_id &&
      renameInputs.contribution_id === ordinalInputs.contribution_id,
    "rename and ordinal fixtures disagree on opaque identity inputs",
  );
  resetHashCallCount();
  const renameInvalidContributionId = renameInputs.validation_controls.invalid_contribution_id;
  check(
    renameInvalidContributionId.slice("contrib:".length).length === 25,
    "rename fixture validation control no longer has the recorded 25-character suffix",
  );
  expectProfileError(
    "invalid_contribution_id",
    () => parseContributionId(renameInvalidContributionId),
    "rename fixture invalid ContributionId refusal",
  );
  check(
    validateProposalId(renameInputs.proposal_id) === "proposal-rename",
    "Rust's generic proposal token was not accepted",
  );
  expectProfileError(
    "invalid_contribution_id",
    () => parseContributionId(renameInputs.proposal_id),
    "generic proposal token is not an opaque ContributionId",
  );
  check(
    validateProposalId("x".repeat(128)) === "x".repeat(128),
    "Rust's maximum 128-byte proposal token was rejected",
  );
  for (const invalidProposalId of ["", "bad token", "proposal\n", "é", "x".repeat(129)]) {
    expectProfileError(
      "invalid_proposal_id",
      () => validateProposalId(invalidProposalId),
      `invalid generic proposal token ${JSON.stringify(invalidProposalId)}`,
    );
  }
  check(
    getHashCallCount() === 0,
    "invalid ContributionIds or proposal tokens were checked after identity hashing",
  );

  const invalidPathCases: Array<{ path: Buffer; label: string }> = [
    { path: Buffer.alloc(4097, 0x61), label: "path longer than Rust's 4096-byte cap" },
    { path: Buffer.from(".GIT/config", "ascii"), label: "case-insensitive .git path alias" },
    { path: Buffer.from(".g\u200cit/config", "utf8"), label: "HFS-ignorable .git path alias" },
    { path: Buffer.from(".git. /config", "ascii"), label: "NTFS trailing-dot/space .git alias" },
    { path: Buffer.from(".git::$INDEX_ALLOCATION/config", "ascii"), label: "NTFS alternate-stream .git alias" },
    { path: Buffer.from("git~1/config", "ascii"), label: "NTFS 8.3 .git path alias" },
  ];
  for (const { path, label } of invalidPathCases) {
    resetHashCallCount();
    expectProfileError(
      "invalid_path",
      () => sourceSnapshot([{ path_hex: path.toString("hex"), mode: "100644", content_hex: "" }]),
      label,
    );
    check(getHashCallCount() === 0, `${label}: rejected only after hashing`);
  }
  resetHashCallCount();
  const maximumLengthPath = Buffer.alloc(4096, 0x61);
  const maximumLengthSnapshot = sourceSnapshot([
    { path_hex: maximumLengthPath.toString("hex"), mode: "100644", content_hex: "" },
  ]);
  check(
    maximumLengthSnapshot.files_sorted_by_raw_path[0]?.path_hex ===
      maximumLengthPath.toString("hex") &&
      getHashCallCount() > 0,
    "the exact 4096-byte source path limit was not accepted as Rust accepts it",
  );
  resetHashCallCount();
  expectProfileError(
    "file_directory_conflict",
    () =>
      sourceSnapshot([
        { path_hex: "61", mode: "100644", content_hex: "" },
        { path_hex: "612f62", mode: "100644", content_hex: "" },
      ]),
    "snapshot rejects a file that is also another path's directory prefix",
  );
  check(
    getHashCallCount() === 0,
    "file/directory conflict was rejected only after hashing",
  );
  resetHashCallCount();
  const opaqueInvalidUtf8Path = Buffer.from([0x2e, 0x67, 0xff, 0x69, 0x74, 0x2f, 0x78]);
  const opaqueInvalidUtf8Snapshot = sourceSnapshot([
    { path_hex: opaqueInvalidUtf8Path.toString("hex"), mode: "100644", content_hex: "" },
  ]);
  check(
    opaqueInvalidUtf8Snapshot.files_sorted_by_raw_path[0]?.path_hex ===
      opaqueInvalidUtf8Path.toString("hex") &&
      getHashCallCount() > 0,
    "non-UTF-8 raw path bytes were normalized as a .git alias instead of preserved",
  );

  const verifiedHashes = verifyPins(pins);
  const ordinalManifestSha = directSha256(
    readFileSync(resolve(HERE, "vectors/ordinal-supplement/manifest.json")),
  );
  check(
    ordinalManifestSha === EXPECTED_ORDINAL_MANIFEST_SHA256,
    "corrected ordinal supplement manifest pin changed",
  );
  const boundRevisionManifestSha = directSha256(
    readFileSync(resolve(HERE, "vectors/contribution-revision-bound/manifest.json")),
  );
  const renameManifestSha = directSha256(
    readFileSync(resolve(HERE, "vectors/explicit-rename/manifest.json")),
  );
  check(
    boundRevisionManifestSha === EXPECTED_BOUND_REVISION_MANIFEST_SHA256,
    "corrected bound-file ContributionRevision manifest pin changed",
  );
  check(
    renameManifestSha === EXPECTED_RENAME_MANIFEST_SHA256,
    "explicit Rename manifest pin changed",
  );
  check(
    objectAt(boundRevisionManifest.source, "bound revision manifest source").commit ===
        EXPECTED_FIXTURE_SOURCE_COMMIT &&
      objectAt(renameManifest.source, "Rename manifest source").commit ===
        EXPECTED_FIXTURE_SOURCE_COMMIT,
    "corrected independent fixtures do not name the expected Rust source commit",
  );
  check(
    boundRevisionManifest.package ===
      "independent D10 corrected bound-file ContributionRevision golden v1" &&
      renameManifest.package ===
        "independent Local D10 explicit-rename transition and ContributionRevision golden v1",
    "corrected independent fixture package identity changed",
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

  const genericMinimalExpected = objectAt(
    ordinalExpected.generic_minimal_new_file,
    "generic_minimal_new_file",
  );
  const genericNewFile: FileVector = {
    path_hex: ordinalInputs.generic_new_file.path_hex,
    mode: ordinalInputs.generic_new_file.mode,
    content_hex: ordinalInputs.generic_new_file.content_hex,
  };
  const genericProposalSeed = validateProposalId(ordinalInputs.generic_new_file.proposal_seed);
  check(
    genericProposalSeed === "proposal-new-file" &&
      genericProposalSeed === genericMinimalExpected.proposal_seed &&
      genericMinimalExpected.raw_path_hex === genericNewFile.path_hex,
    "frozen generic new-file seed or raw path changed",
  );
  const genericMinimal = advanceFileIdentities(
    ordinalInputs.project_id,
    genericProposalSeed,
    [],
    [genericNewFile],
    [],
  );
  check(
    genericMinimal.base_snapshot.id === genericMinimalExpected.base_snapshot_id,
    "generic-seed minimal new-file base snapshot ID mismatch",
  );
  checkExpectedSnapshot(
    genericMinimal.result_snapshot,
    genericMinimalExpected.result_snapshot,
    "generic-seed minimal new-file result snapshot",
  );
  checkCanonicalValue(
    genericMinimal.result_files,
    [objectAt(genericMinimalExpected.source_change, "generic new-file source change").after],
    "generic-seed minimal new-file FileId",
  );
  const genericIdentity = genericMinimal.proposed_preimages.get(genericNewFile.path_hex);
  if (genericIdentity === undefined) throw new Error("generic new-file preimage is absent");
  check(
    genericIdentity.file_id === genericMinimalExpected.file_id &&
      genericIdentity.ordinal === genericMinimalExpected.ordinal &&
      genericIdentity.preimage_hex === genericMinimalExpected.preimage_hex,
    "generic-seed proposed FileId or identity preimage mismatch",
  );
  const genericChange = genericMinimal.source_changes[0];
  if (genericChange === undefined) throw new Error("generic new-file source change is absent");
  checkCanonicalText(
    genericChange,
    stringAt(genericMinimalExpected.source_change_canonical_json, "generic source-change JSON"),
    stringAt(genericMinimalExpected.source_change_canonical_hex, "generic source-change hex"),
    "generic-seed minimal new-file source change",
  );
  checkCanonicalValue(
    genericChange,
    genericMinimalExpected.source_change,
    "generic-seed minimal new-file source-change fields",
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

  check(
    boundRevisionInputs.project_id === ordinalInputs.project_id &&
      boundRevisionInputs.contribution_id === ordinalInputs.contribution_id &&
      boundRevisionInputs.proposal_seed === ordinalInputs.contribution_id &&
      boundRevisionInputs.capture_record_id === priorV2Input.capture_record_id,
    "corrected bound-file revision inputs changed the frozen identity lineage",
  );
  checkCanonicalValue(
    boundRevisionInputs.base_files,
    [ordinalInputs.bound_profile_existing_file],
    "corrected bound-file revision base inputs",
  );
  checkCanonicalValue(
    boundRevisionInputs.new_file,
    {
      path_hex: ordinalInputs.generic_new_file.path_hex,
      mode: ordinalInputs.generic_new_file.mode,
      content_hex: ordinalInputs.generic_new_file.content_hex,
    },
    "corrected bound-file revision new-file inputs",
  );
  const correctedBoundGolden = objectAt(
    boundRevisionExpected.contribution_revision,
    "corrected bound ContributionRevision golden",
  );
  const boundRevision = contributionRevisionRecord({
    project_id: boundRevisionInputs.project_id,
    contribution_id: boundRevisionInputs.contribution_id,
    capture_record_id: boundRevisionInputs.capture_record_id,
    base: bound.base_snapshot.id,
    result: bound.result_snapshot.id,
    source_changes: bound.source_changes,
    depends_on: [],
  });
  const boundRevisionCanonicalJson = stringAt(
    correctedBoundGolden.canonical_json,
    "corrected bound ContributionRevision canonical JSON",
  );
  const boundRevisionCanonicalHex = stringAt(
    correctedBoundGolden.canonical_hex,
    "corrected bound ContributionRevision canonical hex",
  );
  checkCanonicalText(
    boundRevision.value,
    boundRevisionCanonicalJson,
    boundRevisionCanonicalHex,
    "corrected bound-file immutable ContributionRevision",
  );
  checkCanonicalValue(
    boundRevision.value,
    correctedBoundGolden.value,
    "corrected bound-file ContributionRevision fields",
  );
  check(
    boundRevision.id === correctedBoundGolden.record_id &&
      boundRevision.id ===
        "sha256:e3803757a1c3c341a47e3fe171930bc911220f7989dd25f756605b7de28b0d85",
    "corrected bound-file ContributionRevision RecordId mismatch",
  );
  const boundRevisionPreimage = Buffer.concat([
    Buffer.from("aethyme record v0\0", "utf8"),
    boundRevision.canonical_bytes,
  ]);
  check(
    boundRevisionPreimage.toString("hex") === correctedBoundGolden.record_preimage_hex,
    "corrected bound-file ContributionRevision preimage mismatch",
  );

  if (!(boundChange instanceof Map)) throw new Error("bound source change is not a canonical object");
  const revisionArgs = {
    project_id: boundRevisionInputs.project_id,
    contribution_id: boundRevisionInputs.contribution_id,
    capture_record_id: boundRevisionInputs.capture_record_id,
    base: bound.base_snapshot.id,
    result: bound.result_snapshot.id,
    depends_on: [] as string[],
  };
  const expectInvalidRevision = (
    expectedCode: string,
    changes: CanonicalValue[],
    label: string,
  ): void => {
    resetHashCallCount();
    expectProfileError(
      expectedCode,
      () => contributionRevisionRecord({ ...revisionArgs, source_changes: changes }),
      label,
    );
    check(getHashCallCount() === 0, `${label}: rejected only after hashing`);
  };
  const newChangeWithBefore = new Map<string, CanonicalValue>(boundChange);
  const boundAfter = newChangeWithBefore.get("after");
  if (!(boundAfter instanceof Map)) throw new Error("bound source change after state is absent");
  newChangeWithBefore.set("before", boundAfter);
  expectInvalidRevision(
    "invalid_source_change",
    [newChangeWithBefore],
    "ContributionRevision rejects a New change with a before state",
  );
  const newChangeWithExtraField = new Map<string, CanonicalValue>(boundChange);
  newChangeWithExtraField.set("unexpected", "value");
  expectInvalidRevision(
    "invalid_source_change",
    [newChangeWithExtraField],
    "ContributionRevision rejects an unknown source-change field",
  );
  const badFileState = new Map<string, CanonicalValue>(boundAfter);
  badFileState.set("file_id", "file:invalid");
  const badFileIdChange = new Map<string, CanonicalValue>(boundChange);
  badFileIdChange.set("after", badFileState);
  expectInvalidRevision(
    "invalid_file_id",
    [badFileIdChange],
    "ContributionRevision rejects a non-canonical source-change FileId",
  );
  const newChange = (pathHex: string, fileId: string): CanonicalValue => {
    const state = new Map<string, CanonicalValue>([
      ["path_hex", pathHex],
      ["mode", "100644"],
      ["content_sha256", "3".repeat(64)],
      ["file_id", fileId],
    ]);
    return new Map<string, CanonicalValue>([
      ["relation", "new"],
      ["derived_from", []],
      ["possible_sources", []],
      ["after", state],
    ]);
  };
  expectInvalidRevision(
    "non_canonical_change_order",
    [
      newChange("62", `file:sha256:${"a".repeat(64)}`),
      newChange("61", `file:sha256:${"b".repeat(64)}`),
    ],
    "ContributionRevision enforces strictly increasing raw-path change order",
  );
  resetHashCallCount();

  const renameIdentity = objectAt(renameExpected.identity, "rename identity expected values");
  const renameBaseIdentityGoldens = objectAt(
    renameIdentity.base_enrolled_file_ids,
    "rename base enrolled FileIds",
  );
  const renameTransitionExpected = objectAt(renameExpected.transition, "rename transition golden");
  const renameSourceChangeExpected = objectAt(
    renameExpected.source_change,
    "rename source-change golden",
  );
  const renameRevisionExpected = objectAt(
    renameExpected.contribution_revision,
    "rename ContributionRevision golden",
  );
  const renameTransition = advanceFileIdentities(
    renameInputs.project_id,
    renameInputs.proposal_id,
    renameInputs.base_files,
    renameInputs.result_files,
    [renameInputs.operation],
  );
  checkExpectedSnapshot(
    renameTransition.base_snapshot,
    renameIdentity.base_snapshot,
    "Rename base snapshot",
  );
  checkExpectedSnapshot(
    renameTransition.result_snapshot,
    renameIdentity.result_snapshot,
    "Rename result snapshot",
  );
  for (const [pathHex, expectedValue] of Object.entries(renameBaseIdentityGoldens)) {
    const expectedIdentity = objectAt(expectedValue, `Rename base identity ${pathHex}`);
    const actualIdentity = enrolledFileId(
      renameInputs.project_id,
      renameTransition.base_snapshot.id,
      decodeLowerHex(pathHex),
    );
    checkCanonicalValue(
      actualIdentity,
      expectedIdentity,
      `Rename enrolled FileId preimage ${pathHex}`,
    );
  }
  const renamedFileId = stringAt(renameIdentity.renamed_file_id, "preserved Rename FileId");
  const renamedState = renameTransition.result_files.find(
    (file) => file.path_hex === renameInputs.operation.to_path_hex,
  );
  if (renamedState === undefined) throw new Error("explicit Rename result file is absent");
  check(
    renamedState.file_id === renamedFileId &&
      renamedState.file_id ===
        "file:sha256:a69cfec7645a258107611f9e440d88f784a9a8ebff6e2f69d039aeb6af026e05",
    "explicit Rename did not preserve the source FileId",
  );
  check(
    !renameTransition.result_files.some(
      (file) => file.path_hex === renameInputs.operation.from_path_hex,
    ),
    "explicit Rename retained the removed source path",
  );
  const renameSourceChange = renameTransition.source_changes[0];
  if (renameSourceChange === undefined) throw new Error("explicit Rename source change is absent");
  checkCanonicalText(
    renameSourceChange,
    stringAt(renameSourceChangeExpected.canonical_json, "Rename source-change JSON"),
    stringAt(renameSourceChangeExpected.canonical_hex, "Rename source-change hex"),
    "explicit Rename source change",
  );
  checkCanonicalValue(
    renameSourceChange,
    renameSourceChangeExpected.value,
    "explicit Rename source-change fields",
  );
  checkCanonicalText(
    renameTransition.transition,
    stringAt(renameTransitionExpected.canonical_json, "Rename transition JSON"),
    stringAt(renameTransitionExpected.canonical_hex, "Rename transition hex"),
    "explicit Rename transition",
  );
  checkCanonicalValue(
    renameTransition.transition,
    renameTransitionExpected.value,
    "explicit Rename transition fields",
  );
  const renameRevision = contributionRevisionRecord({
    project_id: renameInputs.project_id,
    contribution_id: renameInputs.contribution_id,
    capture_record_id: renameInputs.capture_record_id,
    base: renameTransition.base_snapshot.id,
    result: renameTransition.result_snapshot.id,
    source_changes: renameTransition.source_changes,
    depends_on: [],
  });
  checkCanonicalText(
    renameRevision.value,
    stringAt(renameRevisionExpected.canonical_json, "Rename revision JSON"),
    stringAt(renameRevisionExpected.canonical_hex, "Rename revision hex"),
    "explicit Rename immutable ContributionRevision",
  );
  checkCanonicalValue(
    renameRevision.value,
    renameRevisionExpected.value,
    "explicit Rename ContributionRevision fields",
  );
  check(
    renameRevision.id === renameRevisionExpected.record_id &&
      renameRevision.id ===
        "sha256:ae98275074fdd0c63af8b5f7e0950df49333ce1aac3cd853ab781287d6a5fb0a",
    "explicit Rename ContributionRevision RecordId mismatch",
  );
  const renameRevisionPreimage = Buffer.concat([
    Buffer.from("aethyme record v0\0", "utf8"),
    renameRevision.canonical_bytes,
  ]);
  check(
    renameRevisionPreimage.toString("hex") === renameRevisionExpected.record_preimage_hex,
    "explicit Rename ContributionRevision preimage mismatch",
  );

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

  check(
    fileStateFiles(mixed.result_snapshot, "mixed result snapshot").length === 5,
    "mixed result snapshot did not retain all five raw-path entries",
  );
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
    pin_counts: {
      rust_sources: REQUIRED_RUST_SOURCES.length,
      typescript_sources: REQUIRED_TYPESCRIPT_SOURCES.length,
      frozen_fixture_files: REQUIRED_FIXTURE_PATHS.length,
    },
    vectors: {
      descriptor: 1,
      contribution_revision_records: 4,
      bound_profile_new_file: 1,
      explicit_rename: 1,
      mixed_copy_split_plain_new: 1,
      contribution_revision_validation_negative_controls: 4,
      retained_invalid_attempts: { v1_contribution_id: "rejected", v2_file_id: "rejected" },
    },
    runtime_boundary: "Node built-ins only; offline; no Cargo, native product, external assets, or generated vector edits",
  };
  console.log(JSON.stringify(report, null, 2));
}

run();
