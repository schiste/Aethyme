import { Buffer } from "node:buffer";
import {
  canonicalizeValue,
  parseCanonicalJson,
  type CanonicalValue,
} from "./canonical-json.mts";
import { decodeRecord, type DecodedRecord, type RecordSchema } from "./records.mts";

export const BRIEF_TOKEN_PROFILE = "aethyme-brief-tokens/v0";
export const BRIEF_SCHEMA_NAME = "aethyme.decision-brief/experimental-v0";
export const BRIEF_LIMITS = {
  max_tokens: 150,
  max_prose_bytes: 2048,
  max_list_entries: 8,
  max_scope_ref_bytes: 64,
  max_record_bytes: 8192,
} as const;

const DECISION_FIELDS = ["scope_ref", "choice", "reason"] as const;
const LIST_FIELDS = ["preserves", "assumptions", "deferred"] as const;
const DECISION_FILE_FIELDS = new Set([
  "intent",
  "decisions",
  ...LIST_FIELDS,
]);
const BIDI_CONTROLS = new Set([
  0x061c,
  0x200e,
  0x200f,
  0x202a,
  0x202b,
  0x202c,
  0x202d,
  0x202e,
  0x2066,
  0x2067,
  0x2068,
  0x2069,
]);

export type BriefDecision = {
  scope_ref: string;
  choice: string;
  reason: string;
};

export type Brief = {
  intent: string;
  decisions: BriefDecision[];
  preserves: string[];
  assumptions: string[];
  deferred: string[];
};

export type BriefProblem = { path: string; code: string };

export class BriefProfileError extends Error {
  readonly code: string;
  readonly problems: BriefProblem[];

  constructor(problems: BriefProblem[]) {
    super(problems.map(({ path, code }) => `${path || "brief"}:${code}`).join(", "));
    this.name = "BriefProfileError";
    this.problems = problems;
    this.code = problems[0]?.code ?? "invalid_brief";
  }
}

export const BRIEF_RECORD_SCHEMA: RecordSchema = {
  name: BRIEF_SCHEMA_NAME,
  capabilities: [],
  fields: [
    { name: "token_profile", required: true, kind: "string" },
    { name: "intent", required: true, kind: "string" },
    { name: "decisions", required: false, kind: "opaque" },
    { name: "preserves", required: false, kind: "opaque" },
    { name: "assumptions", required: false, kind: "opaque" },
    { name: "deferred", required: false, kind: "opaque" },
  ],
};

function problem(path: string, code: string): BriefProblem {
  return { path, code };
}

function isMap(value: CanonicalValue | undefined): value is Map<string, CanonicalValue> {
  return value instanceof Map;
}

function sortedNames(object: Map<string, CanonicalValue>): string[] {
  return [...object.keys()].sort((left, right) =>
    left < right ? -1 : left > right ? 1 : 0,
  );
}

function parseObject(input: Uint8Array): Map<string, CanonicalValue> {
  let value: CanonicalValue;
  try {
    value = parseCanonicalJson(input);
  } catch (error) {
    const code =
      typeof error === "object" &&
      error !== null &&
      "code" in error &&
      typeof error.code === "string"
        ? error.code
        : "syntax";
    throw new BriefProfileError([problem("", code)]);
  }
  if (!isMap(value)) throw new BriefProfileError([problem("", "not_an_object")]);
  return value;
}

function readText(
  object: Map<string, CanonicalValue>,
  name: string,
  path: string,
  problems: BriefProblem[],
): string {
  const value = object.get(name);
  if (value === undefined) {
    problems.push(problem(path, "missing_field"));
    return "";
  }
  if (typeof value !== "string") {
    problems.push(problem(path, "wrong_type"));
    return "";
  }
  return value;
}

function readList(
  object: Map<string, CanonicalValue>,
  name: string,
  inRecord: boolean,
  problems: BriefProblem[],
): CanonicalValue[] | undefined {
  const value = object.get(name);
  if (value === undefined) return undefined;
  if (!Array.isArray(value)) {
    problems.push(problem(name, "wrong_type"));
    return undefined;
  }
  if (inRecord && value.length === 0) {
    problems.push(problem(name, "empty"));
    return undefined;
  }
  return value;
}

function readFields(
  object: Map<string, CanonicalValue>,
  inRecord: boolean,
  problems: BriefProblem[],
): Brief {
  const brief: Brief = {
    intent: readText(object, "intent", "intent", problems),
    decisions: [],
    preserves: [],
    assumptions: [],
    deferred: [],
  };

  const decisionItems = readList(object, "decisions", inRecord, problems);
  if (decisionItems !== undefined) {
    for (const [index, item] of decisionItems.entries()) {
      const path = `decisions[${index}]`;
      if (!isMap(item)) {
        problems.push(problem(path, "wrong_type"));
        continue;
      }

      for (const name of sortedNames(item)) {
        if (!(DECISION_FIELDS as readonly string[]).includes(name)) {
          problems.push(problem(`${path}.${name}`, "unknown_field"));
        }
      }

      brief.decisions.push({
        scope_ref: readText(item, "scope_ref", `${path}.scope_ref`, problems),
        choice: readText(item, "choice", `${path}.choice`, problems),
        reason: readText(item, "reason", `${path}.reason`, problems),
      });
    }
  }

  for (const name of LIST_FIELDS) {
    const items = readList(object, name, inRecord, problems);
    if (items === undefined) continue;
    const texts: string[] = [];
    for (const [index, item] of items.entries()) {
      if (typeof item === "string") {
        texts.push(item);
      } else {
        problems.push(problem(`${name}[${index}]`, "wrong_type"));
      }
    }
    brief[name] = texts;
  }

  return brief;
}

function isForbidden(codePoint: number): boolean {
  return (
    (codePoint >= 0x00 && codePoint <= 0x09) ||
    (codePoint >= 0x0b && codePoint <= 0x1f) ||
    (codePoint >= 0x7f && codePoint <= 0x9f) ||
    BIDI_CONTROLS.has(codePoint)
  );
}

function checkProse(path: string, value: string, problems: BriefProblem[]): void {
  if ([...value].every((character) => " \t\n\r".includes(character))) {
    problems.push(problem(path, "empty"));
    return;
  }
  for (const character of value) {
    if (isForbidden(character.codePointAt(0) ?? 0)) {
      problems.push(problem(path, "forbidden_character"));
      return;
    }
  }
}

function isScopeRef(value: string): boolean {
  const bytes = Buffer.byteLength(value, "utf8");
  if (bytes === 0 || bytes > BRIEF_LIMITS.max_scope_ref_bytes) return false;
  if (!/^[a-z0-9]$/.test(value[0] ?? "")) return false;
  for (const character of value) {
    if (!/[a-z0-9._/-]/.test(character)) return false;
  }
  return true;
}

export function countBriefTokens(text: string): number {
  type Run = "letter" | "digit" | "two_byte";
  let total = 0;
  let run: Run | undefined;
  let length = 0;

  const flush = (): void => {
    if (run !== undefined) total += Math.ceil(length / (run === "letter" ? 4 : 2));
    run = undefined;
    length = 0;
  };

  for (const character of text) {
    const codePoint = character.codePointAt(0) ?? 0;
    const next: Run | undefined =
      (codePoint >= 0x41 && codePoint <= 0x5a) ||
      (codePoint >= 0x61 && codePoint <= 0x7a)
        ? "letter"
        : codePoint >= 0x30 && codePoint <= 0x39
          ? "digit"
          : codePoint >= 0x80 && codePoint <= 0x7ff
            ? "two_byte"
            : undefined;
    if (next === undefined) {
      flush();
      if (
        codePoint !== 0x20 &&
        codePoint !== 0x09 &&
        codePoint !== 0x0a &&
        codePoint !== 0x0d
      ) {
        total += 1;
      }
    } else if (next !== run) {
      flush();
      run = next;
      length = 1;
    } else {
      length += 1;
    }
  }
  flush();
  return total;
}

function prose(brief: Brief): string[] {
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

function validateContent(brief: Brief, problems: BriefProblem[]): void {
  checkProse("intent", brief.intent, problems);
  checkEntries("decisions", brief.decisions.length, problems);
  for (const [index, decision] of brief.decisions.entries()) {
    const path = `decisions[${index}]`;
    if (!isScopeRef(decision.scope_ref)) {
      problems.push(problem(`${path}.scope_ref`, "invalid_scope_ref"));
    }
    checkProse(`${path}.choice`, decision.choice, problems);
    checkProse(`${path}.reason`, decision.reason, problems);
  }
  for (const name of LIST_FIELDS) {
    const values = brief[name];
    checkEntries(name, values.length, problems);
    for (const [index, value] of values.entries()) {
      checkProse(`${name}[${index}]`, value, problems);
    }
  }

  const values = prose(brief);
  const bytes = values.reduce(
    (total, value) => total + Buffer.byteLength(value, "utf8"),
    0,
  );
  if (bytes > BRIEF_LIMITS.max_prose_bytes) {
    problems.push(problem("", "over_byte_budget"));
  }
  const tokens = values.reduce((total, value) => total + countBriefTokens(value), 0);
  if (tokens > BRIEF_LIMITS.max_tokens) {
    problems.push(problem("", "over_token_budget"));
  }
}

function checkEntries(path: string, count: number, problems: BriefProblem[]): void {
  if (count > BRIEF_LIMITS.max_list_entries) {
    problems.push(problem(path, "too_many_entries"));
  }
}

function finish(brief: Brief, problems: BriefProblem[]): Brief {
  if (problems.length === 0) validateContent(brief, problems);
  if (problems.length > 0) throw new BriefProfileError(problems);
  return brief;
}

export function validateBriefDecisionFile(input: Uint8Array): Brief {
  const object = parseObject(input);
  const problems: BriefProblem[] = [];
  for (const name of sortedNames(object)) {
    if (!DECISION_FILE_FIELDS.has(name)) problems.push(problem(name, "unknown_field"));
  }
  return finish(readFields(object, false, problems), problems);
}

function decodeBriefRecord(input: Uint8Array): DecodedRecord {
  try {
    return decodeRecord(input, [BRIEF_RECORD_SCHEMA]);
  } catch (error) {
    const code =
      typeof error === "object" &&
      error !== null &&
      "code" in error &&
      typeof error.code === "string"
        ? error.code
        : "invalid_record";
    throw new BriefProfileError([problem("", code)]);
  }
}

export function makeBriefRecord(brief: Brief): DecodedRecord {
  const members = new Map<string, CanonicalValue>([
    ["schema", BRIEF_SCHEMA_NAME],
    ["token_profile", BRIEF_TOKEN_PROFILE],
    ["intent", brief.intent],
  ]);
  if (brief.decisions.length > 0) {
    members.set(
      "decisions",
      brief.decisions.map(
        (decision) =>
          new Map<string, CanonicalValue>([
            ["scope_ref", decision.scope_ref],
            ["choice", decision.choice],
            ["reason", decision.reason],
          ]),
      ),
    );
  }
  for (const name of LIST_FIELDS) {
    if (brief[name].length > 0) members.set(name, brief[name]);
  }
  return decodeBriefRecord(canonicalizeValue(members));
}

export function validateBriefRecord(input: Uint8Array): {
  brief: Brief;
  record: DecodedRecord;
} {
  if (input.byteLength > BRIEF_LIMITS.max_record_bytes) {
    throw new BriefProfileError([problem("", "record_too_large")]);
  }
  const record = decodeBriefRecord(input);
  const profile = record.object.get("token_profile");
  if (profile !== BRIEF_TOKEN_PROFILE) {
    throw new BriefProfileError([problem("token_profile", "unsupported_profile")]);
  }
  const problems: BriefProblem[] = [];
  const brief = finish(readFields(record.object, true, problems), problems);
  return { brief, record };
}
