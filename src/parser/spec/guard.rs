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

use super::guard_grammar::{node_types_json, parse_node_types};
use super::lang_spec::{DeclaratorNaming, LangSpec};
use super::registry::{MIGRATED_SPECS, SHALLOW_SPECS};
use super::shallow::ShallowSpec;

/// Every node-kind string a `DeclaratorNaming` sub-table references, tagged with
/// its path. One helper for both C-family rows, so adding a family cannot forget
/// to validate its naming data (issues #106, #123).
fn naming_node_kinds(
    prefix: &'static str,
    naming: &DeclaratorNaming,
) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for k in naming.identifier_kinds {
        out.push((prefix, (*k).to_string()));
    }
    for k in naming.name_text_kinds {
        out.push((prefix, (*k).to_string()));
    }
    for k in naming.indirection_declarator_kinds {
        out.push((prefix, (*k).to_string()));
    }
    out
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
        ];
        for (field, kinds) in cf_slices {
            for k in *kinds {
                out.push((field, (*k).to_string()));
            }
        }
        out.extend(naming_node_kinds("c_family.naming", cf.naming));
        out.push((
            "c_family.func_declarator_kind",
            cf.func_declarator_kind.to_string(),
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
            ("cpp_family.enum_member_kinds", cf.enum_member_kinds),
            ("cpp_family.template_kinds", cf.template_kinds),
            ("cpp_family.func_def_kinds", cf.func_def_kinds),
            ("cpp_family.field_decl_kinds", cf.field_decl_kinds),
            ("cpp_family.member_decl_kinds", cf.member_decl_kinds),
            ("cpp_family.typedef_kinds", cf.typedef_kinds),
            ("cpp_family.alias_kinds", cf.alias_kinds),
            ("cpp_family.base_type_kinds", cf.base_type_kinds),
        ];
        for (field, kinds) in cpp_slices {
            for k in *kinds {
                out.push((field, (*k).to_string()));
            }
        }
        out.extend(naming_node_kinds("cpp_family.naming", cf.naming));
        out.push((
            "cpp_family.func_declarator_kind",
            cf.func_declarator_kind.to_string(),
        ));
        out.push((
            "cpp_family.qualified_declarator_kind",
            cf.qualified_declarator_kind.to_string(),
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
            (
                "objc_family.method_parameter_kinds",
                of.method_parameter_kinds,
            ),
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
            (
                "ts_family.abstract_method_sig_kinds",
                tf.abstract_method_sig_kinds,
            ),
            ("ts_family.object_literal_kinds", tf.object_literal_kinds),
            ("ts_family.pair_kinds", tf.pair_kinds),
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
    // The C-family naming fields: the declarator chain the name lives under, and
    // the parameter list the search must SKIP (#106/#123). A stale
    // `parameters_field` would silently reintroduce the last-parameter defect —
    // the skip would match nothing — so it is validated like any other field.
    if let Some(cf) = spec.c_family {
        out.extend(naming_field_names("c_family.naming", cf.naming));
    }
    if let Some(cf) = spec.cpp_family {
        out.extend(naming_field_names("cpp_family.naming", cf.naming));
        out.push((
            "cpp_family.qualified_scope_field",
            cf.qualified_scope_field.to_string(),
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

/// The field names a `DeclaratorNaming` sub-table references, tagged with its
/// path. One helper for both C-family rows (mirrors `naming_node_kinds`).
fn naming_field_names(
    prefix: &'static str,
    naming: &DeclaratorNaming,
) -> Vec<(&'static str, String)> {
    vec![
        (prefix, naming.declarator_field.to_string()),
        (prefix, naming.parameters_field.to_string()),
    ]
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
