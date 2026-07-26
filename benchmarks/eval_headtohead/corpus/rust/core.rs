//! Order pipeline core — the symbols the eval questions target.

use std::collections::HashMap;

/// Configuration for the order pipeline (D4 target type).
pub struct OrderConfig {
    pub retries: u32,
    pub region: String,
}

/// Return the default OrderConfig (D1/D5 target).
pub fn load_config() -> OrderConfig {
    OrderConfig {
        retries: 3,
        region: "eu".to_string(),
    }
}

/// Validate an order; error on missing id (D5 'validate' target).
pub fn validate_order(order: &HashMap<String, String>) -> Result<(), String> {
    if !order.contains_key("id") {
        return Err("order missing id".to_string());
    }
    Ok(())
}

/// Process a single validated order (D1/D2 target symbol).
pub fn process_order(order: &HashMap<String, String>) -> HashMap<String, String> {
    let _ = validate_order(order);
    let mut out = HashMap::new();
    out.insert("id".to_string(), order.get("id").cloned().unwrap_or_default());
    out.insert("status".to_string(), "processed".to_string());
    out
}
