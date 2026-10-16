import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";

export type SnapshotEntryVector = {
  path_hex: string;
  mode: string;
  content_hex: string;
};

export type SnapshotVector = {
  name: string;
  entries: SnapshotEntryVector[];
  manifest_hex?: string;
  id?: string;
  error?: string;
};

export type SnapshotVectors = {
  valid: SnapshotVector[];
  invalid: SnapshotVector[];
  unsupported_modes: string[];
  malformed_ids: string[];
};

type EntryKind = {
  mode: "100644" | "100755" | "120000";
};

type SourceEntry = {
  path: Buffer;
  kind: EntryKind;
  contentSha256: Buffer;
};

export class SnapshotProfileError extends Error {
  readonly code: string;

  constructor(code: string) {
    super(code);
    this.name = "SnapshotProfileError";
    this.code = code;
  }
}

const MANIFEST_HEADER = Buffer.from("aethyme source-snapshot v0\0", "utf8");
const MAX_PATH_BYTES = 4096;
const utf8Decoder = new TextDecoder("utf-8", { fatal: true });

function snapshotError(code: string): never {
  throw new SnapshotProfileError(code);
}

function decodeHex(encoded: string): Buffer {
  if (
    encoded.length % 2 !== 0 ||
    !/^[0-9a-f]*$/.test(encoded)
  ) {
    throw new Error("invalid frozen lowercase hex");
  }
  return Buffer.from(encoded, "hex");
}

function hex(bytes: Uint8Array): string {
  return Buffer.from(bytes).toString("hex");
}

function compareBytes(left: Uint8Array, right: Uint8Array): number {
  const limit = Math.min(left.byteLength, right.byteLength);
  for (let index = 0; index < limit; index += 1) {
    const difference = left[index] - right[index];
    if (difference !== 0) return difference;
  }
  return left.byteLength - right.byteLength;
}

function equalAsciiCaseInsensitive(bytes: Uint8Array, expected: string): boolean {
  if (bytes.byteLength !== expected.length) return false;
  for (let index = 0; index < bytes.byteLength; index += 1) {
    const byte = bytes[index];
    const lower = byte >= 0x41 && byte <= 0x5a ? byte + 0x20 : byte;
    if (lower !== expected.charCodeAt(index)) return false;
  }
  return true;
}

function isGitComponent(component: Uint8Array): boolean {
  let hfsEquivalent = Buffer.from(component);
  try {
    const decoded = utf8Decoder.decode(component);
    const filtered = [...decoded]
      .filter(
        (character) =>
          !(
            (character >= "\u200c" && character <= "\u200f") ||
            (character >= "\u202a" && character <= "\u202e") ||
            (character >= "\u206a" && character <= "\u206f") ||
            character === "\ufeff"
          ),
      )
      .join("");
    hfsEquivalent = Buffer.from(filtered, "utf8");
  } catch {
    // Rust keeps invalid UTF-8 path components byte-for-byte for this check.
  }

  if (equalAsciiCaseInsensitive(hfsEquivalent, ".git")) return true;

  let colon = component.indexOf(0x3a);
  if (colon < 0) colon = component.byteLength;
  let stem = Buffer.from(component.subarray(0, colon));
  while (
    stem.byteLength > 0 &&
    (stem[stem.byteLength - 1] === 0x2e || stem[stem.byteLength - 1] === 0x20)
  ) {
    stem = stem.subarray(0, stem.byteLength - 1);
  }
  return (
    equalAsciiCaseInsensitive(stem, ".git") ||
    equalAsciiCaseInsensitive(stem, "git~1")
  );
}

function validatePath(path: Uint8Array): void {
  if (path.byteLength === 0) snapshotError("invalid_path:empty");
  if (path.byteLength > MAX_PATH_BYTES) snapshotError("invalid_path:too_long");
  if (path.includes(0)) snapshotError("invalid_path:contains_nul");
  if (path[0] === 0x2f) snapshotError("invalid_path:absolute");
  if (path[path.byteLength - 1] === 0x2f) {
    snapshotError("invalid_path:trailing_slash");
  }

  let componentStart = 0;
  for (let index = 0; index <= path.byteLength; index += 1) {
    if (index !== path.byteLength && path[index] !== 0x2f) continue;
    const component = path.subarray(componentStart, index);
    if (component.byteLength === 0) {
      snapshotError("invalid_path:empty_component");
    }
    if (
      (component.byteLength === 1 && component[0] === 0x2e) ||
      (component.byteLength === 2 &&
        component[0] === 0x2e &&
        component[1] === 0x2e)
    ) {
      snapshotError("invalid_path:relative_component");
    }
    if (isGitComponent(component)) {
      snapshotError("invalid_path:git_component");
    }
    componentStart = index + 1;
  }
}

function entryKind(mode: string): EntryKind {
  if (mode === "100644" || mode === "100755" || mode === "120000") {
    return { mode };
  }
  snapshotError("unsupported_mode");
}

function entries(vectors: SnapshotEntryVector[]): SourceEntry[] {
  return vectors.map((vector) => {
    const kind = entryKind(vector.mode);
    const path = decodeHex(vector.path_hex);
    const content = decodeHex(vector.content_hex);
    return {
      path,
      kind,
      contentSha256: createHash("sha256").update(content).digest(),
    };
  });
}

function validateAndSort(inputEntries: SourceEntry[]): SourceEntry[] {
  for (const entry of inputEntries) validatePath(entry.path);
  const sorted = [...inputEntries].sort((left, right) =>
    compareBytes(left.path, right.path),
  );

  for (let index = 1; index < sorted.length; index += 1) {
    if (compareBytes(sorted[index - 1].path, sorted[index].path) === 0) {
      snapshotError("duplicate_path");
    }
  }

  const directories = new Set<string>();
  for (const entry of sorted) {
    for (let index = 0; index < entry.path.byteLength; index += 1) {
      if (entry.path[index] === 0x2f) {
        directories.add(hex(entry.path.subarray(0, index)));
      }
    }
  }
  if (sorted.some((entry) => directories.has(hex(entry.path)))) {
    snapshotError("file_directory_conflict");
  }
  return sorted;
}

function manifestBytes(sortedEntries: SourceEntry[]): Buffer {
  const parts: Buffer[] = [MANIFEST_HEADER];
  for (const entry of sortedEntries) {
    parts.push(
      Buffer.from(
        `${entry.kind.mode} ${entry.contentSha256.toString("hex")} `,
        "ascii",
      ),
      entry.path,
      Buffer.from([0]),
    );
  }
  return Buffer.concat(parts);
}

function snapshotId(manifest: Uint8Array): string {
  return `sha256:${createHash("sha256").update(manifest).digest("hex")}`;
}

export function isCanonicalSnapshotId(value: string): boolean {
  return /^sha256:[0-9a-f]{64}$/.test(value);
}

export function qualifyValidSnapshot(vector: SnapshotVector): {
  manifest: Buffer;
  id: string;
} {
  const sorted = validateAndSort(entries(vector.entries));
  const manifest = manifestBytes(sorted);
  return { manifest, id: snapshotId(manifest) };
}

export function qualifyInvalidSnapshot(vector: SnapshotVector): never {
  validateAndSort(entries(vector.entries));
  throw new Error(`invalid snapshot was accepted: ${vector.name}`);
}

export function qualifyUnsupportedMode(mode: string): never {
  entryKind(mode);
  throw new Error(`unsupported entry mode was accepted: ${mode}`);
}
