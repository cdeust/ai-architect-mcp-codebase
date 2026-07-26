//! Background worker — a real caller of process_order (D2 ground truth).

use crate::core::process_order;
use std::collections::HashMap;

pub fn drain(queue: Vec<HashMap<String, String>>) -> Vec<HashMap<String, String>> {
    queue.iter().map(process_order).collect()
}
