import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import {
  canonicalizeValue,
  compareUtf16,
  parseCanonicalJson,
  type CanonicalValue,
} from "./canonical-json.mts";

export type RecordField = {
  name: string;
  required: boolean;
  kind: string;
  values?: string[];
  capability?: string;
};

export type RecordSchema = {
  name: string;
  capabilities: string[];
  fields: RecordField[];
};

export type RecordReading =
  | { kind: "known"; value: string }
  | { kind: "unknown" }
  | { kind: "unrecognized"; value: string };

export type DecodedRecord = {
  schema: RecordSchema;
  object: Map<string, CanonicalValue>;
  canonicalBytes: Buffer;
  id: string;
};

export class RecordProfileError extends Error {
  readonly code: string;

  constructor(code: string) {
    super(code);
    this.name = "RecordProfileError";
    this.code = code;
  }
}

const RECORD_DOMAIN = Buffer.from("aethyme record v0\0", "utf8");
const MAX_DECIMAL_DIGITS = 78;
const UNKNOWN_STATE = "unknown";

function recordError(code: string): never {
  throw new RecordProfileError(code);
}

function isString(value: CanonicalValue | undefined): value is string {
  return typeof value === "string";
}

function stringSet(field: string, value: CanonicalValue): string[] {
  if (!Array.isArray(value)) recordError("wrong_type");
  if (!value.every((item) => typeof item === "string")) {
    recordError("wrong_type");
  }

  const sorted = [...value].sort((left, right) =>
    compareUtf16(left as string, right as string),
  ) as string[];
  for (let index = 1; index < sorted.length; index += 1) {
    if (sorted[index - 1] === sorted[index]) {
      recordError("duplicate_set_member");
    }
  }
  return sorted;
}

function checkField(field: RecordField, value: CanonicalValue): CanonicalValue {
  switch (field.kind) {
    case "boolean":
      if (typeof value !== "boolean") recordError("wrong_type");
      return value;
    case "integer":
      if (typeof value !== "number") recordError("wrong_type");
      return value;
    case "string":
    case "state":
      if (typeof value !== "string") recordError("wrong_type");
      return value;
    case "decimal_string": {
      if (typeof value !== "string") recordError("wrong_type");
      const canonical =
        value.length > 0 &&
        value.length <= MAX_DECIMAL_DIGITS &&
        /^[0-9]+$/.test(value) &&
        (value === "0" || !value.startsWith("0"));
      if (!canonical) recordError("invalid_decimal");
      return value;
    }
    case "string_set":
      return stringSet(field.name, value);
    case "opaque":
      return value;
    default:
      throw new Error(`unknown frozen record field kind: ${field.kind}`);
  }
}

function sha256(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

export function isCanonicalRecordId(value: string): boolean {
  return /^sha256:[0-9a-f]{64}$/.test(value);
}

export function decodeRecord(
  input: Uint8Array,
  schemas: RecordSchema[],
): DecodedRecord {
  const value = parseCanonicalJson(input);
  if (!(value instanceof Map)) recordError("not_an_object");

  const schemaValue = value.get("schema");
  if (schemaValue === undefined) recordError("missing_field");
  if (!isString(schemaValue)) recordError("wrong_type");

  const schema = schemas.find((candidate) => candidate.name === schemaValue);
  if (schema === undefined) recordError("unsupported_schema");

  const normalized = new Map<string, CanonicalValue>();
  for (const [name, memberValue] of value.entries()) {
    let kept: CanonicalValue | undefined;
    if (name === "schema") {
      kept = memberValue;
    } else if (name === "requires") {
      const requiredCapabilities = stringSet(name, memberValue);
      for (const capability of requiredCapabilities) {
        if (!schema.capabilities.includes(capability)) {
          recordError("unsupported_capability");
        }
      }
      if (requiredCapabilities.length > 0) kept = requiredCapabilities;
    } else if (name === "extensions") {
      if (!(memberValue instanceof Map)) recordError("wrong_type");
      kept = memberValue;
    } else {
      const field = schema.fields.find((candidate) => candidate.name === name);
      if (field === undefined) {
        kept = memberValue;
      } else {
        const checked = checkField(field, memberValue);
        const unknownState =
          field.kind === "state" && checked === UNKNOWN_STATE;
        if (!unknownState) kept = checked;
      }
    }
    if (kept !== undefined) normalized.set(name, kept);
  }

  const missing = schema.fields.find(
    (field) =>
      field.required &&
      field.kind !== "state" &&
      !value.has(field.name),
  );
  if (missing !== undefined) recordError("missing_field");

  const declared = value.get("requires");
  const declaredCapabilities = Array.isArray(declared)
    ? declared.filter((item): item is string => typeof item === "string")
    : [];
  const undeclared = schema.fields.find(
    (field) =>
      field.capability !== undefined &&
      !declaredCapabilities.includes(field.capability) &&
      value.has(field.name),
  );
  if (undeclared !== undefined) recordError("undeclared_capability");

  const canonicalBytes = canonicalizeValue(normalized);
  const id = `sha256:${sha256(Buffer.concat([RECORD_DOMAIN, canonicalBytes]))}`;
  return { schema, object: normalized, canonicalBytes, id };
}

export function readState(record: DecodedRecord, fieldName: string): RecordReading {
  const field = record.schema.fields.find(
    (candidate) => candidate.name === fieldName && candidate.kind === "state",
  );
  if (field === undefined) {
    throw new Error(`${fieldName} is not a state field in ${record.schema.name}`);
  }
  const value = record.object.get(fieldName);
  if (value === undefined || value === UNKNOWN_STATE) return { kind: "unknown" };
  if (typeof value !== "string") {
    throw new Error(`${fieldName} was not validated as a string`);
  }
  if (field.values?.includes(value)) return { kind: "known", value };
  return { kind: "unrecognized", value };
}

export function stateReadingText(reading: RecordReading): string {
  switch (reading.kind) {
    case "known":
      return `known:${reading.value}`;
    case "unrecognized":
      return `unrecognized:${reading.value}`;
    case "unknown":
      return "unknown";
  }
}

export function stateMatches(reading: RecordReading, value: string): boolean {
  return reading.kind === "known" && reading.value === value;
}

export function requiredCapabilities(
  schema: RecordSchema,
  memberNames: string[],
): string[] {
  const capabilities = new Set<string>();
  for (const name of memberNames) {
    const field = schema.fields.find((candidate) => candidate.name === name);
    if (field?.capability !== undefined) capabilities.add(field.capability);
  }
  return [...capabilities].sort(compareUtf16);
}
