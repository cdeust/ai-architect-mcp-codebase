//! Reporting — a LEXICAL DISTRACTOR module (§3 pre-registration).
//! Mentions process_order in a comment and a string, and defines
//! substring-colliding names (preprocess_order, process_orders). None of these
//! are real call sites of core::process_order.

use std::collections::HashMap;

// NOTE: process_order is the hot path; report on its throughput here.
pub const LABEL: &str = "process_order latency";

pub fn preprocess_order(order: &HashMap<String, String>) -> HashMap<String, String> {
    order.clone()
}

pub fn process_orders(orders: Vec<HashMap<String, String>>) -> Vec<HashMap<String, String>> {
    orders.iter().map(preprocess_order).collect()
}
