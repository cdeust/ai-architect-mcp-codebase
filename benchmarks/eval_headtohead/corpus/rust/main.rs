//! Application entry point (D3 process root).

mod api;
mod core;
mod gateway;
mod legacy;
mod reporting;
mod worker;

use std::collections::HashMap;

fn main() {
    let mut ov = HashMap::new();
    ov.insert("retries".to_string(), 5u32);
    api::apply_config(&ov);
    let mut order = HashMap::new();
    order.insert("id".to_string(), "A-1".to_string());
    gateway::handle_request(&order);
}
