// Inbound gateway — a real caller of processOrder (D2 ground truth).
import { processOrder, validateOrder } from "./core";

export function handleRequest(order: Record<string, unknown>): Record<string, unknown> {
  validateOrder(order);
  return processOrder(order);
}
