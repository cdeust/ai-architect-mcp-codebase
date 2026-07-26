//! Legacy shim — a LEXICAL DISTRACTOR: mentions process_order only in prose.

use std::collections::HashMap;

// TODO: migrate callers off process_order once the v2 pipeline lands.
pub fn deprecated_entry(payload: String) -> HashMap<String, String> {
    let mut out = HashMap::new();
    out.insert("warning".to_string(), "use handle_request instead".to_string());
    out.insert("payload".to_string(), payload);
    out
}
