import { Buffer } from "node:buffer";

export type CanonicalValue =
  | boolean
  | number
  | string
  | CanonicalValue[]
  | Map<string, CanonicalValue>;

export type CanonicalErrorCode =
  | "too_large"
  | "byte_order_mark"
  | "invalid_utf8"
  | "syntax"
  | "trailing_data"
  | "too_deep"
  | "too_many_entries"
  | "duplicate_key"
  | "null_value"
  | "non_integer_number"
  | "integer_out_of_range"
  | "negative_zero"
  | "lone_surrogate"
  | "noncharacter"
  | "string_too_long";

export class CanonicalProfileError extends Error {
  readonly code: CanonicalErrorCode;

  constructor(code: CanonicalErrorCode) {
    super(code);
    this.name = "CanonicalProfileError";
    this.code = code;
  }
}

export const PROFILE_LIMITS = {
  max_document_bytes: 1 << 20,
  max_depth: 32,
  max_string_bytes: 64 * 1024,
  max_container_entries: 4096,
  max_safe_integer: Number.MAX_SAFE_INTEGER,
};

const MAX_SAFE_INTEGER_BIGINT = BigInt(Number.MAX_SAFE_INTEGER);
const decoder = new TextDecoder("utf-8", { fatal: true });

class Parser {
  private position = 0;
  private readonly text: string;

  constructor(text: string) {
    this.text = text;
  }

  parse(): CanonicalValue {
    this.skipWhitespace();
    const value = this.parseValue(0);
    this.skipWhitespace();
    if (this.position !== this.text.length) {
      throw new CanonicalProfileError("trailing_data");
    }
    return value;
  }

  private peek(): string | undefined {
    return this.text[this.position];
  }

  private skipWhitespace(): void {
    while (" \t\n\r".includes(this.peek() ?? "\0")) {
      this.position += 1;
    }
  }

  private parseValue(containerDepth: number): CanonicalValue {
    const next = this.peek();
    if (next === "{") return this.parseObject(containerDepth + 1);
    if (next === "[") return this.parseArray(containerDepth + 1);
    if (next === '"') return this.parseString();
    if (next === "-" || (next !== undefined && next >= "0" && next <= "9")) {
      return this.parseInteger();
    }
    if (this.text.startsWith("true", this.position)) {
      this.position += 4;
      return true;
    }
    if (this.text.startsWith("false", this.position)) {
      this.position += 5;
      return false;
    }
    if (this.text.startsWith("null", this.position)) {
      throw new CanonicalProfileError("null_value");
    }
    throw new CanonicalProfileError("syntax");
  }

  private enterContainer(depth: number): void {
    if (depth > PROFILE_LIMITS.max_depth) {
      throw new CanonicalProfileError("too_deep");
    }
  }

  private parseObject(depth: number): CanonicalValue {
    this.enterContainer(depth);
    this.position += 1;
    this.skipWhitespace();
    if (this.peek() === "}") {
      this.position += 1;
      return new Map();
    }

    const members: Array<[string, CanonicalValue]> = [];
    while (true) {
      if (this.peek() !== '"') throw new CanonicalProfileError("syntax");
      const key = this.parseString();
      this.skipWhitespace();
      if (this.peek() !== ":") throw new CanonicalProfileError("syntax");
      this.position += 1;
      this.skipWhitespace();
      const value = this.parseValue(depth);
      members.push([key, value]);
      if (members.length > PROFILE_LIMITS.max_container_entries) {
        throw new CanonicalProfileError("too_many_entries");
      }
      this.skipWhitespace();

      if (this.peek() === ",") {
        this.position += 1;
        this.skipWhitespace();
        continue;
      }
      if (this.peek() !== "}") throw new CanonicalProfileError("syntax");
      this.position += 1;

      const object = new Map<string, CanonicalValue>();
      for (const [memberName, memberValue] of members) {
        if (object.has(memberName)) {
          throw new CanonicalProfileError("duplicate_key");
        }
        object.set(memberName, memberValue);
      }
      return object;
    }
  }

  private parseArray(depth: number): CanonicalValue {
    this.enterContainer(depth);
    this.position += 1;
    this.skipWhitespace();
    if (this.peek() === "]") {
      this.position += 1;
      return [];
    }

    const items: CanonicalValue[] = [];
    while (true) {
      items.push(this.parseValue(depth));
      if (items.length > PROFILE_LIMITS.max_container_entries) {
        throw new CanonicalProfileError("too_many_entries");
      }
      this.skipWhitespace();

      if (this.peek() === ",") {
        this.position += 1;
        this.skipWhitespace();
        continue;
      }
      if (this.peek() !== "]") throw new CanonicalProfileError("syntax");
      this.position += 1;
      return items;
    }
  }

  private parseInteger(): number {
    const start = this.position;
    const negative = this.peek() === "-";
    if (negative) this.position += 1;
    const digitsStart = this.position;

    if (this.peek() === "0") {
      this.position += 1;
    } else {
      const first = this.peek();
      if (first === undefined || first < "1" || first > "9") {
        throw new CanonicalProfileError("syntax");
      }
      while (this.isDigit(this.peek())) this.position += 1;
    }

    if (".eE".includes(this.peek() ?? "\0")) {
      throw new CanonicalProfileError("non_integer_number");
    }

    const digits = this.text.slice(digitsStart, this.position);
    const magnitude = BigInt(digits);
    if (magnitude > MAX_SAFE_INTEGER_BIGINT) {
      throw new CanonicalProfileError("integer_out_of_range");
    }
    if (negative && magnitude === 0n) {
      throw new CanonicalProfileError("negative_zero");
    }
    return Number(negative ? -magnitude : magnitude);
  }

  private isDigit(value: string | undefined): boolean {
    return value !== undefined && value >= "0" && value <= "9";
  }

  private parseString(): string {
    this.position += 1;
    const output: string[] = [];
    let decodedBytes = 0;

    const append = (codePoint: number): void => {
      decodedBytes += this.appendScalar(output, codePoint);
    };

    while (true) {
      const current = this.peek();
      if (current === undefined) throw new CanonicalProfileError("syntax");
      if (current === '"') {
        this.position += 1;
        return output.join("");
      }
      if (current === "\\") {
        this.position += 1;
        const escape = this.peek();
        switch (escape) {
          case '"':
          case "\\":
          case "/":
            append(escape.codePointAt(0)!);
            this.position += 1;
            break;
          case "b":
            append(0x08);
            this.position += 1;
            break;
          case "f":
            append(0x0c);
            this.position += 1;
            break;
          case "n":
            append(0x0a);
            this.position += 1;
            break;
          case "r":
            append(0x0d);
            this.position += 1;
            break;
          case "t":
            append(0x09);
            this.position += 1;
            break;
          case "u":
            this.position += 1;
            append(this.parseUnicodeEscape());
            break;
          default:
            throw new CanonicalProfileError("syntax");
        }
      } else {
        const codeUnit = this.text.charCodeAt(this.position);
        if (codeUnit < 0x20) throw new CanonicalProfileError("syntax");
        const codePoint = this.text.codePointAt(this.position);
        if (codePoint === undefined) throw new CanonicalProfileError("syntax");
        append(codePoint);
        this.position += codePoint > 0xffff ? 2 : 1;
      }

      if (decodedBytes > PROFILE_LIMITS.max_string_bytes) {
        throw new CanonicalProfileError("string_too_long");
      }
    }
  }

  private parseUnicodeEscape(): number {
    const first = this.parseHex4();
    if (first >= 0xd800 && first <= 0xdbff) {
      if (this.text.slice(this.position, this.position + 2) !== "\\u") {
        throw new CanonicalProfileError("lone_surrogate");
      }
      this.position += 2;
      const second = this.parseHex4();
      if (second < 0xdc00 || second > 0xdfff) {
        throw new CanonicalProfileError("lone_surrogate");
      }
      return 0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00);
    }
    if (first >= 0xdc00 && first <= 0xdfff) {
      throw new CanonicalProfileError("lone_surrogate");
    }
    return first;
  }

  private parseHex4(): number {
    const digits = this.text.slice(this.position, this.position + 4);
    if (!/^[0-9a-fA-F]{4}$/.test(digits)) {
      throw new CanonicalProfileError("syntax");
    }
    this.position += 4;
    return Number.parseInt(digits, 16);
  }

  private appendScalar(output: string[], codePoint: number): number {
    if (
      (codePoint >= 0xfdd0 && codePoint <= 0xfdef) ||
      (codePoint & 0xfffe) === 0xfffe
    ) {
      throw new CanonicalProfileError("noncharacter");
    }
    output.push(String.fromCodePoint(codePoint));
    if (codePoint <= 0x7f) return 1;
    if (codePoint <= 0x7ff) return 2;
    if (codePoint <= 0xffff) return 3;
    return 4;
  }
}

export function parseCanonicalJson(input: Uint8Array): CanonicalValue {
  if (input.byteLength > PROFILE_LIMITS.max_document_bytes) {
    throw new CanonicalProfileError("too_large");
  }
  if (
    input.byteLength >= 3 &&
    input[0] === 0xef &&
    input[1] === 0xbb &&
    input[2] === 0xbf
  ) {
    throw new CanonicalProfileError("byte_order_mark");
  }

  let text: string;
  try {
    text = decoder.decode(input);
  } catch {
    throw new CanonicalProfileError("invalid_utf8");
  }
  return new Parser(text).parse();
}

export function compareUtf16(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function quoteCanonicalString(value: string): string {
  let output = '"';
  for (const character of value) {
    const codePoint = character.codePointAt(0)!;
    switch (codePoint) {
      case 0x22:
        output += '\\"';
        break;
      case 0x5c:
        output += "\\\\";
        break;
      case 0x08:
        output += "\\b";
        break;
      case 0x0c:
        output += "\\f";
        break;
      case 0x0a:
        output += "\\n";
        break;
      case 0x0d:
        output += "\\r";
        break;
      case 0x09:
        output += "\\t";
        break;
      default:
        output +=
          codePoint < 0x20
            ? `\\u${codePoint.toString(16).padStart(4, "0")}`
            : character;
    }
  }
  return output + '"';
}

export function canonicalStringify(value: CanonicalValue): string {
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value) || Object.is(value, -0)) {
      throw new Error("canonicalStringify received an out-of-profile number");
    }
    return value.toString(10);
  }
  if (typeof value === "string") return quoteCanonicalString(value);
  if (Array.isArray(value)) {
    return `[${value.map(canonicalStringify).join(",")}]`;
  }
  if (value instanceof Map) {
    const members = Array.from(value.entries()).sort(([left], [right]) =>
      compareUtf16(left, right),
    );
    return `{${members
      .map(([key, memberValue]) =>
        `${quoteCanonicalString(key)}:${canonicalStringify(memberValue)}`,
      )
      .join(",")}}`;
  }
  throw new Error("canonicalStringify received an unsupported value");
}

function boundedCanonicalStringByteLength(
  value: string,
  limit: number,
): number | undefined {
  let rawBytes = 0;
  let encodedBytes = 2;
  for (const character of value) {
    const codePoint = character.codePointAt(0)!;
    if (
      (codePoint >= 0xd800 && codePoint <= 0xdfff) ||
      (codePoint >= 0xfdd0 && codePoint <= 0xfdef) ||
      (codePoint & 0xfffe) === 0xfffe
    ) {
      return undefined;
    }
    let scalarBytes: number;
    if (codePoint <= 0x7f) scalarBytes = 1;
    else if (codePoint <= 0x7ff) scalarBytes = 2;
    else if (codePoint <= 0xffff) scalarBytes = 3;
    else scalarBytes = 4;
    rawBytes += scalarBytes;
    if (rawBytes > PROFILE_LIMITS.max_string_bytes) return undefined;
    const escapedBytes =
      codePoint === 0x22 ||
      codePoint === 0x5c ||
      codePoint === 0x08 ||
      codePoint === 0x0c ||
      codePoint === 0x0a ||
      codePoint === 0x0d ||
      codePoint === 0x09
        ? 2
        : codePoint < 0x20
          ? 6
          : scalarBytes;
    encodedBytes += escapedBytes;
    if (encodedBytes > limit) return undefined;
  }
  return encodedBytes <= limit ? encodedBytes : undefined;
}

function addBoundedCanonicalBytes(
  current: number,
  addition: number,
  limit: number,
): number | undefined {
  const total = current + addition;
  return total <= limit ? total : undefined;
}

function boundedCanonicalByteLengthAtDepth(
  value: CanonicalValue,
  depth: number,
  limit: number,
): number | undefined {
  if (typeof value === "boolean") {
    const length = value ? 4 : 5;
    return length <= limit ? length : undefined;
  }
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value) || Object.is(value, -0)) return undefined;
    const length = value.toString(10).length;
    return length <= limit ? length : undefined;
  }
  if (typeof value === "string") {
    return boundedCanonicalStringByteLength(value, limit);
  }
  if (Array.isArray(value)) {
    if (
      depth >= PROFILE_LIMITS.max_depth ||
      value.length > PROFILE_LIMITS.max_container_entries
    ) {
      return undefined;
    }
    let size = 2;
    if (size > limit) return undefined;
    for (let index = 0; index < value.length; index += 1) {
      if (index > 0) {
        const next = addBoundedCanonicalBytes(size, 1, limit);
        if (next === undefined) return undefined;
        size = next;
      }
      const child = boundedCanonicalByteLengthAtDepth(
        value[index]!,
        depth + 1,
        limit - size,
      );
      if (child === undefined) return undefined;
      const next = addBoundedCanonicalBytes(size, child, limit);
      if (next === undefined) return undefined;
      size = next;
    }
    return size;
  }
  if (value instanceof Map) {
    if (
      depth >= PROFILE_LIMITS.max_depth ||
      value.size > PROFILE_LIMITS.max_container_entries
    ) {
      return undefined;
    }
    let size = 2;
    if (size > limit) return undefined;
    let index = 0;
    for (const [key, childValue] of value) {
      if (index > 0) {
        const next = addBoundedCanonicalBytes(size, 1, limit);
        if (next === undefined) return undefined;
        size = next;
      }
      const keyBytes = boundedCanonicalStringByteLength(key, limit - size);
      if (keyBytes === undefined) return undefined;
      let next = addBoundedCanonicalBytes(size, keyBytes, limit);
      if (next === undefined) return undefined;
      size = next;
      next = addBoundedCanonicalBytes(size, 1, limit);
      if (next === undefined) return undefined;
      size = next;
      const childBytes = boundedCanonicalByteLengthAtDepth(
        childValue,
        depth + 1,
        limit - size,
      );
      if (childBytes === undefined) return undefined;
      next = addBoundedCanonicalBytes(size, childBytes, limit);
      if (next === undefined) return undefined;
      size = next;
      index += 1;
    }
    return size;
  }
  return undefined;
}

/** Measure canonical UTF-8 JSON bytes without building the encoded document. */
export function boundedCanonicalByteLength(
  value: CanonicalValue,
): number | undefined {
  return boundedCanonicalByteLengthAtDepth(
    value,
    0,
    PROFILE_LIMITS.max_document_bytes,
  );
}

export function canonicalizeValue(value: CanonicalValue): Buffer {
  return Buffer.from(canonicalStringify(value), "utf8");
}

export function canonicalizeJson(input: Uint8Array): Buffer {
  return canonicalizeValue(parseCanonicalJson(input));
}
