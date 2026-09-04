// parser::spec::guard::collect::cpp_family — the hybrid C++ sub-table's
// node-kind and field-name enumerators. Split out of `collect.rs` (§4.1)
// alongside the other per-family enumerators; pure move, no field added or
// dropped.

use super::super::super::lang_spec::CppFamilySpec;

/// The hybrid C++ sub-table's node kinds.
pub(super) fn cpp_family_node_kinds(cf: &CppFamilySpec) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
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
    out.extend(super::naming_node_kinds("cpp_family.naming", cf.naming));
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
    out
}

/// The hybrid C++ sub-table's field names.
pub(super) fn cpp_family_field_names(cf: &CppFamilySpec) -> Vec<(&'static str, String)> {
    let mut out = super::naming_field_names("cpp_family.naming", cf.naming);
    out.push((
        "cpp_family.qualified_scope_field",
        cf.qualified_scope_field.to_string(),
    ));
    out
}
