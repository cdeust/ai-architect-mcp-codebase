// HTTP API layer — uses OrderConfig (D4) and matches 'config' (D5).
import { loadConfig, OrderConfig } from "./core";

export function applyConfig(overrides: Record<string, number>): OrderConfig {
  const cfg: OrderConfig = loadConfig();
  cfg.retries = overrides.retries ?? cfg.retries;
  return cfg;
}
