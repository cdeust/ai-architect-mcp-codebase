//! HTTP API layer — uses OrderConfig (D4) and matches 'config' (D5).

use crate::core::{load_config, OrderConfig};
use std::collections::HashMap;

pub fn apply_config(overrides: &HashMap<String, u32>) -> OrderConfig {
    let mut cfg: OrderConfig = load_config();
    if let Some(r) = overrides.get("retries") {
        cfg.retries = *r;
    }
    cfg
}
