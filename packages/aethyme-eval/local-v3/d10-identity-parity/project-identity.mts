import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import { TextDecoder } from "node:util";
import {
  canonicalizeValue,
  PROFILE_LIMITS,
  type CanonicalValue,
} from "../t01-canonical-parity/canonical-json.mts";

export type FileVector = {
  path_hex: string;
  mode: string;
  content_hex: string;
};

export type SnapshotFile = {
  content_sha256: string;
  mode: string;
  path_hex: string;
};

export type Snapshot = {
  id: string;
  manifest_hex: string;
  files_sorted_by_raw_path: SnapshotFile[];
};

export type FileState = SnapshotFile & {
  file_id: string;
};

export type RenameOperation = {
  kind: "rename";
  from_path_hex: string;
  to_path_hex: string;
};

export type CopyOperation = {
  kind: "copy";
  from_path_hex: string;
  to_path_hex: string;
};

export type SplitOperation = {
  kind: "split";
  from_path_hex: string;
  to_path_hexes_input_order: string[];
};

export type FileOperation = RenameOperation | CopyOperation | SplitOperation;

export type IdentityChange = {
  before?: FileState;
  after?: FileState;
  relation: string;
  derived_from: string[];
  possible_sources: string[];
};

export class IdentityProfileError extends Error {
  readonly code: string;

  constructor(code: string) {
    super(code);
    this.name = "IdentityProfileError";
    this.code = code;
  }
}

const BASE32 = "abcdefghijklmnopqrstuvwxyz234567";
const ALLOWED_MODES = new Set(["100644", "100755", "120000"]);
const MAX_PATH_BYTES = 4096;
const SNAPSHOT_DOMAIN = Buffer.from("aethyme source-snapshot v0\0", "utf8");
const FILE_ID_DOMAIN = Buffer.from("aethyme file identity v0\0", "utf8");
const PROPOSED_FILE_ID_DOMAIN = Buffer.from(
  "aethyme proposed file identity v0\0",
  "utf8",
);
const RECORD_DOMAIN = Buffer.from("aethyme record v0\0", "utf8");

let hashCallCount = 0;

function fail(code: string): never {
  throw new IdentityProfileError(code);
}

export function resetHashCallCount(): void {
  hashCallCount = 0;
}

export function getHashCallCount(): number {
  return hashCallCount;
}

export function immutableRecordId(canonicalBytes: Uint8Array): string {
  return `sha256:${sha256(Buffer.concat([RECORD_DOMAIN, Buffer.from(canonicalBytes)]))}`;
}

function sha256(bytes: Uint8Array): string {
  hashCallCount += 1;
  return createHash("sha256").update(bytes).digest("hex");
}

export function decodeLowerHex(value: string): Buffer {
  if (value.length % 2 !== 0 || !/^[0-9a-f]*$/.test(value)) {
    fail("invalid_hex");
  }
  const decoded = Buffer.from(value, "hex");
  if (decoded.toString("hex") !== value) fail("invalid_hex");
  return decoded;
}

function lowerHex(bytes: Uint8Array): string {
  return Buffer.from(bytes).toString("hex");
}

export function validateCommit(value: string): string {
  if (
    (value.length !== 40 && value.length !== 64) ||
    /[^0-9a-f]/.test(value)
  ) {
    fail("invalid_commit");
  }
  return value;
}

function validateSha256Id(value: string, prefix: string, code: string): string {
  if (
    value.length !== prefix.length + 64 ||
    !new RegExp(`^${prefix}[0-9a-f]{64}$`).test(value)
  ) {
    fail(code);
  }
  return value;
}

function parseOpaqueId(value: string, prefix: string, code: string): string {
  if (!value.startsWith(prefix)) fail(code);
  const suffix = value.slice(prefix.length);
  if (suffix.length !== 26 || !/^[a-z2-7]{26}$/.test(suffix)) fail(code);
  const tail = BASE32.indexOf(suffix[suffix.length - 1] ?? "");
  if (tail < 0 || tail % 4 !== 0) fail(code);
  return value;
}

export function parseProjectId(value: string): string {
  return parseOpaqueId(value, "proj:", "invalid_project_id");
}

export function parseContributionId(value: string): string {
  return parseOpaqueId(value, "contrib:", "invalid_contribution_id");
}

export function validateProposalId(value: string): string {
  if (value.length === 0 || value.length > 128 || /[^A-Za-z0-9._:-]/.test(value)) {
    fail("invalid_proposal_id");
  }
  return value;
}

export function isCanonicalFileId(value: string): boolean {
  return value.length === "file:sha256:".length + 64 &&
    /^file:sha256:[0-9a-f]{64}$/.test(value);
}

export function parseFileId(value: string): string {
  if (!isCanonicalFileId(value)) fail("invalid_file_id");
  return value;
}

const hfsUtf8Decoder = new TextDecoder("utf-8", { fatal: true });

function asciiEqualsIgnoreCase(bytes: Uint8Array, expected: string): boolean {
  if (bytes.length !== expected.length) return false;
  for (let index = 0; index < bytes.length; index += 1) {
    const byte = bytes[index]!;
    const folded = byte >= 0x41 && byte <= 0x5a ? byte + 0x20 : byte;
    if (folded !== expected.charCodeAt(index)) return false;
  }
  return true;
}

function isGitComponent(component: Buffer): boolean {
  let hfsFiltered = component;
  try {
    const text = hfsUtf8Decoder.decode(component);
    hfsFiltered = Buffer.from(
      [...text]
        .filter((character) => {
          const point = character.codePointAt(0)!;
          return !(
            (point >= 0x200c && point <= 0x200f) ||
            (point >= 0x202a && point <= 0x202e) ||
            (point >= 0x206a && point <= 0x206f) ||
            point === 0xfeff
          );
        })
        .join(""),
      "utf8",
    );
  } catch {
    // Rust falls back to the original bytes when a component is not UTF-8.
  }
  if (asciiEqualsIgnoreCase(hfsFiltered, ".git")) return true;

  const colon = component.indexOf(0x3a);
  let stem = component.subarray(0, colon < 0 ? component.length : colon);
  let end = stem.length;
  while (end > 0 && (stem[end - 1] === 0x2e || stem[end - 1] === 0x20)) {
    end -= 1;
  }
  stem = stem.subarray(0, end);
  return asciiEqualsIgnoreCase(stem, ".git") || asciiEqualsIgnoreCase(stem, "git~1");
}

function validatePath(path: Uint8Array): Buffer {
  const raw = Buffer.from(path);
  if (raw.length === 0 || raw.length > MAX_PATH_BYTES || raw.includes(0) || raw[0] === 0x2f) {
    fail("invalid_path");
  }
  if (raw[raw.length - 1] === 0x2f) fail("invalid_path");
  let componentStart = 0;
  for (let index = 0; index <= raw.length; index += 1) {
    if (index !== raw.length && raw[index] !== 0x2f) continue;
    const component = raw.subarray(componentStart, index);
    if (component.length === 0) fail("invalid_path");
    if (
      (component.length === 1 && component[0] === 0x2e) ||
      (component.length === 2 && component[0] === 0x2e && component[1] === 0x2e) ||
      isGitComponent(component)
    ) {
      fail("invalid_path");
    }
    componentStart = index + 1;
  }
  return raw;
}

function u64be(value: number): Buffer {
  if (!Number.isSafeInteger(value) || value < 0) fail("invalid_ordinal");
  const result = Buffer.alloc(8);
  result.writeBigUInt64BE(BigInt(value));
  return result;
}

function compareRawPath(left: Uint8Array, right: Uint8Array): number {
  return Buffer.compare(Buffer.from(left), Buffer.from(right));
}

function validateSnapshotPathSet(paths: readonly Buffer[]): void {
  const sorted = [...paths].sort(compareRawPath);
  for (let index = 1; index < sorted.length; index += 1) {
    if (compareRawPath(sorted[index - 1]!, sorted[index]!) === 0) {
      fail("duplicate_path");
    }
  }
  const directoryPrefixes = new Set<string>();
  for (const path of sorted) {
    for (let index = 0; index < path.length; index += 1) {
      if (path[index] === 0x2f) directoryPrefixes.add(lowerHex(path.subarray(0, index)));
    }
  }
  if (sorted.some((path) => directoryPrefixes.has(lowerHex(path)))) {
    fail("file_directory_conflict");
  }
}

function contentBytes(file: FileVector): Buffer {
  if (!ALLOWED_MODES.has(file.mode)) fail("unsupported_mode");
  return decodeLowerHex(file.content_hex);
}

function normalizedFile(file: FileVector, validatedPath?: Buffer): {
  path: Buffer;
  mode: string;
  content: Buffer;
  content_sha256: string;
  path_hex: string;
} {
  const path = validatedPath ?? validatePath(decodeLowerHex(file.path_hex));
  const content = contentBytes(file);
  return {
    path,
    mode: file.mode,
    content,
    content_sha256: sha256(content),
    path_hex: lowerHex(path),
  };
}

export function sourceSnapshot(entries: FileVector[]): Snapshot {
  const paths = entries.map((file) => validatePath(decodeLowerHex(file.path_hex)));
  validateSnapshotPathSet(paths);
  const files = entries.map((file, index) => normalizedFile(file, paths[index]!));
  files.sort((left, right) => compareRawPath(left.path, right.path));

  const manifestParts: Buffer[] = [SNAPSHOT_DOMAIN];
  for (const file of files) {
    manifestParts.push(
      Buffer.from(`${file.mode} ${file.content_sha256} `, "ascii"),
      file.path,
      Buffer.from([0]),
    );
  }
  const manifest = Buffer.concat(manifestParts);
  return {
    id: `sha256:${sha256(manifest)}`,
    manifest_hex: lowerHex(manifest),
    files_sorted_by_raw_path: files.map(({ content_sha256, mode, path_hex }) => ({
      content_sha256,
      mode,
      path_hex,
    })),
  };
}

export function enrolledFileId(
  projectId: string,
  snapshotId: string,
  rawPath: Uint8Array,
): { file_id: string; preimage_hex: string } {
  parseProjectId(projectId);
  validateSha256Id(snapshotId, "sha256:", "invalid_snapshot_id");
  const path = validatePath(rawPath);
  const preimage = Buffer.concat([
    FILE_ID_DOMAIN,
    Buffer.from(projectId, "ascii"),
    Buffer.from([0]),
    Buffer.from(snapshotId, "ascii"),
    Buffer.from([0]),
    u64be(path.length),
    path,
  ]);
  return {
    file_id: parseFileId(`file:sha256:${sha256(preimage)}`),
    preimage_hex: lowerHex(preimage),
  };
}

export function proposedFileId(
  projectId: string,
  proposalId: string,
  resultSnapshotId: string,
  ordinal: number,
  rawPath: Uint8Array,
): { file_id: string; preimage_hex: string } {
  parseProjectId(projectId);
  validateProposalId(proposalId);
  validateSha256Id(resultSnapshotId, "sha256:", "invalid_snapshot_id");
  const path = validatePath(rawPath);
  const preimage = Buffer.concat([
    PROPOSED_FILE_ID_DOMAIN,
    Buffer.from(projectId, "ascii"),
    Buffer.from([0]),
    Buffer.from(proposalId, "ascii"),
    Buffer.from([0]),
    Buffer.from(resultSnapshotId, "ascii"),
    Buffer.from([0]),
    u64be(ordinal),
    u64be(path.length),
    path,
  ]);
  return {
    file_id: parseFileId(`file:sha256:${sha256(preimage)}`),
    preimage_hex: lowerHex(preimage),
  };
}

export function projectDescriptorRecord(
  projectId: string,
  anchorCommit: string,
  snapshot: Snapshot,
): {
  value: Map<string, CanonicalValue>;
  canonical_bytes: Buffer;
  id: string;
  enrolled_files: Array<{ state: FileState; preimage_hex: string }>;
} {
  parseProjectId(projectId);
  validateCommit(anchorCommit);
  const entries = snapshot.files_sorted_by_raw_path;
  if (entries.length > PROFILE_LIMITS.max_container_entries) fail("too_many_files");

  const enrolled_files = entries.map((entry) => {
    const identity = enrolledFileId(
      projectId,
      snapshot.id,
      decodeLowerHex(entry.path_hex),
    );
    return {
      state: { ...entry, file_id: identity.file_id },
      preimage_hex: identity.preimage_hex,
    };
  });
  const descriptorFiles = enrolled_files.map(({ state }) =>
    new Map<string, CanonicalValue>([
      ["path_hex", state.path_hex],
      ["mode", state.mode],
      ["content_sha256", state.content_sha256],
      ["file_id", state.file_id],
    ]),
  );
  const value = new Map<string, CanonicalValue>([
    ["schema", "aethyme.project-descriptor/experimental-v0"],
    ["project_id", projectId],
    ["anchor_commit", anchorCommit],
    ["anchor_snapshot", snapshot.id],
    ["source_binding", `git-commit:${anchorCommit}`],
    ["policy_location", ".aethyme/config.toml"],
    ["acceptance_target", "unbound"],
    ["visibility_boundary", "local"],
    ["files", descriptorFiles],
  ]);
  const canonicalBytes = canonicalizeValue(value);
  if (canonicalBytes.length > PROFILE_LIMITS.max_document_bytes) {
    fail("descriptor_too_large");
  }
  return {
    value,
    canonical_bytes: canonicalBytes,
    id: immutableRecordId(canonicalBytes),
    enrolled_files,
  };
}

function fileStateMap(file: FileState): Map<string, CanonicalValue> {
  return new Map([
    ["path_hex", file.path_hex],
    ["mode", file.mode],
    ["content_sha256", file.content_sha256],
    ["file_id", parseFileId(file.file_id)],
  ]);
}

function changeMap(change: IdentityChange): Map<string, CanonicalValue> {
  const result = new Map<string, CanonicalValue>([
    ["relation", change.relation],
    ["derived_from", change.derived_from.map(parseFileId)],
    ["possible_sources", change.possible_sources.map(parseFileId)],
  ]);
  if (change.before !== undefined) result.set("before", fileStateMap(change.before));
  if (change.after !== undefined) result.set("after", fileStateMap(change.after));
  return result;
}

function sameContent(left: FileVector, right: FileVector): boolean {
  return (
    left.mode === right.mode &&
    decodeLowerHex(left.content_hex).equals(decodeLowerHex(right.content_hex))
  );
}

function stateFrom(
  file: FileVector,
  fileId: string,
  contentHash?: string,
): FileState {
  const path = validatePath(decodeLowerHex(file.path_hex));
  return {
    path_hex: lowerHex(path),
    mode: file.mode,
    content_sha256: contentHash ?? sha256(contentBytes(file)),
    file_id: parseFileId(fileId),
  };
}

function fileEntryMap(files: FileVector[]): Map<string, FileVector> {
  const entries = new Map<string, FileVector>();
  for (const file of files) {
    const path = validatePath(decodeLowerHex(file.path_hex));
    const key = lowerHex(path);
    if (entries.has(key)) fail("duplicate_path");
    contentBytes(file);
    entries.set(key, file);
  }
  return entries;
}

function mapByRawPath<T>(entries: Map<string, T>): Array<[string, T]> {
  return [...entries.entries()].sort(([left], [right]) =>
    compareRawPath(decodeLowerHex(left), decodeLowerHex(right)),
  );
}

export function advanceFileIdentities(
  projectId: string,
  proposalId: string,
  baseFiles: FileVector[],
  resultFiles: FileVector[],
  operations: FileOperation[],
): {
  base_snapshot: Snapshot;
  result_snapshot: Snapshot;
  base_files: FileState[];
  result_files: FileState[];
  changes: IdentityChange[];
  source_changes: CanonicalValue[];
  transition: Map<string, CanonicalValue>;
  canonical_bytes: Buffer;
  proposed_preimages: Map<string, { file_id: string; ordinal: number; preimage_hex: string }>;
} {
  parseProjectId(projectId);
  validateProposalId(proposalId);
  const baseSnapshot = sourceSnapshot(baseFiles);
  const resultSnapshot = sourceSnapshot(resultFiles);
  const baseEntries = fileEntryMap(baseFiles);
  const resultEntries = fileEntryMap(resultFiles);
  const baseIds = new Map<string, string>();
  const baseStates = new Map<string, FileState>();
  for (const [path, file] of mapByRawPath(baseEntries)) {
    const identity = enrolledFileId(
      projectId,
      baseSnapshot.id,
      decodeLowerHex(path),
    );
    const normalized = normalizedFile(file);
    baseIds.set(path, identity.file_id);
    baseStates.set(path, stateFrom(file, identity.file_id, normalized.content_sha256));
  }

  const destinationIds = new Map<
    string,
    {
      relation: string;
      file_id: string;
      derived_from: string[];
      ordinal?: number;
      preimage_hex?: string;
    }
  >();
  const sourceOperations = new Map<string, { relation: string; destinations: string[] }>();
  const usedOperationPaths = new Set<string>();

  for (const operation of operations) {
    const fromPath = lowerHex(validatePath(decodeLowerHex(operation.from_path_hex)));
    if (!baseEntries.has(fromPath)) fail("invalid_operation_source");
    if (operation.kind === "rename") {
      const toPath = lowerHex(validatePath(decodeLowerHex(operation.to_path_hex)));
      const before = baseEntries.get(fromPath)!;
      const after = resultEntries.get(toPath);
      if (
        after === undefined ||
        baseEntries.has(toPath) ||
        resultEntries.has(fromPath) ||
        !sameContent(before, after) ||
        usedOperationPaths.has(fromPath) ||
        usedOperationPaths.has(toPath)
      ) {
        fail("invalid_rename_operation");
      }
      usedOperationPaths.add(fromPath);
      usedOperationPaths.add(toPath);
      sourceOperations.set(fromPath, {
        relation: "explicit_rename",
        destinations: [toPath],
      });
      destinationIds.set(toPath, {
        relation: "explicit_rename",
        file_id: baseIds.get(fromPath)!,
        derived_from: [],
      });
    } else if (operation.kind === "copy") {
      const toPath = lowerHex(validatePath(decodeLowerHex(operation.to_path_hex)));
      const sourceAfter = resultEntries.get(fromPath);
      const copied = resultEntries.get(toPath);
      const sourceBefore = baseEntries.get(fromPath)!;
      if (
        sourceAfter === undefined ||
        copied === undefined ||
        baseEntries.has(toPath) ||
        !sameContent(sourceBefore, sourceAfter) ||
        !sameContent(sourceBefore, copied) ||
        usedOperationPaths.has(toPath)
      ) {
        fail("invalid_copy_operation");
      }
      usedOperationPaths.add(toPath);
      const identity = proposedFileId(
        projectId,
        proposalId,
        resultSnapshot.id,
        0,
        decodeLowerHex(toPath),
      );
      destinationIds.set(toPath, {
        relation: "explicit_copy",
        file_id: identity.file_id,
        derived_from: [baseIds.get(fromPath)!],
        ordinal: 0,
        preimage_hex: identity.preimage_hex,
      });
    } else {
      const destinations = operation.to_path_hexes_input_order.map((pathHex) =>
        lowerHex(validatePath(decodeLowerHex(pathHex))),
      );
      const sortedDestinations = [...destinations].sort((left, right) =>
        compareRawPath(decodeLowerHex(left), decodeLowerHex(right)),
      );
      if (
        destinations.length < 2 ||
        new Set(destinations).size !== destinations.length ||
        resultEntries.has(fromPath) ||
        sourceOperations.has(fromPath) ||
        usedOperationPaths.has(fromPath)
      ) {
        fail("invalid_split_operation");
      }
      sourceOperations.set(fromPath, {
        relation: "explicit_split",
        destinations: sortedDestinations,
      });
      usedOperationPaths.add(fromPath);
      sortedDestinations.forEach((toPath, ordinal) => {
        if (
          baseEntries.has(toPath) ||
          !resultEntries.has(toPath) ||
          usedOperationPaths.has(toPath)
        ) {
          fail("invalid_split_operation");
        }
        usedOperationPaths.add(toPath);
        const identity = proposedFileId(
          projectId,
          proposalId,
          resultSnapshot.id,
          ordinal,
          decodeLowerHex(toPath),
        );
        destinationIds.set(toPath, {
          relation: "explicit_split",
          file_id: identity.file_id,
          derived_from: [baseIds.get(fromPath)!],
          ordinal,
          preimage_hex: identity.preimage_hex,
        });
      });
    }
  }

  const newPaths = mapByRawPath(resultEntries)
    .map(([path]) => path)
    .filter((path) => !baseEntries.has(path));
  const globalOrdinals = new Map<string, number>();
  newPaths.forEach((path, ordinal) => globalOrdinals.set(path, ordinal));
  const proposedPreimages = new Map<
    string,
    { file_id: string; ordinal: number; preimage_hex: string }
  >();
  const resultStates = new Map<string, FileState>();
  for (const [path, file] of mapByRawPath(resultEntries)) {
    const existingId = baseIds.get(path);
    if (existingId !== undefined) {
      resultStates.set(path, baseStates.get(path)!);
      continue;
    }
    const operated = destinationIds.get(path);
    if (operated !== undefined) {
      resultStates.set(path, stateFrom(file, operated.file_id));
      if (operated.ordinal !== undefined && operated.preimage_hex !== undefined) {
        proposedPreimages.set(path, {
          file_id: operated.file_id,
          ordinal: operated.ordinal,
          preimage_hex: operated.preimage_hex,
        });
      }
      continue;
    }
    const ordinal = globalOrdinals.get(path);
    if (ordinal === undefined) fail("missing_new_path_ordinal");
    const identity = proposedFileId(
      projectId,
      proposalId,
      resultSnapshot.id,
      ordinal,
      decodeLowerHex(path),
    );
    resultStates.set(path, stateFrom(file, identity.file_id));
    proposedPreimages.set(path, {
      file_id: identity.file_id,
      ordinal,
      preimage_hex: identity.preimage_hex,
    });
  }

  const changes: IdentityChange[] = [];
  for (const [path, before] of mapByRawPath(baseStates)) {
    const sourceOperation = sourceOperations.get(path);
    if (sourceOperation !== undefined) {
      if (sourceOperation.relation === "explicit_rename") {
        const after = resultStates.get(sourceOperation.destinations[0]!);
        if (after === undefined) fail("missing_rename_destination");
        changes.push({
          before,
          after,
          relation: "explicit_rename",
          derived_from: [],
          possible_sources: [],
        });
      } else {
        changes.push({
          before,
          relation: "removed",
          derived_from: [],
          possible_sources: [],
        });
      }
      continue;
    }
    const after = resultStates.get(path);
    if (after === undefined) {
      changes.push({
        before,
        relation: "removed",
        derived_from: [],
        possible_sources: [],
      });
    } else if (
      before.mode !== after.mode ||
      before.content_sha256 !== after.content_sha256
    ) {
      changes.push({
        before,
        after,
        relation: "same_path",
        derived_from: [],
        possible_sources: [],
      });
    }
  }
  for (const [path, after] of mapByRawPath(resultStates)) {
    if (baseEntries.has(path)) continue;
    const operated = destinationIds.get(path);
    if (operated !== undefined) {
      if (operated.relation === "explicit_rename") continue;
      changes.push({
        after,
        relation: operated.relation,
        derived_from: operated.derived_from,
        possible_sources: [],
      });
      continue;
    }
    const possibleSources = mapByRawPath(baseEntries)
      .filter(
        ([basePath, base]) =>
          !resultEntries.has(basePath) && sameContent(base, resultEntries.get(path)!),
      )
      .map(([basePath]) => baseIds.get(basePath)!);
    changes.push({
      after,
      relation: possibleSources.length === 0 ? "new" : "uncertain_rename",
      derived_from: [],
      possible_sources: possibleSources,
    });
  }
  changes.sort((left, right) => {
    const leftPath = left.after?.path_hex ?? left.before?.path_hex;
    const rightPath = right.after?.path_hex ?? right.before?.path_hex;
    if (leftPath === undefined || rightPath === undefined) fail("invalid_change_path");
    return (
      compareRawPath(decodeLowerHex(leftPath), decodeLowerHex(rightPath)) ||
      (left.relation < right.relation ? -1 : left.relation > right.relation ? 1 : 0)
    );
  });

  const sortedResultStates = mapByRawPath(resultStates).map(([, state]) => state);
  const transition = new Map<string, CanonicalValue>([
    ["base_snapshot_id", baseSnapshot.id],
    ["changes", changes.map(changeMap)],
    ["project_id", projectId],
    ["proposal_id", proposalId],
    ["result_files", sortedResultStates.map(fileStateMap)],
    ["result_snapshot_id", resultSnapshot.id],
  ]);
  return {
    base_snapshot: baseSnapshot,
    result_snapshot: resultSnapshot,
    base_files: mapByRawPath(baseStates).map(([, state]) => state),
    result_files: sortedResultStates,
    changes,
    source_changes: changes.map(changeMap),
    transition,
    canonical_bytes: canonicalizeValue(transition),
    proposed_preimages: proposedPreimages,
  };
}

type ParsedSourceChangeState = {
  path: Buffer;
  file_id: string;
};

function sourceChangeObject(value: CanonicalValue): Map<string, CanonicalValue> {
  if (!(value instanceof Map)) fail("invalid_source_change");
  return value;
}

function sourceChangeString(
  object: Map<string, CanonicalValue>,
  field: string,
): string {
  const value = object.get(field);
  if (typeof value !== "string") fail("invalid_source_change");
  return value;
}

function sourceChangeState(
  object: Map<string, CanonicalValue>,
  field: "before" | "after",
): ParsedSourceChangeState | undefined {
  if (!object.has(field)) return undefined;
  const stateValue = object.get(field);
  if (!(stateValue instanceof Map)) fail("invalid_source_change");
  const allowed = new Set(["path_hex", "mode", "content_sha256", "file_id"]);
  for (const key of stateValue.keys()) {
    if (!allowed.has(key)) fail("invalid_source_change");
  }
  const pathHex = stateValue.get("path_hex");
  const mode = stateValue.get("mode");
  const contentSha256 = stateValue.get("content_sha256");
  const fileId = stateValue.get("file_id");
  if (
    typeof pathHex !== "string" ||
    typeof mode !== "string" ||
    typeof contentSha256 !== "string" ||
    typeof fileId !== "string"
  ) {
    fail("invalid_source_change");
  }
  if (!ALLOWED_MODES.has(mode)) fail("invalid_source_change");
  const path = validatePath(decodeLowerHex(pathHex));
  if (!/^[0-9a-f]{64}$/.test(contentSha256)) fail("invalid_source_change");
  if (decodeLowerHex(contentSha256).length !== 32) fail("invalid_source_change");
  return { path, file_id: parseFileId(fileId) };
}

function sourceChangeIds(
  object: Map<string, CanonicalValue>,
  field: "derived_from" | "possible_sources",
): string[] {
  const values = object.get(field);
  if (!Array.isArray(values)) fail("invalid_source_change");
  if (values.length > PROFILE_LIMITS.max_container_entries) fail("too_many_references");
  return values.map((value) => {
    if (typeof value !== "string") fail("invalid_source_change");
    return parseFileId(value);
  });
}

function validateContributionChanges(changes: CanonicalValue[]): void {
  const allowedFields = new Set([
    "before",
    "after",
    "relation",
    "derived_from",
    "possible_sources",
  ]);
  let previousPath: Buffer | undefined;
  for (const value of changes) {
    const change = sourceChangeObject(value);
    for (const key of change.keys()) {
      if (!allowedFields.has(key)) fail("invalid_source_change");
    }
    const relation = sourceChangeString(change, "relation");
    const before = sourceChangeState(change, "before");
    const after = sourceChangeState(change, "after");
    const sources = sourceChangeIds(change, "derived_from");
    const possible = sourceChangeIds(change, "possible_sources");
    let shapeIsValid = false;
    switch (relation) {
      case "same_path":
        shapeIsValid =
          before !== undefined &&
          after !== undefined &&
          Buffer.compare(before.path, after.path) === 0 &&
          before.file_id === after.file_id &&
          sources.length === 0 &&
          possible.length === 0;
        break;
      case "explicit_rename":
        shapeIsValid =
          before !== undefined &&
          after !== undefined &&
          Buffer.compare(before.path, after.path) !== 0 &&
          before.file_id === after.file_id &&
          sources.length === 0 &&
          possible.length === 0;
        break;
      case "explicit_copy":
      case "explicit_split":
        shapeIsValid =
          before === undefined &&
          after !== undefined &&
          !sources.includes(after.file_id) &&
          sources.length > 0 &&
          (relation !== "explicit_copy" || sources.length === 1) &&
          possible.length === 0;
        break;
      case "new":
        shapeIsValid =
          before === undefined &&
          after !== undefined &&
          sources.length === 0 &&
          possible.length === 0;
        break;
      case "removed":
        shapeIsValid =
          before !== undefined &&
          after === undefined &&
          sources.length === 0 &&
          possible.length === 0;
        break;
      case "uncertain_rename":
        shapeIsValid =
          before === undefined &&
          after !== undefined &&
          sources.length === 0 &&
          possible.length > 0;
        break;
      default:
        fail("invalid_source_change");
    }
    if (!shapeIsValid) fail("invalid_source_change");
    const path = after?.path ?? before?.path;
    if (path === undefined) fail("invalid_source_change");
    if (previousPath !== undefined && Buffer.compare(previousPath, path) >= 0) {
      fail("non_canonical_change_order");
    }
    previousPath = path;
  }
}

export function contributionRevisionRecord(args: {
  project_id: string;
  contribution_id: string;
  capture_record_id: string;
  base: string;
  result: string;
  source_changes: CanonicalValue[];
  depends_on?: string[];
  brief_record_id?: string;
  brief_object_sha256?: string;
}): { value: Map<string, CanonicalValue>; canonical_bytes: Buffer; id: string } {
  parseProjectId(args.project_id);
  parseContributionId(args.contribution_id);
  validateSha256Id(args.capture_record_id, "sha256:", "invalid_capture_record_id");
  validateSha256Id(args.base, "sha256:", "invalid_base_snapshot_id");
  validateSha256Id(args.result, "sha256:", "invalid_result_snapshot_id");
  if (args.source_changes.length > PROFILE_LIMITS.max_container_entries) {
    fail("too_many_changes");
  }
  const suppliedDependencies = args.depends_on ?? [];
  if (suppliedDependencies.length > PROFILE_LIMITS.max_container_entries) {
    fail("too_many_dependencies");
  }
  validateContributionChanges(args.source_changes);
  const dependencies = [...suppliedDependencies].sort();
  for (let index = 0; index < dependencies.length; index += 1) {
    validateSha256Id(dependencies[index]!, "sha256:", "invalid_dependency_id");
    if (index > 0 && dependencies[index - 1] === dependencies[index]) {
      fail("duplicate_dependency");
    }
  }
  const fields = new Map<string, CanonicalValue>([
    ["schema", "aethyme.contribution-revision/experimental-v0"],
    ["project_id", args.project_id],
    ["contribution_id", args.contribution_id],
    ["capture_record_id", args.capture_record_id],
    ["base", args.base],
    ["result", args.result],
    ["source_changes", args.source_changes],
    ["depends_on", dependencies],
  ]);
  if (args.brief_record_id !== undefined) {
    validateSha256Id(args.brief_record_id, "sha256:", "invalid_brief_record_id");
    fields.set("brief_record_id", args.brief_record_id);
  }
  if (args.brief_object_sha256 !== undefined) {
    if (
      args.brief_object_sha256.length !== 64 ||
      !/^[0-9a-f]{64}$/.test(args.brief_object_sha256)
    ) {
      fail("invalid_brief_object_sha256");
    }
    fields.set("brief_object_sha256", args.brief_object_sha256);
  }
  if ((args.brief_record_id === undefined) !== (args.brief_object_sha256 === undefined)) {
    fail("incomplete_brief_reference");
  }
  const canonicalBytes = canonicalizeValue(fields);
  if (canonicalBytes.length > PROFILE_LIMITS.max_document_bytes) {
    fail("too_large_revision");
  }
  return {
    value: fields,
    canonical_bytes: canonicalBytes,
    id: `sha256:${sha256(Buffer.concat([RECORD_DOMAIN, canonicalBytes]))}`,
  };
}
