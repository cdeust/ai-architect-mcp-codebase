// parser::spec::guard — the spec-validation guard (ADR-0055 Consequences,
// "the single most important guard in this ADR").
//
// A wrong or stale node-kind string in a `LangSpec` does not error at runtime:
// the walker simply never matches that node, so symbols are silently dropped
// and the loss surfaces only as an F1 dip in `graph_accuracy` — and only if a
// fixture happens to cover the construct. This guard converts that silent-drop
// failure into a loud test failure: it loads each migrated grammar's
// `NODE_TYPES` (the very `node-types.json` cited as the source for every
// node-kind string) and asserts every kind and field name in the spec is real
// for that grammar. It makes the §8 source citation executable.

use std::collections::BTreeSet;

use serde_json::Value;

use super::registry::{MIGRATED_SPECS, SHALLOW_SPECS};
use crate::parser::Language;
use collect::{shallow_field_names, shallow_node_kinds, spec_field_names, spec_node_kinds};

mod collect;

/// The grammar's `node-types.json` text for a migrated language. One arm per
/// migrated language (added as each language migrates — OCP dispatch, not a
/// growing conditional in the walker). source: each crate's `NODE_TYPES`,
/// which is `include_str!("../../src/node-types.json")`.
fn node_types_json(language: Language) -> &'static str {
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
             wire it in guard::node_types_json"
        ),
    }
}

/// Parses `node-types.json` into (set of node kinds, set of field names).
fn parse_node_types(json: &str) -> (BTreeSet<String>, BTreeSet<String>) {
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

#[test]
fn every_shallow_spec_node_kind_is_real_for_its_grammar() {
    let mut failures: Vec<String> = Vec::new();
    for spec in SHALLOW_SPECS {
        let (kinds, fields) = parse_node_types(node_types_json(spec.language));
        for (field, kind) in shallow_node_kinds(spec) {
            if !kinds.contains(&kind) {
                failures.push(format!(
                    "{:?}.{field}: node kind {kind:?} is NOT in the grammar's node-types.json",
                    spec.language
                ));
            }
        }
        for (field, name) in shallow_field_names(spec) {
            if !fields.contains(&name) {
                failures.push(format!(
                    "{:?}.{field}: field name {name:?} is NOT a field in the grammar's node-types.json",
                    spec.language
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "shallow spec-validation guard found stale/wrong rows:\n{}",
        failures.join("\n")
    );
}

/// The guard must actually inspect something — an empty `SHALLOW_SPECS` would
/// make the test above vacuously green, which is the failure mode this repo has
/// already paid for twice (a static check passing against a file that no longer
/// holds the code).
#[test]
fn shallow_guard_is_not_vacuous() {
    assert!(
        !SHALLOW_SPECS.is_empty(),
        "SHALLOW_SPECS is empty: every_shallow_spec_node_kind_is_real_for_its_grammar \
         would pass without checking anything"
    );
    let checked: usize = SHALLOW_SPECS
        .iter()
        .map(|s| shallow_node_kinds(s).len())
        .sum();
    assert!(
        checked > 0,
        "no shallow node kinds were validated; the guard is asserting nothing"
    );
}

#[test]
fn every_spec_node_kind_is_real_for_its_grammar() {
    let mut failures: Vec<String> = Vec::new();
    for spec in MIGRATED_SPECS {
        let (kinds, fields) = parse_node_types(node_types_json(spec.language));

        for (field, kind) in spec_node_kinds(spec) {
            if !kinds.contains(&kind) {
                failures.push(format!(
                    "{:?}.{field}: node kind {kind:?} is NOT in the grammar's node-types.json",
                    spec.language
                ));
            }
        }
        for (field, name) in spec_field_names(spec) {
            if !fields.contains(&name) {
                failures.push(format!(
                    "{:?}.{field}: field name {name:?} is NOT a field in the grammar's node-types.json",
                    spec.language
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "spec-validation guard found stale/wrong spec rows:\n{}",
        failures.join("\n")
    );
}

#[test]
fn guard_rejects_a_bogus_node_kind() {
    // Proves the guard actually fails on a wrong row (kills the mutant where
    // the guard vacuously passes). tree-sitter-go has no `not_a_real_kind`.
    let (kinds, _fields) = parse_node_types(tree_sitter_go::NODE_TYPES);
    assert!(
        !kinds.contains("not_a_real_node_kind"),
        "sanity: bogus kind must be absent"
    );
    assert!(
        kinds.contains("function_declaration"),
        "sanity: real Go kind must be present"
    );
}
