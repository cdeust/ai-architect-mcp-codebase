// parser::spec::guard::collect::ts_family — the TypeScript sub-table's
// node-kind and field-name enumerators. Split out of `collect.rs` (§4.1)
// alongside the other per-family enumerators; pure move, no field added or
// dropped.

use super::super::super::lang_spec::TsFamilySpec;

/// The TypeScript sub-table's node kinds. Split into two halves
/// (declaration-shape kinds, then heritage/import/misc kinds) purely to stay
/// under the §4.2 cap — `ts_family_node_kinds` is the one delegation point
/// callers use; `ts_family_heritage_import_kinds` is a private continuation,
/// not a second entry point.
pub(super) fn ts_family_node_kinds(tf: &TsFamilySpec) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
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
    ];
    for (field, kinds) in ts_slices {
        for k in *kinds {
            out.push((field, (*k).to_string()));
        }
    }
    out.extend(ts_family_heritage_import_kinds(tf));
    out
}

/// The heritage-clause, generic, import, and object-literal node kinds of the
/// TypeScript sub-table — the second half of `ts_family_node_kinds`.
fn ts_family_heritage_import_kinds(tf: &TsFamilySpec) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let ts_slices: &[(&'static str, &[&'static str])] = &[
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
    out
}

/// The TypeScript field names read by its walker/conventions (import source,
/// declarator/type-alias value, import-specifier alias).
pub(super) fn ts_family_field_names(tf: &TsFamilySpec) -> Vec<(&'static str, String)> {
    vec![
        ("ts_family.source_field", tf.source_field.to_string()),
        ("ts_family.value_field", tf.value_field.to_string()),
        ("ts_family.alias_field", tf.alias_field.to_string()),
    ]
}
