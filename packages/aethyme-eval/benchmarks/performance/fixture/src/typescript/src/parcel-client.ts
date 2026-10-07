import { formatParcel } from "./format.js";

export class ParcelClient {
  get(id: string): string {
    return formatParcel(id);
  }
}
