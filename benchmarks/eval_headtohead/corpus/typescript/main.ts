// Application entry point (D3 process root).
import { handleRequest } from "./gateway";
import { applyConfig } from "./api";

export function main(): Record<string, unknown> {
  applyConfig({ retries: 5 });
  return handleRequest({ id: "A-1" });
}

main();
