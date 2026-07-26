// Background worker — a real caller of processOrder (D2 ground truth).
import { processOrder } from "./core";

export function drain(queue: Record<string, unknown>[]): Record<string, unknown>[] {
  return queue.map((order) => processOrder(order));
}
