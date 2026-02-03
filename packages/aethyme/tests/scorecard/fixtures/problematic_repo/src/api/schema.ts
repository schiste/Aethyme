export interface User {
  id: string;
  name: string;
  data: any;  // Using 'any' reduces type safety
  metadata: any;
}
