// parser::spec::guard::collect::objc_family — the Objective-C sub-table's
// node-kind and field-name enumerators. Split out of `collect.rs` (§4.1)
// alongside the other per-family enumerators; pure move, no field added or
// dropped.

use super::super::super::lang_spec::ObjcFamilySpec;

/// The Objective-C sub-table's node kinds.
pub(super) fn objc_family_node_kinds(of: &ObjcFamilySpec) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
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
    out
}

/// The ObjC field names read by its walker (declarator/category/superclass).
pub(super) fn objc_family_field_names(of: &ObjcFamilySpec) -> Vec<(&'static str, String)> {
    vec![
        (
            "objc_family.declarator_field",
            of.declarator_field.to_string(),
        ),
        ("objc_family.category_field", of.category_field.to_string()),
        (
            "objc_family.superclass_field",
            of.superclass_field.to_string(),
        ),
    ]
}
