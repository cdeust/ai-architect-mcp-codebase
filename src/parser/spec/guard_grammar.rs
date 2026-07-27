// parser::spec::guard_grammar — grammar-introspection helpers for the spec
// guard (split from `guard.rs` along the §4.1 concern boundary: loading and
// parsing a grammar's `node-types.json` is a distinct concern from walking a
// `LangSpec` to collect the kinds/fields it references).
//
// Both helpers are consumed only by the `#[cfg(test)] mod guard` validation
// tests. source: each grammar crate's `NODE_TYPES`
// (`include_str!("../../src/node-types.json")`).

use std::collections::BTreeSet;

use serde_json::Value;

use crate::parser::Language;

/// The grammar's `node-types.json` text for a migrated language. One arm per
/// migrated language (added as each language migrates — OCP dispatch, not a
/// growing conditional in the walker). source: each crate's `NODE_TYPES`,
/// which is `include_str!("../../src/node-types.json")`.
pub(super) fn node_types_json(language: Language) -> &'static str {
    match language {
        Language::Go => tree_sitter_go::NODE_TYPES,
        Language::Python => tree_sitter_python::NODE_TYPES,
        Language::Java => tree_sitter_java::NODE_TYPES,
        Language::Kotlin => tree_sitter_kotlin_ng::NODE_TYPES,
        Language::Swift => tree_sitter_swift::NODE_TYPES,
        Language::C => tree_sitter_c::NODE_TYPES,
        Language::Cpp => tree_sitter_cpp::NODE_TYPES,
        Language::ObjC => tree_sitter_objc::NODE_TYPES,
        // TypeScript ships two grammars (typescript + tsx); every node kind in
        // TS_FAMILY is a core-TS kind present in the typescript grammar (the tsx
        // grammar adds only JSX kinds, which the spec does not reference), so the
        // typescript node-types.json is the validation source.
        Language::TypeScript => tree_sitter_typescript::TYPESCRIPT_NODE_TYPES,
        // Shallow-path languages (ADR-0056) are validated by the same guard:
        // a stale node kind in a shallow row drops symbols exactly as silently
        // as in a deep row, so breadth must not come with weaker validation.
        Language::Ruby => tree_sitter_ruby::NODE_TYPES,
        // A spec row for a not-yet-wired language would fail loudly here rather
        // than silently skip validation.
        other => panic!(
            "spec guard: no node-types.json wired for migrated language {other:?}; \
             wire it in guard_grammar::node_types_json"
        ),
    }
}

/// Parses `node-types.json` into (set of node kinds, set of field names).
pub(super) fn parse_node_types(json: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let root: Value = serde_json::from_str(json).expect("node-types.json is valid JSON");
    let entries = root.as_array().expect("node-types.json is a JSON array");
    let mut kinds = BTreeSet::new();
    let mut fields = BTreeSet::new();
    for entry in entries {
        if let Some(t) = entry.get("type").and_then(Value::as_str) {
            kinds.insert(t.to_string());
        }
        // Supertype nodes list their concrete members under "subtypes".
        if let Some(subs) = entry.get("subtypes").and_then(Value::as_array) {
            for s in subs {
                if let Some(t) = s.get("type").and_then(Value::as_str) {
                    kinds.insert(t.to_string());
                }
            }
        }
        if let Some(field_map) = entry.get("fields").and_then(Value::as_object) {
            for name in field_map.keys() {
                fields.insert(name.clone());
            }
        }
    }
    (kinds, fields)
}
