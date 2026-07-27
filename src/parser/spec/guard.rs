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
use super::registry::{MIGRATED_SPECS, SHALLOW_SPECS};
use super::shallow::ShallowSpec;
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
    // The C-family sub-table's node kinds (validated only when present).
    if let Some(cf) = spec.c_family {
        let cf_slices: &[(&'static str, &[&'static str])] = &[
            ("c_family.struct_like_kinds", cf.struct_like_kinds),
            ("c_family.enum_like_kinds", cf.enum_like_kinds),
            ("c_family.enum_member_kinds", cf.enum_member_kinds),
            ("c_family.typedef_kinds", cf.typedef_kinds),
            ("c_family.func_def_kinds", cf.func_def_kinds),
            ("c_family.func_decl_kinds", cf.func_decl_kinds),
            ("c_family.field_decl_kinds", cf.field_decl_kinds),
            ("c_family.identifier_kinds", cf.identifier_kinds),
        ];
        for (field, kinds) in cf_slices {
            for k in *kinds {
                out.push((field, (*k).to_string()));
            }
        }
        out.push((
            "c_family.func_declarator_kind",
            cf.func_declarator_kind.to_string(),
        ));
        out.push((
            "c_family.init_declarator_kind",
            cf.init_declarator_kind.to_string(),
        ));
        out.push((
            "c_family.field_identifier_kind",
            cf.field_identifier_kind.to_string(),
        ));
    }
    // The hybrid C++ sub-table's node kinds (validated only when present).
    if let Some(cf) = spec.cpp_family {
        let cpp_slices: &[(&'static str, &[&'static str])] = &[
            ("cpp_family.namespace_kinds", cf.namespace_kinds),
            ("cpp_family.class_kinds", cf.class_kinds),
            ("cpp_family.struct_kinds", cf.struct_kinds),
            ("cpp_family.enum_kinds", cf.enum_kinds),
            ("cpp_family.template_kinds", cf.template_kinds),
            ("cpp_family.func_def_kinds", cf.func_def_kinds),
            ("cpp_family.field_decl_kinds", cf.field_decl_kinds),
            ("cpp_family.typedef_kinds", cf.typedef_kinds),
            ("cpp_family.base_type_kinds", cf.base_type_kinds),
            ("cpp_family.identifier_kinds", cf.identifier_kinds),
        ];
        for (field, kinds) in cpp_slices {
            for k in *kinds {
                out.push((field, (*k).to_string()));
            }
        }
        out.push((
            "cpp_family.func_declarator_kind",
            cf.func_declarator_kind.to_string(),
        ));
        out.push((
            "cpp_family.base_clause_kind",
            cf.base_clause_kind.to_string(),
        ));
    }
    // The Objective-C sub-table's node kinds (validated only when present).
    if let Some(of) = spec.objc_family {
        let objc_slices: &[(&'static str, &[&'static str])] = &[
            ("objc_family.class_kinds", of.class_kinds),
            ("objc_family.protocol_kinds", of.protocol_kinds),
            ("objc_family.method_kinds", of.method_kinds),
            ("objc_family.func_def_kinds", of.func_def_kinds),
            ("objc_family.struct_kinds", of.struct_kinds),
            ("objc_family.enum_kinds", of.enum_kinds),
            ("objc_family.enum_member_kinds", of.enum_member_kinds),
            ("objc_family.typedef_kinds", of.typedef_kinds),
            ("objc_family.field_decl_kinds", of.field_decl_kinds),
            ("objc_family.func_body_kinds", of.func_body_kinds),
            ("objc_family.identifier_kinds", of.identifier_kinds),
        ];
        for (field, kinds) in objc_slices {
            for k in *kinds {
                out.push((field, (*k).to_string()));
            }
        }
        out.push((
            "objc_family.field_identifier_kind",
            of.field_identifier_kind.to_string(),
        ));
        out.push((
            "objc_family.plain_identifier_kind",
            of.plain_identifier_kind.to_string(),
        ));
        out.push((
            "objc_family.typedef_name_kind",
            of.typedef_name_kind.to_string(),
        ));
    }
    // The TypeScript sub-table's node kinds (validated only when present).
    if let Some(tf) = spec.ts_family {
        let ts_slices: &[(&'static str, &[&'static str])] = &[
            ("ts_family.type_alias_kinds", tf.type_alias_kinds),
            ("ts_family.export_kinds", tf.export_kinds),
            ("ts_family.method_def_kinds", tf.method_def_kinds),
            ("ts_family.field_def_kinds", tf.field_def_kinds),
            ("ts_family.method_sig_kinds", tf.method_sig_kinds),
            ("ts_family.property_sig_kinds", tf.property_sig_kinds),
            ("ts_family.class_body_kinds", tf.class_body_kinds),
            ("ts_family.interface_body_kinds", tf.interface_body_kinds),
            ("ts_family.enum_body_kinds", tf.enum_body_kinds),
            ("ts_family.enum_assignment_kinds", tf.enum_assignment_kinds),
            (
                "ts_family.enum_member_ident_kinds",
                tf.enum_member_ident_kinds,
            ),
            ("ts_family.heritage_kinds", tf.heritage_kinds),
            ("ts_family.extends_clause_kinds", tf.extends_clause_kinds),
            (
                "ts_family.implements_clause_kinds",
                tf.implements_clause_kinds,
            ),
            (
                "ts_family.interface_extends_kinds",
                tf.interface_extends_kinds,
            ),
            ("ts_family.heritage_name_kinds", tf.heritage_name_kinds),
            ("ts_family.generic_type_kinds", tf.generic_type_kinds),
            ("ts_family.import_clause_kinds", tf.import_clause_kinds),
            ("ts_family.named_imports_kinds", tf.named_imports_kinds),
            (
                "ts_family.namespace_import_kinds",
                tf.namespace_import_kinds,
            ),
            (
                "ts_family.import_specifier_kinds",
                tf.import_specifier_kinds,
            ),
            (
                "ts_family.default_import_ident_kinds",
                tf.default_import_ident_kinds,
            ),
            ("ts_family.declarator_kinds", tf.declarator_kinds),
            ("ts_family.arrow_kinds", tf.arrow_kinds),
            ("ts_family.accessibility_kinds", tf.accessibility_kinds),
        ];
        for (field, kinds) in ts_slices {
            for k in *kinds {
                out.push((field, (*k).to_string()));
            }
        }
    }
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
    // The C-family declarator field (the struct/function name lives under it).
    if let Some(cf) = spec.c_family {
        out.push(("c_family.declarator_field", cf.declarator_field.to_string()));
    }
    // The C++ declarator field (the function/method name lives under it).
    if let Some(cf) = spec.cpp_family {
        out.push((
            "cpp_family.declarator_field",
            cf.declarator_field.to_string(),
        ));
    }
    // The ObjC field names read by its walker (declarator/category/superclass).
    if let Some(of) = spec.objc_family {
        out.push((
            "objc_family.declarator_field",
            of.declarator_field.to_string(),
        ));
        out.push(("objc_family.category_field", of.category_field.to_string()));
        out.push((
            "objc_family.superclass_field",
            of.superclass_field.to_string(),
        ));
    }
    // The TypeScript field names read by its walker/conventions
    // (import source, declarator/type-alias value, import-specifier alias).
    if let Some(tf) = spec.ts_family {
        out.push(("ts_family.source_field", tf.source_field.to_string()));
        out.push(("ts_family.value_field", tf.value_field.to_string()));
        out.push(("ts_family.alias_field", tf.alias_field.to_string()));
    }
    out
}

/// Every node-kind string a shallow row references, tagged with its field.
/// Mirrors `spec_node_kinds` for the ADR-0056 path so a shallow language gets
/// the same executable §8 citation as a deep one.
fn shallow_node_kinds(spec: &ShallowSpec) -> Vec<(&'static str, String)> {
    let slices: &[(&'static str, &[&'static str])] = &[
        ("function_node_kinds", spec.function_node_kinds),
        ("method_node_kinds", spec.method_node_kinds),
        ("class_node_kinds", spec.class_node_kinds),
        ("call_node_kinds", spec.call_node_kinds),
        ("import_node_kinds", spec.import_node_kinds),
    ];
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for (field, kinds) in slices {
        for k in *kinds {
            out.push((field, (*k).to_string()));
        }
    }
    out
}

/// Every field name a shallow row references. `name_field` is always read;
/// `body_field` and `callee_field` only when the row sets them.
fn shallow_field_names(spec: &ShallowSpec) -> Vec<(&'static str, String)> {
    let mut out = vec![("name_field", spec.name_field.to_string())];
    let optional: &[(&'static str, Option<&'static str>)] = &[
        ("body_field", spec.body_field),
        ("callee_field", spec.callee_field),
    ];
    for (field, value) in optional {
        if let Some(v) = value {
            out.push((field, v.to_string()));
        }
    }
    out
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
