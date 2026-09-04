// parser::spec::guard::collect — the spec/shallow-row ENUMERATORS the guard
// asserts over: every node-kind string and field name a LangSpec / ShallowSpec
// (and its family sub-tables) references, each tagged with its source field for
// a legible failure message. Split out of `guard` so the enumerators and the
// #[test] assertions each stay under the §4.1 500-line cap. Pure move.
//
// Split again into a directory module (mirroring `parser::spec::walkers`,
// issue #101) once Rust-family support pushed the single-file version over
// the §4.1 cap: each family's node-kind/field-name enumerators live in their
// own file (`c_family`, `cpp_family`, `objc_family`, `ts_family`,
// `rust_family`); this file keeps only the shared helpers and the two
// dispatchers (`spec_node_kinds`, `spec_field_names`) the guard calls. Pure
// move — no field added or dropped, no output reordered.

mod c_family;
mod cpp_family;
mod objc_family;
mod rust_family;
mod ts_family;

use super::super::lang_spec::{DeclaratorNaming, LangSpec};
use super::super::shallow::ShallowSpec;

/// Every node-kind string a `DeclaratorNaming` sub-table references, tagged with
/// its path. One helper for both C-family rows, so adding a family cannot forget
/// to validate its naming data (issues #106, #123).
pub(super) fn naming_node_kinds(
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
/// embedded host kinds + every per-family sub-table's own kinds), each tagged
/// with the field it came from for a legible failure message. Delegates each
/// family sub-table to its own module (`c_family`, `cpp_family`,
/// `objc_family`, `ts_family`, `rust_family`) so this dispatcher, and each
/// family's own enumerator, stay under the §4.2 50-line cap — a pure split,
/// no field added or dropped.
pub(super) fn spec_node_kinds(spec: &LangSpec) -> Vec<(&'static str, String)> {
    let mut out = core_spec_node_kinds(spec);
    if let Some(cf) = spec.c_family {
        out.extend(c_family::c_family_node_kinds(cf));
    }
    if let Some(cf) = spec.cpp_family {
        out.extend(cpp_family::cpp_family_node_kinds(cf));
    }
    if let Some(of) = spec.objc_family {
        out.extend(objc_family::objc_family_node_kinds(of));
    }
    if let Some(tf) = spec.ts_family {
        out.extend(ts_family::ts_family_node_kinds(tf));
    }
    if let Some(rf) = spec.rust_family {
        out.extend(rust_family::rust_family_node_kinds(rf));
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

/// The always-present slices + single-option fields every `LangSpec` carries,
/// independent of which (if any) family sub-table it populates.
fn core_spec_node_kinds(spec: &LangSpec) -> Vec<(&'static str, String)> {
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
        ("type_construction_kinds", spec.type_construction_kinds),
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
    out
}

/// Every field name a spec references, tagged with its source field. The
/// always-present fields are validated unconditionally; the language-optional
/// ones (`receiver_field`, `import_path_field`, `extends_field`,
/// `value_name_field`, `value_type_field`) only when the language sets them.
pub(super) fn spec_field_names(spec: &LangSpec) -> Vec<(&'static str, String)> {
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
        ("return_type_field", spec.return_type_field),
        ("construction_type_field", spec.construction_type_field),
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
        out.extend(cpp_family::cpp_family_field_names(cf));
    }
    if let Some(of) = spec.objc_family {
        out.extend(objc_family::objc_family_field_names(of));
    }
    if let Some(tf) = spec.ts_family {
        out.extend(ts_family::ts_family_field_names(tf));
    }
    if let Some(rf) = spec.rust_family {
        out.extend(rust_family::rust_family_field_names(rf));
    }
    out
}

/// The field names a `DeclaratorNaming` sub-table references, tagged with its
/// path. One helper for both C-family rows (mirrors `naming_node_kinds`).
pub(super) fn naming_field_names(
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
pub(super) fn shallow_node_kinds(spec: &ShallowSpec) -> Vec<(&'static str, String)> {
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
pub(super) fn shallow_field_names(spec: &ShallowSpec) -> Vec<(&'static str, String)> {
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
