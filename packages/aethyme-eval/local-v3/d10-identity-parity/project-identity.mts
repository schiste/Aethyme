import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import {
  canonicalizeValue,
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

export type FileOperation = CopyOperation | SplitOperation;

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
  return Buffer.from(value, "hex");
}

function lowerHex(bytes: Uint8Array): string {
  return Buffer.from(bytes).toString("hex");
}

function validateCommit(value: string): string {
  if (!/^[0-9a-f]{40}$/.test(value)) fail("invalid_commit");
  return value;
}

function validateSha256Id(value: string, prefix: string, code: string): string {
  if (!new RegExp(`^${prefix}[0-9a-f]{64}$`).test(value)) fail(code);
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

export function isCanonicalFileId(value: string): boolean {
  return /^file:sha256:[0-9a-f]{64}$/.test(value);
}

export function parseFileId(value: string): string {
  if (!isCanonicalFileId(value)) fail("invalid_file_id");
  return value;
}

function validatePath(path: Uint8Array): Buffer {
  const raw = Buffer.from(path);
  if (raw.length === 0 || raw.includes(0) || raw[0] === 0x2f) {
    fail("invalid_path");
  }
  for (const component of raw.toString("latin1").split("/")) {
    if (component === "" || component === "." || component === "..") {
      fail("invalid_path");
    }
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

function contentBytes(file: FileVector): Buffer {
  if (!ALLOWED_MODES.has(file.mode)) fail("unsupported_mode");
  return decodeLowerHex(file.content_hex);
}

function normalizedFile(file: FileVector): {
  path: Buffer;
  mode: string;
  content: Buffer;
  content_sha256: string;
  path_hex: string;
} {
  const path = validatePath(decodeLowerHex(file.path_hex));
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
  const files = entries.map(normalizedFile);
  files.sort((left, right) => compareRawPath(left.path, right.path));
  for (let index = 1; index < files.length; index += 1) {
    if (compareRawPath(files[index - 1]!.path, files[index]!.path) === 0) {
      fail("duplicate_path");
    }
  }

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
  parseContributionId(proposalId);
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
  parseContributionId(proposalId);
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
    { relation: string; file_id: string; derived_from: string[]; ordinal: number; preimage_hex: string }
  >();
  const sourceOperations = new Map<string, { relation: string; destinations: string[] }>();
  const usedOperationPaths = new Set<string>();

  for (const operation of operations) {
    const fromPath = lowerHex(validatePath(decodeLowerHex(operation.from_path_hex)));
    if (!baseEntries.has(fromPath)) fail("invalid_operation_source");
    if (operation.kind === "copy") {
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
      proposedPreimages.set(path, {
        file_id: operated.file_id,
        ordinal: operated.ordinal,
        preimage_hex: operated.preimage_hex,
      });
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
      changes.push({
        before,
        relation: "removed",
        derived_from: [],
        possible_sources: [],
      });
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
  const dependencies = [...(args.depends_on ?? [])].sort();
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
    if (!/^[0-9a-f]{64}$/.test(args.brief_object_sha256)) {
      fail("invalid_brief_object_sha256");
    }
    fields.set("brief_object_sha256", args.brief_object_sha256);
  }
  if ((args.brief_record_id === undefined) !== (args.brief_object_sha256 === undefined)) {
    fail("incomplete_brief_reference");
  }
  const canonicalBytes = canonicalizeValue(fields);
  return {
    value: fields,
    canonical_bytes: canonicalBytes,
    id: `sha256:${sha256(Buffer.concat([RECORD_DOMAIN, canonicalBytes]))}`,
  };
}
