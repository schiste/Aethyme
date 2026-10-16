export function formatPrice(amount: number, currency: string): string {
  return `${currency} ${amount.toFixed(2)}`;
}
