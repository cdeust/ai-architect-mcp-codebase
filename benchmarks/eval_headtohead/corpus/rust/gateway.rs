//! Inbound gateway — a real caller of process_order (D2 ground truth).

use crate::core::{process_order, validate_order};
use std::collections::HashMap;

pub fn handle_request(order: &HashMap<String, String>) -> HashMap<String, String> {
    let _ = validate_order(order);
    process_order(order)
}
