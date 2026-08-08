// parser::spec::walkers::typescript — the TypeScript/TSX definition walker
// (ADR-0055 phase 9). Reproduces the pre-migration hand-written TS walker
// (`parser::typescript::extract`) at EXACT parity, driven by the `TsFamilySpec`
// sub-table instead of hardcoded `TS_*` constants.
//
// TypeScript is class-model-shaped but carries six structural divergences the
// generic class-model arms cannot express (enumerated on `TsFamilySpec`), so
// `walk_defs` delegates the whole file here. This module holds the dispatch,
// the export wrapper, functions, classes (heritage + body), methods, fields,
// and the call-site walk; the remaining concerns (interfaces, enums, type
// aliases, value declarations) live in the `members` submodule (split along the
// §4.1 concern boundary). Imports route through the generic `imports::walk_imports`
// via `TsConventions::imports_of` (edge kind `Defines`).
//
// Distinctive behaviors preserved here:
//   - An `export_statement` marks its inner declaration `pub` (threaded, not a
//     token check); a re-export (`export { x }`, `export * from`) emits nothing.
//   - A getter and setter of the same name are TWO `Method`s on ONE QN (no
//     dedup — `def_qn` is `{scope}::{name}`).
//   - A class member is a `Field` + `HasField`; an interface `property_signature`
//     is likewise a `Field`. A field/const/property's `type_annotation` is the
//     BARE type text (`number`), the grammar's leading `: ` stripped (issue #140),
//     matching the clike/constants/types walkers' bare `type_field` convention.
//   - An abstract method signature (`abstract_method_signature`) inside a class
//     body emits a `Method` + `HasMethod` like a concrete method (bodiless, so no
//     calls are scanned), aligned with Swift's bodiless
//     `protocol_function_declaration` and Java's `abstract` `method_declaration`
//     (issue #141).
//   - A call site emits TWO refs: `Defines`(caller → call-site node) AND
//     `Calls`(caller → callee tail).

mod members;

use tree_sitter::Node;

use super::super::lang_spec::{LangSpec, TsFamilySpec};
use super::{end_line_of, imports, kind_in, line_of, type_uses, WalkCtx};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_CALL_SITE, LABEL_FIELD,
    LABEL_FUNCTION, LABEL_METHOD, LABEL_STRUCT,
};

/// Top-level TypeScript definition walker: dispatches each child of `parent` to
/// the concern its node kind names. At file scope declarations are not exported
/// unless wrapped in an `export_statement`; `dispatch_decl` threads the export
/// flag into the same emitters.
pub(super) fn walk_ts_defs(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    parent: Node,
    scope: &str,
) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        dispatch_decl(spec, tf, ctx, child, scope, false);
    }
}

/// Dispatches one top-level or exported declaration. `is_exported` is `true`
/// when reached through an `export_statement`. An `import_statement` routes to
/// the generic import walker; an `export_statement` recurses its children with
/// the flag set; every other kind is a def/class/interface/enum/type-alias/
/// value declaration or (a re-export clause, a decorator, punctuation) ignored.
fn dispatch_decl(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    child: Node,
    scope: &str,
    is_exported: bool,
) {
    let k = child.kind();
    if kind_in(spec.import_node_kinds, k) {
        imports::walk_imports(spec, ctx, child, scope);
    } else if kind_in(tf.export_kinds, k) {
        let mut cursor = child.walk();
        for inner in child.children(&mut cursor) {
            dispatch_decl(spec, tf, ctx, inner, scope, true);
        }
    } else if kind_in(spec.function_node_kinds, k) {
        emit_function(spec, ctx, child, scope, is_exported);
    } else if kind_in(spec.class_node_kinds, k) {
        emit_class(spec, tf, ctx, child, scope, is_exported);
    } else if kind_in(spec.interface_node_kinds, k) {
        members::emit_interface(spec, tf, ctx, child, scope, is_exported);
    } else if kind_in(spec.enum_node_kinds, k) {
        members::emit_enum(spec, tf, ctx, child, scope, is_exported);
    } else if kind_in(tf.type_alias_kinds, k) {
        members::emit_type_alias(spec, tf, ctx, child, scope, is_exported);
    } else if kind_in(spec.value_decl_node_kinds, k) {
        members::emit_value_decl(spec, tf, ctx, child, scope, is_exported);
    }
}

/// `"pub"` for an exported declaration, empty otherwise. TS visibility of a
/// top-level definition is exactly its export status.
pub(super) fn export_vis(is_exported: bool) -> String {
    if is_exported {
        "pub".to_string()
    } else {
        String::new()
    }
}

/// Whether a node's source text begins with the `async ` keyword. Reproduces
/// the hand-written `has_async_keyword`. A generator (`function*`) begins with
/// `function`, so it is correctly non-async.
pub(super) fn has_async(source: &str, node: Node) -> bool {
    source[node.byte_range()].starts_with("async ")
}

/// The visibility text of a class member: the first `accessibility_kinds`
/// (`accessibility_modifier`) child's text (`public`/`private`/`protected`),
/// or empty when none is present. Reproduces `extract_ts_member_visibility`.
pub(super) fn member_visibility(tf: &TsFamilySpec, source: &str, node: Node) -> String {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| kind_in(tf.accessibility_kinds, c.kind()))
        .map(|c| node_text(source, c))
        .unwrap_or_default();
    found
}

/// The BARE type text of a member's `type` field (a `type_annotation` node whose
/// grammar text is `": T"`): the leading `:` and surrounding whitespace stripped,
/// so a class field / interface property / const `age: number` yields `number`
/// rather than `: number` (issue #140). Empty when the member has no annotation
/// (the field is absent → `node_field_text` returns empty → no `:` to strip).
/// Aligns TS with the house convention: the clike/constants/types walkers all
/// store the bare `type_field` text.
///
/// pre: `node` is a class field / interface property / const declarator whose
///      `type_field` (when present) names a `type_annotation` node.
/// post: result has no leading `:` and no surrounding whitespace; empty iff the
///       annotation is absent.
pub(super) fn ts_type_annotation(source: &str, node: Node, type_field: &str) -> String {
    let raw = node_field_text(source, node, type_field);
    match raw.trim_start().strip_prefix(':') {
        Some(rest) => rest.trim().to_string(),
        None => raw,
    }
}

/// Emits a free function (`Function` + `Defines`), `is_async` property, `pub`
/// when exported, and scans its body for calls. Both `function_declaration` and
/// `generator_function_declaration` route here.
fn emit_function(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str, is_exported: bool) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = spec.conventions.def_qn(scope, &name, 0);
    let body = spec.body_field.and_then(|f| node.child_by_field_name(f));
    let mut properties = vec![(
        "is_async".to_string(),
        has_async(ctx.source, node).to_string(),
    )];
    // Return type + constructed types (issue #92).
    type_uses::append_type_use_props(spec, ctx, node, body, &mut properties);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_FUNCTION.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: export_vis(is_exported),
        properties,
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = body {
        ts_walk_calls(spec, ctx, body, &qn);
    }
}

/// A class's collected heritage: base-class names (`extends`) and
/// interface names (`implements`), gathered once and reused for both the
/// `bases`/`implements` node properties (what `resolver::extends`/
/// `resolver::implements` actually read — `s.bases`/`s.implements` CSV
/// columns, per `resolve_extends`/`resolve_one_implements`) and the
/// `Extends`/`Implements` refs (kept for parity with the ref-level
/// ground truth; the indexer defers/drops raw ref kinds and relies on the
/// CSV properties for resolution — see `indexer::persist::edges::
/// resolve_edge_table`).
struct ClassHeritage {
    extends: Vec<String>,
    implements: Vec<String>,
}

impl ClassHeritage {
    fn is_empty(&self) -> bool {
        self.extends.is_empty() && self.implements.is_empty()
    }
}

/// Emits a class (`Struct` + `Defines`), its heritage (`bases`/`implements`
/// properties + `Extends`/`Implements` refs), then recurses its body for
/// methods and fields. Both `class_declaration` and
/// `abstract_class_declaration` route here.
fn emit_class(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    is_exported: bool,
) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let heritage = collect_class_heritage(tf, ctx.source, node);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_STRUCT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: export_vis(is_exported),
        properties: class_heritage_properties(&heritage),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    emit_class_heritage_refs(ctx, &heritage, &qn);
    if let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) {
        emit_class_body(spec, tf, ctx, body, &qn);
    }
}

/// The `bases`/`implements` CSV properties for a `Struct` node, mirroring
/// Java's `class_inheritance` convention (`JavaConventions::class_inheritance`):
/// a property is recorded only when its clause is present.
fn class_heritage_properties(heritage: &ClassHeritage) -> Vec<(String, String)> {
    let mut properties = Vec::new();
    if !heritage.extends.is_empty() {
        properties.push(("bases".to_string(), heritage.extends.join(",")));
    }
    if !heritage.implements.is_empty() {
        properties.push(("implements".to_string(), heritage.implements.join(",")));
    }
    properties
}

/// Collects the base-class (`extends_clause`) and interface (`implements_clause`)
/// names out of a class's `class_heritage`, unwrapping a `generic_type` base
/// (`extends Wrapper<T>` → `Wrapper`). Reproduces `extract_class_heritage` +
/// `extract_heritage_clause`, but returns the names instead of emitting refs
/// directly, so the same walk feeds both the CSV properties and the refs.
fn collect_class_heritage(tf: &TsFamilySpec, source: &str, class_node: Node) -> ClassHeritage {
    let mut heritage = ClassHeritage {
        extends: Vec::new(),
        implements: Vec::new(),
    };
    let mut cursor = class_node.walk();
    for child in class_node.children(&mut cursor) {
        if !kind_in(tf.heritage_kinds, child.kind()) {
            continue;
        }
        let mut hcursor = child.walk();
        for clause in child.children(&mut hcursor) {
            let target = if kind_in(tf.extends_clause_kinds, clause.kind()) {
                &mut heritage.extends
            } else if kind_in(tf.implements_clause_kinds, clause.kind()) {
                &mut heritage.implements
            } else {
                continue;
            };
            collect_heritage_names(tf, source, clause, target);
        }
    }
    heritage
}

/// Appends one name per base type named directly in a heritage `clause`: a
/// bare `identifier`/`type_identifier`, or the `name` child of a `generic_type`.
fn collect_heritage_names(tf: &TsFamilySpec, source: &str, clause: Node, out: &mut Vec<String>) {
    let mut cursor = clause.walk();
    for child in clause.children(&mut cursor) {
        let name = if kind_in(tf.heritage_name_kinds, child.kind()) {
            node_text(source, child)
        } else if kind_in(tf.generic_type_kinds, child.kind()) {
            child
                .child_by_field_name("name")
                .map(|n| node_text(source, n))
                .unwrap_or_default()
        } else {
            continue;
        };
        if !name.is_empty() {
            out.push(name);
        }
    }
}

/// Emits one `Extends` ref per collected base-class name and one `Implements`
/// ref per collected interface name — unchanged ref-level output, now sourced
/// from the already-collected `ClassHeritage` instead of a second tree walk.
fn emit_class_heritage_refs(ctx: &mut WalkCtx, heritage: &ClassHeritage, class_qn: &str) {
    if heritage.is_empty() {
        return;
    }
    for name in &heritage.extends {
        ctx.refs.push(ExtractedRef {
            kind: "Extends".to_string(),
            from_qualified_name: class_qn.to_string(),
            to_qualified_name: name.clone(),
        });
    }
    for name in &heritage.implements {
        ctx.refs.push(ExtractedRef {
            kind: "Implements".to_string(),
            from_qualified_name: class_qn.to_string(),
            to_qualified_name: name.clone(),
        });
    }
}

/// Recurses a `class_body`, emitting a `Method` per `method_definition`, a
/// `Method` per `abstract_method_signature` (bodiless requirement, issue #141),
/// and a `Field` per `public_field_definition`. Other member kinds are ignored.
fn emit_class_body(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    body: Node,
    class_qn: &str,
) {
    if !kind_in(tf.class_body_kinds, body.kind()) {
        return;
    }
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        let k = child.kind();
        if kind_in(tf.method_def_kinds, k) {
            emit_method(spec, tf, ctx, child, class_qn);
        } else if kind_in(tf.abstract_method_sig_kinds, k) {
            emit_abstract_method(spec, tf, ctx, child, class_qn);
        } else if kind_in(tf.field_def_kinds, k) {
            emit_field(spec, tf, ctx, child, class_qn);
        }
    }
}

/// Emits a `Method` + `HasMethod` (receiver = the class QN) for an
/// `abstract_method_signature` — an abstract member is a requirement, extracted
/// like a concrete method so it is visible in the graph (issue #141). It is
/// bodiless, so no calls are scanned and `is_async` is always `false` (an
/// abstract member carries no `async ` keyword). Visibility comes from any
/// `accessibility_modifier` child, else empty. This mirrors Swift's bodiless
/// `protocol_function_declaration` (#98) and Java's `abstract` `method_declaration`.
///
/// pre: `node` is an `abstract_method_signature` inside `class_qn`'s body.
/// post: exactly one `Method` node and one `HasMethod`(class → method) ref are
///       pushed; no `CallSite`/`Calls` (bodiless). Emits nothing if unnamed.
fn emit_abstract_method(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    class_qn: &str,
) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = spec.conventions.def_qn(class_qn, &name, 0);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: member_visibility(tf, ctx.source, node),
        properties: vec![
            ("is_async".to_string(), "false".to_string()),
            ("receiver_type".to_string(), class_qn.to_string()),
        ],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: class_qn.to_string(),
        to_qualified_name: qn,
    });
}

/// Emits a `Method` + `HasMethod` (receiver = the class QN), `is_async`
/// property, member visibility, and scans its body for calls. A getter and
/// setter of the same name produce TWO methods on the SAME QN (no dedup).
fn emit_method(spec: &LangSpec, tf: &TsFamilySpec, ctx: &mut WalkCtx, node: Node, class_qn: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = spec.conventions.def_qn(class_qn, &name, 0);
    let body = spec.body_field.and_then(|f| node.child_by_field_name(f));
    let mut properties = vec![
        (
            "is_async".to_string(),
            has_async(ctx.source, node).to_string(),
        ),
        ("receiver_type".to_string(), class_qn.to_string()),
    ];
    // Return type + constructed types (issue #92).
    type_uses::append_type_use_props(spec, ctx, node, body, &mut properties);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: member_visibility(tf, ctx.source, node),
        properties,
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: class_qn.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = body {
        ts_walk_calls(spec, ctx, body, &qn);
    }
}

/// Emits a `Field` + `HasField` for a `public_field_definition`, with its
/// `type_annotation` (the bare type text, leading `: ` stripped — issue #140)
/// and member visibility.
fn emit_field(spec: &LangSpec, tf: &TsFamilySpec, ctx: &mut WalkCtx, node: Node, class_qn: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let type_ann = ts_type_annotation(ctx.source, node, spec.type_field);
    let fqn = qual(class_qn, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_FIELD.to_string(),
        name,
        qualified_name: fqn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: member_visibility(tf, ctx.source, node),
        properties: vec![("type_annotation".to_string(), type_ann)],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasField".to_string(),
        from_qualified_name: class_qn.to_string(),
        to_qualified_name: fqn,
    });
}

/// Scans `root` for `call_expression` nodes (stack DFS), emitting per accepted
/// call a `CallSite` node plus TWO refs: `Defines`(caller → call-site QN) and
/// the callee edge from `call_entry` (`Calls`(caller → callee tail)). The
/// callee decision and shaping come from `TsConventions`.
pub(super) fn ts_walk_calls(spec: &LangSpec, ctx: &mut WalkCtx, root: Node, caller_qn: &str) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if kind_in(spec.call_node_kinds, n.kind()) {
            if let Some(callee) = spec.conventions.call_callee(ctx.source, n) {
                let entry = spec
                    .conventions
                    .call_entry(ctx.source, n, caller_qn, &callee, 0);
                ctx.nodes.push(ExtractedNode {
                    label: LABEL_CALL_SITE.to_string(),
                    name: entry.name,
                    qualified_name: entry.qualified_name.clone(),
                    start_line: entry.start_line,
                    end_line: entry.end_line,
                    visibility: entry.visibility,
                    properties: entry.properties,
                });
                ctx.refs.push(ExtractedRef {
                    kind: "Defines".to_string(),
                    from_qualified_name: caller_qn.to_string(),
                    to_qualified_name: entry.qualified_name,
                });
                ctx.refs.push(ExtractedRef {
                    kind: entry.ref_kind.to_string(),
                    from_qualified_name: caller_qn.to_string(),
                    to_qualified_name: entry.ref_to,
                });
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}
