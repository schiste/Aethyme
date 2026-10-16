import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  canonicalizeValue,
  parseCanonicalJson,
  PROFILE_LIMITS,
} from "./canonical-json.mts";
import {
  decodeRecord,
  isCanonicalRecordId,
  readState,
  requiredCapabilities,
  stateMatches,
  stateReadingText,
  type RecordSchema,
} from "./records.mts";
import {
  isCanonicalSnapshotId,
  qualifyInvalidSnapshot,
  qualifyUnsupportedMode,
  qualifyValidSnapshot,
  type SnapshotVectors,
} from "./source-snapshot.mts";
import {
  BRIEF_LIMITS,
  BRIEF_SCHEMA_NAME,
  BRIEF_TOKEN_PROFILE,
  BriefProfileError,
  countBriefTokens,
  makeBriefRecord,
  validateBriefDecisionFile,
  validateBriefRecord,
  type Brief,
} from "./brief-profile.mts";

type CanonicalInput =
  | { hex: string }
  | { repeat: [string, string, number, string] };

type CanonicalOutput =
  | { hex: string }
  | { sha256: string };

type CanonicalCase = {
  name: string;
  input: CanonicalInput;
  canonical: CanonicalOutput;
};

type InvalidCanonicalCase = {
  name: string;
  input: CanonicalInput;
  error: string;
};

type CanonicalVectors = {
  limits: {
    max_document_bytes: number;
    max_depth: number;
    max_string_bytes: number;
    max_container_entries: number;
    max_safe_integer: number;
  };
  valid: CanonicalCase[];
  invalid: InvalidCanonicalCase[];
};

type RecordValidCase = {
  name: string;
  reader: string;
  input: { hex: string };
  canonical_hex: string;
  id: string;
  states: Record<string, string>;
  equivalence_class: string;
};

type RecordInvalidCase = {
  name: string;
  reader: string;
  input: { hex: string };
  error: string;
};

type RecordVectors = {
  domain_hex: string;
  readers: Record<string, RecordSchema[]>;
  valid: RecordValidCase[];
  invalid: RecordInvalidCase[];
};

type BriefTokenVectors = {
  profile: string;
  cases: Array<{ text: string; tokens: number }>;
};

type BriefVectors = {
  profile: string;
  schema: string;
  limits: {
    max_tokens: number;
    max_prose_bytes: number;
    max_list_entries: number;
    max_scope_ref_bytes: number;
    max_record_bytes: number;
  };
  valid: Array<{
    name: string;
    input: { hex: string };
    tokens: number;
    record_hex: string;
    id: string;
    equivalence_class: string;
  }>;
  invalid_decision_files: Array<{
    name: string;
    input: { hex: string };
    errors: Array<[string, string]>;
  }>;
  valid_records: Array<{
    name: string;
    input: { hex: string };
    id: string;
  }>;
  invalid_records: Array<{
    name: string;
    input: { hex: string };
    errors: Array<[string, string]>;
  }>;
};

type BriefCalibrationVectors = {
  reference: string;
  cases: Array<{ text: string; reference_tokens: number }>;
};

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "../../../../");
const REFERENCE_COMMIT = "025103777c16607639073cbf9024d2ad0c39f72d";
const TYPESCRIPT_SOURCES = [
  "canonical-json.mts",
  "records.mts",
  "source-snapshot.mts",
  "brief-profile.mts",
  "t01-canonical-parity.mts",
] as const;
const FROZEN_SOURCES = [
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/fixtures/experimental-v0/canonical_json.json",
    sha256: "23aeb5caff95304ae2047e1b13eca3825cb8cebb46e1e56676db2b4ef7001450",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/fixtures/experimental-v0/records.json",
    sha256: "2c6e2a21c1cd7e3fad9f670152bf04ae474fbfb87e12c74901088e9f96c3176b",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/fixtures/experimental-v0/source_snapshot.json",
    sha256: "9d5f05a3525e9000875e3f0fc6c3ad41802a23c9336e6f82f6e9e781ef622840",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/tests/canonical_json_vectors.rs",
    sha256: "403d6afa08ac94b71b1386bc20a50b401e513cb9765f2d60007c3ff7e8bba96e",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/tests/record_vectors.rs",
    sha256: "563bd824740be45f994d50999da3f1a6dccb50b3f652378a9c0fd94533207c61",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/tests/source_snapshot_vectors.rs",
    sha256: "881fb26bddb06042605b922dd706430502de4e4354ab6cfa9eefe53150623a40",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/src/experimental_v0/canonical_json.rs",
    sha256: "90070c56f613bb94564072c56076daf2428f2888f0b71e34a526b718c55b52f3",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/src/experimental_v0/record.rs",
    sha256: "d4888555e1653ea8897f383dfb6f6f9c50e5e2abd19ee113eb440ee575ad4400",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/src/experimental_v0/source_snapshot.rs",
    sha256: "d36bcd57d0f2ccc47aff1a4c69d5ee974d09a22879cfc0ce938687761737ed14",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/fixtures/experimental-v0/brief_tokens.json",
    sha256: "cff420ec39830255df6b3f408eae566bbccbb7a7759cffc877c797a0cee1ffd1",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/fixtures/experimental-v0/briefs.json",
    sha256: "08aec8cb3a1722a5c41c0ff2c5efc359e8cc084130be9b521564e3edd45369fd",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/fixtures/experimental-v0/brief_calibration.json",
    sha256: "d4f62dfbe73d2906fe766d70bb240d16717d1eaf53b7bddbe6bd55de5cc7d695",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/tests/brief_vectors.rs",
    sha256: "22540d74de0440ddfc1faa6e3b7479e49c00fb8a13ff05e300667052edf803e7",
  },
  {
    path: "packages/aethyme/rust/crates/aethyme-contracts/src/experimental_v0/brief.rs",
    sha256: "3a903fbadb0775a0bc9ac669f2e8b8b37fb6c54ca4f0687571ae315474792a17",
  },
] as const;

type FrozenSourcePin = { path: string; sha256: string };

const LOCAL_STATE26_PROFILE = {
  name: "local-state26-e6fb95",
  referenceCommit: "e6fb95f65f3bf91d46c1ab76136486f492529ebb",
  canonicalJsonPath:
    "packages/aethyme/rust/crates/aethyme-contracts/src/experimental_v0/canonical_json.rs",
  previousCanonicalJsonSha256:
    "90070c56f613bb94564072c56076daf2428f2888f0b71e34a526b718c55b52f3",
  canonicalJsonSha256:
    "6bbeea89e7d5850651687382255619b2ea9d30538341ebbba51a53a0605a6a35",
} as const;

const LOCAL_STATE26_SOURCES = (() => {
  const canonicalJsonPins = FROZEN_SOURCES.filter(
    (source) => source.path === LOCAL_STATE26_PROFILE.canonicalJsonPath,
  );
  if (
    canonicalJsonPins.length !== 1 ||
    canonicalJsonPins[0].sha256 !==
      LOCAL_STATE26_PROFILE.previousCanonicalJsonSha256
  ) {
    throw new Error("T01 source profile pin setup is inconsistent");
  }

  return FROZEN_SOURCES.map((source) =>
    source.path === LOCAL_STATE26_PROFILE.canonicalJsonPath
      ? { ...source, sha256: LOCAL_STATE26_PROFILE.canonicalJsonSha256 }
      : source,
  );
})();

type T01SourceSelection = {
  sourceProfile?: typeof LOCAL_STATE26_PROFILE.name;
  referenceCommit: string;
  sources: readonly FrozenSourcePin[];
};

function selectSourceProfile(args: readonly string[]): T01SourceSelection {
  if (args.length === 0) {
    return { referenceCommit: REFERENCE_COMMIT, sources: FROZEN_SOURCES };
  }

  if (args.length === 2 && args[0] === "--source-profile") {
    if (args[1] !== LOCAL_STATE26_PROFILE.name) {
      throw new Error(`Error: unsupported T01 source profile: ${args[1]}`);
    }
    return {
      sourceProfile: LOCAL_STATE26_PROFILE.name,
      referenceCommit: LOCAL_STATE26_PROFILE.referenceCommit,
      sources: LOCAL_STATE26_SOURCES,
    };
  }

  throw new Error(
    `Error: unsupported T01 command-line arguments: ${args.join(" ")}`,
  );
}

const BRIEF_TOKEN_VECTORS_PATH =
  "packages/aethyme/rust/crates/aethyme-contracts/fixtures/experimental-v0/brief_tokens.json";
const BRIEF_VECTORS_PATH =
  "packages/aethyme/rust/crates/aethyme-contracts/fixtures/experimental-v0/briefs.json";
const BRIEF_CALIBRATION_PATH =
  "packages/aethyme/rust/crates/aethyme-contracts/fixtures/experimental-v0/brief_calibration.json";

let assertionCount = 0;

function check(condition: boolean, message: string): asserts condition {
  assertionCount += 1;
  if (!condition) throw new Error(message);
}

function sha256(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

function decodeHex(encoded: string): Buffer {
  check(
    encoded.length % 2 === 0 && /^[0-9a-f]*$/.test(encoded),
    "frozen fixture contains non-canonical lowercase hex",
  );
  return Buffer.from(encoded, "hex");
}

function bytesForInput(input: CanonicalInput): Buffer {
  if ("hex" in input) return decodeHex(input.hex);

  const [prefixHex, unitHex, count, suffixHex] = input.repeat;
  check(Number.isSafeInteger(count) && count >= 0, "invalid repeat count");
  const prefix = decodeHex(prefixHex);
  const unit = decodeHex(unitHex);
  const suffix = decodeHex(suffixHex);
  const output = Buffer.alloc(
    prefix.byteLength + unit.byteLength * count + suffix.byteLength,
  );
  let offset = 0;
  prefix.copy(output, offset);
  offset += prefix.byteLength;
  for (let index = 0; index < count; index += 1) {
    unit.copy(output, offset);
    offset += unit.byteLength;
  }
  suffix.copy(output, offset);
  return output;
}

function codeFrom(error: unknown): string | undefined {
  if (
    typeof error === "object" &&
    error !== null &&
    "code" in error &&
    typeof error.code === "string"
  ) {
    return error.code;
  }
  return undefined;
}

function refusalCode(operation: () => unknown): string | undefined {
  try {
    operation();
    return undefined;
  } catch (error) {
    return codeFrom(error);
  }
}

function readFrozenSources(sourcePins: readonly FrozenSourcePin[]): {
  canonical: CanonicalVectors;
  records: RecordVectors;
  snapshots: SnapshotVectors;
  briefTokens: BriefTokenVectors;
  briefs: BriefVectors;
  calibration: BriefCalibrationVectors;
  hashes: Array<{ path: string; sha256: string }>;
} {
  const hashes: Array<{ path: string; sha256: string }> = [];
  const contents = new Map<string, Buffer>();

  for (const source of sourcePins) {
    const content = readFileSync(resolve(REPO_ROOT, source.path));
    const actual = sha256(content);
    check(
      actual === source.sha256,
      `frozen source drift at ${source.path}: expected ${source.sha256}, found ${actual}`,
    );
    hashes.push({ path: source.path, sha256: actual });
    contents.set(source.path, content);
  }

  const json = (path: string): unknown => {
    const content = contents.get(path);
    if (content === undefined) throw new Error(`source not pinned: ${path}`);
    return JSON.parse(content.toString("utf8")) as unknown;
  };

  return {
    canonical: json(sourcePins[0].path) as CanonicalVectors,
    records: json(sourcePins[1].path) as RecordVectors,
    snapshots: json(sourcePins[2].path) as SnapshotVectors,
    briefTokens: json(BRIEF_TOKEN_VECTORS_PATH) as BriefTokenVectors,
    briefs: json(BRIEF_VECTORS_PATH) as BriefVectors,
    calibration: json(BRIEF_CALIBRATION_PATH) as BriefCalibrationVectors,
    hashes,
  };
}

function qualifyCanonicalVectors(vectors: CanonicalVectors): {
  valid: number;
  invalid: number;
} {
  check(
    vectors.limits.max_document_bytes === PROFILE_LIMITS.max_document_bytes &&
      vectors.limits.max_depth === PROFILE_LIMITS.max_depth &&
      vectors.limits.max_string_bytes === PROFILE_LIMITS.max_string_bytes &&
      vectors.limits.max_container_entries === PROFILE_LIMITS.max_container_entries &&
      vectors.limits.max_safe_integer === PROFILE_LIMITS.max_safe_integer,
    "TypeScript canonical limits differ from the frozen Rust profile",
  );

  for (const vector of vectors.valid) {
    const input = bytesForInput(vector.input);
    const canonical = canonicalizeValue(parseCanonicalJson(input));
    if ("hex" in vector.canonical) {
      check(
        canonical.toString("hex") === vector.canonical.hex,
        `${vector.name}: canonical bytes differ`,
      );
    } else {
      check(
        sha256(canonical) === vector.canonical.sha256,
        `${vector.name}: canonical SHA-256 differs`,
      );
    }
    const fixedPoint = canonicalizeValue(parseCanonicalJson(canonical));
    check(
      fixedPoint.equals(canonical),
      `${vector.name}: canonical output is not a fixed point`,
    );
  }

  for (const vector of vectors.invalid) {
    const actual = refusalCode(() =>
      parseCanonicalJson(bytesForInput(vector.input)),
    );
    check(
      actual === vector.error,
      `${vector.name}: expected ${vector.error}, received ${actual ?? "accept"}`,
    );
  }

  const requiredInvalid = [
    "duplicate key",
    "duplicate key after unescaping",
    "2^53",
    "-2^53",
    "beyond i64",
    "null member",
  ];
  for (const name of requiredInvalid) {
    check(
      vectors.invalid.some((vector) => vector.name === name),
      `T01 critical canonical rejection vector missing: ${name}`,
    );
  }
  check(
    vectors.valid.some((vector) => vector.name === "safe integer bounds"),
    "T01 safe integer positive boundary vector missing",
  );

  return { valid: vectors.valid.length, invalid: vectors.invalid.length };
}

function qualifyRecordVectors(vectors: RecordVectors): {
  valid: number;
  invalid: number;
} {
  check(
    decodeHex(vectors.domain_hex).equals(Buffer.from("aethyme record v0\0")),
    "record domain header differs from the frozen Rust profile",
  );

  const idForClass = new Map<string, string>();
  const classForId = new Map<string, string>();

  for (const vector of vectors.valid) {
    const schemas = vectors.readers[vector.reader];
    check(schemas !== undefined, `${vector.name}: missing reader schema`);
    const record = decodeRecord(decodeHex(vector.input.hex), schemas);
    check(
      record.canonicalBytes.toString("hex") === vector.canonical_hex,
      `${vector.name}: canonical record bytes differ`,
    );
    check(record.id === vector.id, `${vector.name}: record digest differs`);
    check(isCanonicalRecordId(vector.id), `${vector.name}: malformed frozen ID`);

    for (const [field, expected] of Object.entries(vector.states)) {
      const actual = stateReadingText(readState(record, field));
      check(
        actual === expected,
        `${vector.name}: state ${field} expected ${expected}, received ${actual}`,
      );
    }

    const priorId = idForClass.get(vector.equivalence_class);
    check(
      priorId === undefined || priorId === vector.id,
      `${vector.name}: equivalent record encodings have different IDs`,
    );
    idForClass.set(vector.equivalence_class, vector.id);

    const priorClass = classForId.get(vector.id);
    check(
      priorClass === undefined || priorClass === vector.equivalence_class,
      `${vector.name}: distinct record classes share an ID`,
    );
    classForId.set(vector.id, vector.equivalence_class);

    for (const field of Object.keys(vector.states)) {
      const reading = readState(record, field);
      if (reading.kind !== "known") {
        for (const positive of [
          "accepted",
          "captured",
          "shared",
          "superseded",
          "unknown",
        ]) {
          check(
            !stateMatches(reading, positive),
            `${vector.name}: non-known ${field} matched ${positive}`,
          );
        }
      }
    }

    const uppercaseHex = `sha256:${vector.id.slice(7).toUpperCase()}`;
    check(!isCanonicalRecordId(uppercaseHex), "uppercase record ID was accepted");
  }

  for (const vector of vectors.invalid) {
    const schemas = vectors.readers[vector.reader];
    check(schemas !== undefined, `${vector.name}: missing reader schema`);
    const actual = refusalCode(() =>
      decodeRecord(decodeHex(vector.input.hex), schemas),
    );
    check(
      actual === vector.error,
      `${vector.name}: expected ${vector.error}, received ${actual ?? "accept"}`,
    );
  }

  const newReader = vectors.readers.new?.[0];
  check(newReader !== undefined, "new reader schema is missing");
  check(
    requiredCapabilities(newReader, ["schema", "subject", "sharing", "note"]).join(
      ",",
    ) === "retention-receipt",
    "record gated field capability calculation differs",
  );
  check(
    requiredCapabilities(newReader, ["subject", "note"]).length === 0,
    "ungated record fields added a required capability",
  );

  for (const vector of vectors.valid.filter((candidate) => candidate.reader === "new")) {
    const input = parseCanonicalJson(decodeHex(vector.input.hex));
    check(input instanceof Map, `${vector.name}: expected record object`);
    const declaredValue = input.get("requires");
    const declared = Array.isArray(declaredValue)
      ? declaredValue.filter((value): value is string => typeof value === "string")
      : [];
    const present = ["sharing", "note", "subject"].filter((field) =>
      input.has(field),
    );
    for (const capability of requiredCapabilities(newReader, present)) {
      check(
        declared.includes(capability),
        `${vector.name}: missing required capability ${capability}`,
      );
    }
  }

  for (const vector of [
    "explicit unknown equals absent",
    "same record: other order, whitespace, set order, escapes",
    "set order differs only",
    "78-digit decimal",
  ]) {
    check(
      vectors.valid.some((candidate) => candidate.name === vector),
      `T01 critical record vector missing: ${vector}`,
    );
  }
  for (const vector of [
    "set repeats a member",
    "decimal as a JSON integer",
    "profile violation passes through",
  ]) {
    check(
      vectors.invalid.some((candidate) => candidate.name === vector),
      `T01 critical record rejection vector missing: ${vector}`,
    );
  }

  return { valid: vectors.valid.length, invalid: vectors.invalid.length };
}

function qualifySnapshotVectors(vectors: SnapshotVectors): {
  valid: number;
  invalid: number;
  unsupportedModes: number;
} {
  const ids = new Set<string>();

  for (const vector of vectors.valid) {
    const actual = qualifyValidSnapshot(vector);
    check(
      actual.manifest.toString("hex") === vector.manifest_hex,
      `${vector.name}: source snapshot manifest differs`,
    );
    check(actual.id === vector.id, `${vector.name}: source snapshot ID differs`);
    check(isCanonicalSnapshotId(actual.id), `${vector.name}: malformed snapshot ID`);

    const reversed = qualifyValidSnapshot({
      ...vector,
      entries: [...vector.entries].reverse(),
    });
    check(
      reversed.manifest.equals(actual.manifest) && reversed.id === actual.id,
      `${vector.name}: input order changed snapshot identity`,
    );
    ids.add(actual.id);
  }

  check(
    ids.size === vectors.valid.length,
    "distinct frozen snapshots unexpectedly share an ID",
  );

  for (const vector of vectors.invalid) {
    const actual = refusalCode(() => qualifyInvalidSnapshot(vector));
    check(
      actual === vector.error,
      `${vector.name}: expected ${vector.error}, received ${actual ?? "accept"}`,
    );
  }

  for (const mode of vectors.unsupported_modes) {
    check(
      refusalCode(() => qualifyUnsupportedMode(mode)) === "unsupported_mode",
      `unsupported mode was not refused: ${mode}`,
    );
  }

  for (const encoded of vectors.malformed_ids) {
    check(
      !isCanonicalSnapshotId(encoded),
      `malformed source snapshot ID was accepted: ${encoded}`,
    );
  }
  for (const vector of [
    "non-UTF-8 path bytes",
    "NFC path",
    "NFD path",
    "raw byte order: '-' < '.' < '/'",
  ]) {
    check(
      vectors.valid.some((candidate) => candidate.name === vector),
      `source snapshot ordering/byte vector missing: ${vector}`,
    );
  }

  return {
    valid: vectors.valid.length,
    invalid: vectors.invalid.length,
    unsupportedModes: vectors.unsupported_modes.length,
  };
}


function briefProblems(operation: () => unknown): Array<{ path: string; code: string }> | undefined {
  try {
    operation();
    return undefined;
  } catch (error) {
    if (error instanceof BriefProfileError) return error.problems;
    const code = codeFrom(error);
    return [{ path: "", code: code ?? "unclassified_exception" }];
  }
}

function qualifyBriefVectors(
  tokenVectors: BriefTokenVectors,
  vectors: BriefVectors,
  calibration: BriefCalibrationVectors,
): {
  tokenCases: number;
  calibrationCases: number;
  validDecisionFiles: number;
  invalidDecisionFiles: number;
  validRecords: number;
  invalidRecords: number;
} {
  check(tokenVectors.profile === BRIEF_TOKEN_PROFILE, "token fixture profile differs");
  check(vectors.profile === BRIEF_TOKEN_PROFILE, "brief fixture profile differs");
  check(vectors.schema === BRIEF_SCHEMA_NAME, "brief fixture schema differs");
  check(calibration.reference === "o200k_base", "calibration reference changed");
  check(
    vectors.limits.max_tokens === BRIEF_LIMITS.max_tokens &&
      vectors.limits.max_prose_bytes === BRIEF_LIMITS.max_prose_bytes &&
      vectors.limits.max_list_entries === BRIEF_LIMITS.max_list_entries &&
      vectors.limits.max_scope_ref_bytes === BRIEF_LIMITS.max_scope_ref_bytes &&
      vectors.limits.max_record_bytes === BRIEF_LIMITS.max_record_bytes,
    "TypeScript brief limits differ from the frozen profile",
  );

  for (const vector of tokenVectors.cases) {
    check(
      countBriefTokens(vector.text) === vector.tokens,
      "brief token count differs for " + JSON.stringify(vector.text),
    );
  }
  for (const vector of calibration.cases) {
    check(
      countBriefTokens(vector.text) >= vector.reference_tokens,
      "brief profile is below its frozen calibration count for " +
        JSON.stringify(vector.text),
    );
  }

  const idForClass = new Map<string, string>();
  const classForId = new Map<string, string>();
  for (const vector of vectors.valid) {
    const input = decodeHex(vector.input.hex);
    const brief = validateBriefDecisionFile(input);
    const count = briefText(brief).reduce(
      (total, value) => total + countBriefTokens(value),
      0,
    );
    check(count === vector.tokens, vector.name + ": token count differs");
    const record = makeBriefRecord(brief);
    check(
      record.canonicalBytes.toString("hex") === vector.record_hex,
      vector.name + ": canonical brief record differs",
    );
    check(record.id === vector.id, vector.name + ": brief record ID differs");

    const readBack = validateBriefRecord(record.canonicalBytes);
    check(
      JSON.stringify(readBack.brief) === JSON.stringify(brief),
      vector.name + ": brief record round trip differs",
    );
    check(readBack.record.id === vector.id, vector.name + ": round-trip ID differs");

    const priorId = idForClass.get(vector.equivalence_class);
    check(
      priorId === undefined || priorId === vector.id,
      vector.name + ": equivalent briefs have different IDs",
    );
    idForClass.set(vector.equivalence_class, vector.id);
    const priorClass = classForId.get(vector.id);
    check(
      priorClass === undefined || priorClass === vector.equivalence_class,
      vector.name + ": distinct brief classes share an ID",
    );
    classForId.set(vector.id, vector.equivalence_class);
  }

  const compareProblems = (
    name: string,
    operation: () => unknown,
    expected: Array<[string, string]>,
  ): void => {
    const actual = briefProblems(operation);
    check(actual !== undefined, name + ": invalid brief was accepted");
    const actualPairs = actual.map((item) => [item.path, item.code]);
    check(
      JSON.stringify(actualPairs) === JSON.stringify(expected),
      name + ": expected " + JSON.stringify(expected) + ", received " +
        JSON.stringify(actualPairs),
    );
  };

  for (const vector of vectors.invalid_decision_files) {
    compareProblems(
      vector.name,
      () => validateBriefDecisionFile(decodeHex(vector.input.hex)),
      vector.errors,
    );
  }

  for (const vector of vectors.valid_records) {
    const result = validateBriefRecord(decodeHex(vector.input.hex));
    check(result.record.id === vector.id, vector.name + ": carried record ID differs");
  }
  for (const vector of vectors.invalid_records) {
    compareProblems(
      vector.name,
      () => validateBriefRecord(decodeHex(vector.input.hex)),
      vector.errors,
    );
  }

  const validNames = new Set(vectors.valid.map((vector) => vector.name));
  const invalidDecisionNames = new Set(
    vectors.invalid_decision_files.map((vector) => vector.name),
  );
  const invalidRecordNames = new Set(vectors.invalid_records.map((vector) => vector.name));
  for (const name of [
    "149 tokens",
    "150 tokens",
    "multilingual",
    "same brief, other key order and escapes",
  ]) {
    check(validNames.has(name), "T02 critical valid brief vector missing: " + name);
  }
  for (const name of ["151 tokens", "all shape problems reported together"]) {
    check(
      invalidDecisionNames.has(name),
      "T02 critical invalid decision-file vector missing: " + name,
    );
  }
  for (const name of [
    "other token profile",
    "hidden metadata makes the record too large",
    "record over the token budget",
  ]) {
    check(
      invalidRecordNames.has(name),
      "T02 critical invalid record vector missing: " + name,
    );
  }
  check(
    vectors.valid_records.some((vector) =>
      vector.name.includes("newer writer carries an unknown member"),
    ),
    "T02 unknown future metadata vector missing",
  );
  check(
    tokenVectors.cases.some((vector) => vector.text.includes("🚀")) &&
      tokenVectors.cases.some((vector) => vector.text.includes("保持")) &&
      tokenVectors.cases.some((vector) => vector.text.includes("שלום")),
    "T02 multilingual token vectors missing",
  );
  check(
    tokenVectors.cases.length === 30 && calibration.cases.length === 27,
    "T02 token or calibration vector count differs from the frozen corpus",
  );

  return {
    tokenCases: tokenVectors.cases.length,
    calibrationCases: calibration.cases.length,
    validDecisionFiles: vectors.valid.length,
    invalidDecisionFiles: vectors.invalid_decision_files.length,
    validRecords: vectors.valid_records.length,
    invalidRecords: vectors.invalid_records.length,
  };
}

function briefText(brief: Brief): string[] {
  return [
    brief.intent,
    ...brief.decisions.flatMap((decision) => [
      decision.scope_ref,
      decision.choice,
      decision.reason,
    ]),
    ...brief.preserves,
    ...brief.assumptions,
    ...brief.deferred,
  ];
}

function currentTypeScriptSources(): Array<{ path: string; sha256: string }> {
  return TYPESCRIPT_SOURCES.map((path) => ({
    path: "packages/aethyme-eval/local-v3/t01-canonical-parity/" + path,
    sha256: sha256(readFileSync(resolve(HERE, path))),
  }));
}

function run(args: readonly string[] = process.argv.slice(2)): void {
  let selected: T01SourceSelection;
  try {
    selected = selectSourceProfile(args);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 2;
    return;
  }

  const frozen = readFrozenSources(selected.sources);
  const canonical = qualifyCanonicalVectors(frozen.canonical);
  const records = qualifyRecordVectors(frozen.records);
  const snapshots = qualifySnapshotVectors(frozen.snapshots);
  const briefs = qualifyBriefVectors(
    frozen.briefTokens,
    frozen.briefs,
    frozen.calibration,
  );

  console.log(
    JSON.stringify(
      {
        schema: "aethyme.local-v3.t01-t02-typescript-parity.v1",
        status: "pass",
        runtime: process.version,
        ...(selected.sourceProfile === undefined
          ? {}
          : { sourceProfile: selected.sourceProfile }),
        referenceCommit: selected.referenceCommit,
        assertions: assertionCount,
        vectors: { canonicalJson: canonical, records, sourceSnapshots: snapshots, briefs },
        frozenSources: frozen.hashes,
        typescriptSources: currentTypeScriptSources(),
      },
      null,
      2,
    ),
  );
}

run();
