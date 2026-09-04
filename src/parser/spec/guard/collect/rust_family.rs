// parser::spec::guard::collect::rust_family — the Rust sub-table's node-kind
// and field-name enumerators. Split out of `collect.rs` (§4.1) alongside the
// other per-family enumerators; pure move, no field added or dropped.

use super::super::super::lang_spec::RustFamilySpec;

/// The Rust sub-table's node kinds. Split into two halves (item-kind slices,
/// then the use-tree/macro/misc single-value kinds) purely to stay under the
/// §4.2 cap, mirroring `ts_family_node_kinds` / `ts_family_heritage_import_kinds`
/// — `rust_family_node_kinds` is the one delegation point callers use;
/// `rust_family_single_node_kinds` is a private continuation, not a second
/// entry point.
pub(super) fn rust_family_node_kinds(rf: &RustFamilySpec) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let rust_slices: &[(&'static str, &[&'static str])] = &[
        ("rust_family.attribute_kinds", rf.attribute_kinds),
        ("rust_family.function_kinds", rf.function_kinds),
        (
            "rust_family.function_signature_kinds",
            rf.function_signature_kinds,
        ),
        ("rust_family.struct_like_kinds", rf.struct_like_kinds),
        ("rust_family.enum_kinds", rf.enum_kinds),
        ("rust_family.variant_kinds", rf.variant_kinds),
        ("rust_family.variant_list_kinds", rf.variant_list_kinds),
        ("rust_family.trait_kinds", rf.trait_kinds),
        ("rust_family.impl_kinds", rf.impl_kinds),
        ("rust_family.decl_list_kinds", rf.decl_list_kinds),
        ("rust_family.constant_kinds", rf.constant_kinds),
        ("rust_family.macro_def_kinds", rf.macro_def_kinds),
        ("rust_family.extern_crate_kinds", rf.extern_crate_kinds),
        ("rust_family.type_alias_kinds", rf.type_alias_kinds),
        ("rust_family.use_kinds", rf.use_kinds),
        ("rust_family.mod_kinds", rf.mod_kinds),
        ("rust_family.fn_value_arg_kinds", rf.fn_value_arg_kinds),
    ];
    for (field, kinds) in rust_slices {
        for k in *kinds {
            out.push((field, (*k).to_string()));
        }
    }
    out.extend(rust_family_single_node_kinds(rf));
    out
}

/// The use-tree, macro-invocation, and modifier single-value node kinds of the
/// Rust sub-table — the second half of `rust_family_node_kinds`.
fn rust_family_single_node_kinds(rf: &RustFamilySpec) -> Vec<(&'static str, String)> {
    let rust_singles: &[(&'static str, &'static str)] = &[
        ("rust_family.use_list_kind", rf.use_list_kind),
        ("rust_family.scoped_use_list_kind", rf.scoped_use_list_kind),
        ("rust_family.use_as_clause_kind", rf.use_as_clause_kind),
        ("rust_family.use_wildcard_kind", rf.use_wildcard_kind),
        (
            "rust_family.macro_invocation_kind",
            rf.macro_invocation_kind,
        ),
        ("rust_family.token_tree_kind", rf.token_tree_kind),
        ("rust_family.visibility_kind", rf.visibility_kind),
        (
            "rust_family.function_modifiers_kind",
            rf.function_modifiers_kind,
        ),
        ("rust_family.async_kind", rf.async_kind),
    ];
    rust_singles
        .iter()
        .map(|(field, kind)| (*field, (*kind).to_string()))
        .collect()
}

/// The Rust sub-table's field names (the use-tree, impl-trait, and call
/// fields the conventions read).
pub(super) fn rust_family_field_names(rf: &RustFamilySpec) -> Vec<(&'static str, String)> {
    let rust_fields: &[(&'static str, &'static str)] = &[
        ("rust_family.argument_field", rf.argument_field),
        ("rust_family.path_field", rf.path_field),
        ("rust_family.alias_field", rf.alias_field),
        ("rust_family.list_field", rf.list_field),
        ("rust_family.trait_field", rf.trait_field),
        ("rust_family.arguments_field", rf.arguments_field),
        ("rust_family.macro_field", rf.macro_field),
        ("rust_family.callee_field", rf.callee_field),
    ];
    rust_fields
        .iter()
        .map(|(field, name)| (*field, (*name).to_string()))
        .collect()
}
