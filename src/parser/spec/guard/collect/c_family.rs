// parser::spec::guard::collect::c_family — the C-family sub-table's
// node-kind enumerator. Split out of `collect.rs` (§4.1) alongside the other
// per-family enumerators; pure move, no field added or dropped.

use super::super::super::lang_spec::CFamilySpec;

/// The C-family sub-table's node kinds.
pub(super) fn c_family_node_kinds(cf: &CFamilySpec) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
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
    out.extend(super::naming_node_kinds("c_family.naming", cf.naming));
    out.push((
        "c_family.func_declarator_kind",
        cf.func_declarator_kind.to_string(),
    ));
    out.push((
        "c_family.field_identifier_kind",
        cf.field_identifier_kind.to_string(),
    ));
    out
}
