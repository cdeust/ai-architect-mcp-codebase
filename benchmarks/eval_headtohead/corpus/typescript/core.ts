// Order pipeline core — the symbols the eval questions target.

export class OrderConfig {
  retries: number;
  region: string;
  constructor(retries = 3, region = "eu") {
    this.retries = retries;
    this.region = region;
  }
}

export function loadConfig(): OrderConfig {
  return new OrderConfig();
}

export function validateOrder(order: Record<string, unknown>): boolean {
  if (!("id" in order)) {
    throw new Error("order missing id");
  }
  return true;
}

export function processOrder(order: Record<string, unknown>): Record<string, unknown> {
  validateOrder(order);
  return { id: order.id, status: "processed" };
}
