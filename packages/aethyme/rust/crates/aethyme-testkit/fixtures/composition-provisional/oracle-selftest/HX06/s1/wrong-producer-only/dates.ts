export function parseDate(text: string, zone: string): Date {
  return new Date(`${text} ${zone}`);
}
