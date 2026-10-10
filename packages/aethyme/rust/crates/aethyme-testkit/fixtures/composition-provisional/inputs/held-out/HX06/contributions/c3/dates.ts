export function parseDate(text: string, zone: string, strict: boolean): Date {
  return strict ? new Date(`${text}T00:00:00${zone}`) : new Date(text);
}
