import { formatPrice } from "./price";

export function cartTotal(total: number): string {
  return formatPrice(total);
}
