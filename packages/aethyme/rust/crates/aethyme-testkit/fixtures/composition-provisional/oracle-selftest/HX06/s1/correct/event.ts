import { parseDate } from "./dates";

export function startsAt(raw: string): Date {
  return parseDate(raw, "UTC");
}
