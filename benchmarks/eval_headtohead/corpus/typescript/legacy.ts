// Legacy shim — a LEXICAL DISTRACTOR: mentions processOrder only in prose.

// TODO: migrate callers off processOrder once the v2 pipeline lands.
export function deprecatedEntry(payload: unknown): Record<string, unknown> {
  return { warning: "use gateway.handleRequest instead", payload };
}
