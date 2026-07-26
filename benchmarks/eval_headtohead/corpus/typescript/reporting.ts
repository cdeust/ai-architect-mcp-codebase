// Reporting — a LEXICAL DISTRACTOR module (§3 pre-registration).
// Mentions processOrder in a comment and a string, and defines
// substring-colliding names (preprocessOrder, processOrders). None of these
// are real call sites of core.processOrder.

// NOTE: processOrder is the hot path; report on its throughput here.
export const LABEL = "processOrder latency";

export function preprocessOrder(order: Record<string, unknown>): Record<string, unknown> {
  return { ...order };
}

export function processOrders(orders: Record<string, unknown>[]): Record<string, unknown>[] {
  return orders.map((o) => preprocessOrder(o));
}
