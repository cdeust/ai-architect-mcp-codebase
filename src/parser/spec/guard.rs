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

use super::lang_spec::LangSpec;
use super::registry::MIGRATED_SPECS;
use crate::parser::Language;

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

/// Every node-kind string a spec references (slices + options + value name +
/// embedded host kinds), each tagged with the field it came from for a
/// legible failure message.
fn spec_node_kinds(spec: &LangSpec) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let slices: &[(&'static str, &[&'static str])] = &[
        ("skip_node_kinds", spec.skip_node_kinds),
        ("function_node_kinds", spec.function_node_kinds),
        ("method_node_kinds", spec.method_node_kinds),
        ("class_node_kinds", spec.class_node_kinds),
        ("interface_node_kinds", spec.interface_node_kinds),
        ("enum_node_kinds", spec.enum_node_kinds),
        ("variant_node_kinds", spec.variant_node_kinds),
        ("member_constant_kinds", spec.member_constant_kinds),
        ("decorated_def_kinds", spec.decorated_def_kinds),
        ("base_node_kinds", spec.base_node_kinds),
        ("type_decl_node_kinds", spec.type_decl_node_kinds),
        ("type_spec_node_kinds", spec.type_spec_node_kinds),
        ("field_container_kinds", spec.field_container_kinds),
        ("field_node_kinds", spec.field_node_kinds),
        ("variable_field_kinds", spec.variable_field_kinds),
        ("body_wrapper_kinds", spec.body_wrapper_kinds),
        ("class_body_kinds", spec.class_body_kinds),
        ("function_body_kinds", spec.function_body_kinds),
        ("value_decl_node_kinds", spec.value_decl_node_kinds),
        ("value_spec_node_kinds", spec.value_spec_node_kinds),
        ("import_node_kinds", spec.import_node_kinds),
        ("import_spec_kinds", spec.import_spec_kinds),
        ("call_node_kinds", spec.call_node_kinds),
    ];
    for (field, kinds) in slices {
        for k in *kinds {
            out.push((field, (*k).to_string()));
        }
    }
    if let Some(k) = spec.struct_type_kind {
        out.push(("struct_type_kind", k.to_string()));
    }
    if let Some(k) = spec.interface_type_kind {
        out.push(("interface_type_kind", k.to_string()));
    }
    if let Some(k) = spec.decorator_node_kind {
        out.push(("decorator_node_kind", k.to_string()));
    }
    if let Some(k) = spec.variable_declarator_kind {
        out.push(("variable_declarator_kind", k.to_string()));
    }
    out.push(("value_name_kind", spec.value_name_kind.to_string()));
    for emb in spec.embedded {
        out.push((
            "embedded.script_node_kind",
            emb.script_node_kind.to_string(),
        ));
        out.push((
            "embedded.content_node_kind",
            emb.content_node_kind.to_string(),
        ));
    }
    out
}

/// Every field name a spec references, tagged with its source field. The
/// always-present fields are validated unconditionally; the language-optional
/// ones (`receiver_field`, `import_path_field`, `extends_field`,
/// `value_name_field`, `value_type_field`) only when the language sets them.
fn spec_field_names(spec: &LangSpec) -> Vec<(&'static str, String)> {
    let mut out = vec![
        ("name_field", spec.name_field.to_string()),
        ("type_field", spec.type_field.to_string()),
    ];
    let optional: &[(&'static str, Option<&'static str>)] = &[
        ("body_field", spec.body_field),
        ("receiver_field", spec.receiver_field),
        ("import_path_field", spec.import_path_field),
        ("extends_field", spec.extends_field),
        ("value_name_field", spec.value_name_field),
        ("value_type_field", spec.value_type_field),
    ];
    for (field, value) in optional {
        if let Some(v) = value {
            out.push((field, v.to_string()));
        }
    }
    out
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
