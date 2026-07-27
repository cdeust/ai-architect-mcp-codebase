// parser::spec::walkers::objc — the Objective-C definition walker
// (ADR-0055 phase 8). Reproduces the pre-migration hand-written ObjC walker
// (`parser::objc::extract`) at EXACT parity, driven by the `ObjcFamilySpec`
// sub-table instead of hardcoded `TS_*` constants.
//
// Objective-C fits none of the other lanes: it is a C superset (plain C
// structs/unions/enums/typedefs/functions live in a `.m`/`.h` file) carrying an
// ObjC object model whose shapes match neither the flat `clike` walker nor the
// C++ hybrid. This module holds the object model (classes, protocols, methods,
// functions, dispatch); the C-superset half (structs/enums/typedefs) lives in
// the `c_constructs` submodule (split along the §4.1 concern boundary). Its
// distinctive behaviors, all preserved here:
//   - `@interface`/`@implementation` → a `Struct` keyed by NAME, so the
//     interface, its implementation, and a category all collapse onto ONE QN
//     (`{file}::{Class}`). A category is the same node carrying a `category`
//     field (→ `is_category=true` + `category` props). The `superclass` field
//     → one `Extends`; protocol conformance (`<Drawable>`) is NOT emitted.
//   - `@protocol` → a `Trait` + `Defines`, with NO member extraction (the
//     hand-written walker never walked a protocol's method declarations).
//   - A method name is its selector's FIRST keyword only (`doWith`, not
//     `doWith:andThen:` — the grammar shape + issue #128), keyed
//     `{class}::{selector}#{seq}` with `receiver_type` = the class QN.
//   - A message send `[obj do:x]` is a `Calls` edge whose callee is the
//     reconstructed selector; a plain `call_expression` splits its callee on
//     `.` only (NOT the C/C++ `['.', '>', ':']`) — both route through the shared
//     `calls::walk_calls` via `ObjcConventions::call_callee`.
//
// A single per-file `seq` (WalkCtx::next_seq) keys methods, functions, AND call
// sites in one DFS order, so `walk_defs` delegates the whole file here whenever
// a `LangSpec` carries `objc_family: Some(_)`.

mod c_constructs;

use tree_sitter::Node;

use super::super::lang_spec::{LangSpec, ObjcFamilySpec};
use super::{calls, end_line_of, imports, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_FUNCTION, LABEL_METHOD,
    LABEL_STRUCT, LABEL_TRAIT,
};

/// Top-level ObjC definition walker: dispatches each child of `parent` to the
/// concern its node kind names in `of`, recursing transparently through any
/// unmatched wrapper with named children (matching the hand-written
/// `extract_top`'s `_ =>` arm). The scope is unchanged across the recursion —
/// ObjC top-level declarations are all file-scoped.
pub(super) fn walk_objc_defs(
    spec: &LangSpec,
    of: &ObjcFamilySpec,
    ctx: &mut WalkCtx,
    parent: Node,
    scope: &str,
) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        let k = child.kind();
        if kind_in(of.class_kinds, k) {
            emit_class(spec, of, ctx, child, scope);
        } else if kind_in(of.protocol_kinds, k) {
            emit_protocol(spec, of, ctx, child, scope);
        } else if kind_in(of.func_def_kinds, k) {
            emit_function(spec, of, ctx, child, scope);
        } else if kind_in(of.struct_kinds, k) {
            c_constructs::emit_c_struct(spec, of, ctx, child, scope);
        } else if kind_in(of.enum_kinds, k) {
            c_constructs::emit_c_enum(spec, of, ctx, child, scope);
        } else if kind_in(of.typedef_kinds, k) {
            c_constructs::emit_c_typedef(spec, of, ctx, child, scope);
        } else if kind_in(spec.import_node_kinds, k) {
            // `preproc_include` (`#import`/`#include`) and `module_import`
            // (`@import`); the conventions shape each into an `Import`.
            imports::walk_imports(spec, ctx, child, scope);
        } else if child.named_child_count() > 0 {
            // Transparent recursion into an unmatched wrapper with named children
            // (the hand-written `_ =>` arm). Same scope.
            // mutation note (§12): the `> 0` → `>= 0` mutant is EQUIVALENT —
            // recursing into a childless node emits nothing (the child loop
            // iterates zero times), so no test can observe a difference. Kept a
            // faithful copy of the old walker's guard and a cheap leaf skip.
            walk_objc_defs(spec, of, ctx, child, scope);
        }
    }
}

/// The class/protocol name: the `name_field` text, or — when absent — the first
/// direct child whose kind is in `identifier_kinds` (`identifier`/
/// `type_identifier`). Matches the hand-written `find_name`.
fn find_name(spec: &LangSpec, of: &ObjcFamilySpec, source: &str, node: Node) -> String {
    let n = node_field_text(source, node, spec.name_field);
    if !n.is_empty() {
        return n;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if kind_in(of.identifier_kinds, c.kind()) {
            return node_text(source, c);
        }
    }
    String::new()
}

/// Emits an `@interface`/`@implementation`/category as a `Struct` keyed by NAME
/// (so all three collapse onto one QN), records `is_category`/`category` props
/// when the `category_field` is present, emits one `Extends` per `superclass_field`,
/// then walks the class for its method members (direct children plus one level
/// of grouping). Matches the hand-written `extract_class`.
fn emit_class(spec: &LangSpec, of: &ObjcFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = find_name(spec, of, ctx.source, node);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let mut props = Vec::new();
    if node.child_by_field_name(of.category_field).is_some() {
        props.push(("is_category".to_string(), "true".to_string()));
        let cat = node_field_text(ctx.source, node, of.category_field);
        if !cat.is_empty() {
            props.push(("category".to_string(), cat));
        }
    }
    ctx.nodes.push(ExtractedNode {
        label: LABEL_STRUCT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: props,
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    let superclass = node_field_text(ctx.source, node, of.superclass_field);
    if !superclass.is_empty() {
        ctx.refs.push(ExtractedRef {
            kind: "Extends".to_string(),
            from_qualified_name: qn.clone(),
            to_qualified_name: superclass,
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if kind_in(of.method_kinds, child.kind()) {
            emit_method(spec, of, ctx, child, &qn);
        } else if child.named_child_count() > 0 {
            // Dive one level into compound groupings (a class body wrapper).
            // mutation note (§12): the `> 0` → `>= 0` mutant is EQUIVALENT —
            // a childless grouping's inner loop iterates zero times, emitting
            // nothing. Kept as the faithful copy of the old walker's guard.
            let mut inner = child.walk();
            for gc in child.children(&mut inner) {
                if kind_in(of.method_kinds, gc.kind()) {
                    emit_method(spec, of, ctx, gc, &qn);
                }
            }
        }
    }
}

/// Emits a `@protocol` as a `Trait` + `Defines`, with NO member extraction.
/// Matches the hand-written `extract_protocol`.
fn emit_protocol(spec: &LangSpec, of: &ObjcFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = find_name(spec, of, ctx.source, node);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_TRAIT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

/// The method's FULL selector name (issue #128).
///
/// tree-sitter-objc 3.0.2 shapes `- (int)areaWithWidth:(int)w height:(int)h;` as
/// the return `method_type`, then the selector keywords as bare
/// `plain_identifier_kind` (`identifier`) children INTERLEAVED with
/// `method_parameter_kinds` (`method_parameter`) nodes — there is NO
/// `keyword_declarator` in this grammar (verified against the AST). So the
/// selector is reconstructed exactly as `message_selector` does for a message
/// SEND: collect the keyword identifiers in order, and if there is at least one
/// argument (`method_parameter`), join them each with a trailing `:`
/// (`areaWithWidth:height:`, `shapeNamed:`); with zero arguments it is a unary
/// selector and the sole keyword stands bare (`start`).
///
/// The pre-#128 walker took only the FIRST identifier, so a keyword selector
/// resolved to its first keyword (`areaWithWidth`) and a single-keyword selector
/// dropped its colon (`shapeNamed`) — asymmetric with sends, which already
/// reconstruct the full selector. This makes declaration and send agree.
///
/// Preconditions: `node`'s kind is in `of.method_kinds`. Postconditions: the full
/// selector, or `""` when the node has no selector keyword.
fn method_selector(of: &ObjcFamilySpec, source: &str, node: Node) -> String {
    let mut keywords: Vec<String> = Vec::new();
    let mut arg_count: usize = 0;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let k = child.kind();
        if k == of.plain_identifier_kind {
            keywords.push(node_text(source, child));
        } else if kind_in(of.method_parameter_kinds, k) {
            arg_count += 1;
        }
    }
    if keywords.is_empty() {
        return String::new();
    }
    if arg_count > 0 {
        keywords.iter().map(|k| format!("{k}:")).collect::<String>()
    } else {
        keywords.join("")
    }
}

/// Emits a method (`Method` + `HasMethod`, `receiver_type` = the class QN),
/// keyed `{scope}::{selector}#{seq}`, scanning its body for calls. Matches the
/// hand-written `extract_method`.
fn emit_method(spec: &LangSpec, of: &ObjcFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let sel = method_selector(of, ctx.source, node);
    if sel.is_empty() {
        return;
    }
    let seq = ctx.next_seq();
    let qn = spec.conventions.def_qn(scope, &sel, seq);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: sel.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&sel),
        properties: vec![("receiver_type".to_string(), scope.to_string())],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = method_body(spec, of, node) {
        calls::walk_calls(spec, ctx, body, &qn);
    }
}

/// Emits a top-level C function (`Function` + `Defines`, `{scope}::{name}#{seq}`),
/// scanning its body for calls. The name is the declarator's `declarator` field
/// text (the function name), falling back to `find_name`. Matches the
/// hand-written `extract_function` (top-level path; ObjC never emits a function
/// as a method — methods use the selector path).
fn emit_function(spec: &LangSpec, of: &ObjcFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node
        .child_by_field_name(of.declarator_field)
        .map(|d| node_field_text(ctx.source, d, of.declarator_field))
        .unwrap_or_default();
    let name = if name.is_empty() {
        find_name(spec, of, ctx.source, node)
    } else {
        name
    };
    if name.is_empty() {
        return;
    }
    let seq = ctx.next_seq();
    let qn = spec.conventions.def_qn(scope, &name, seq);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_FUNCTION.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = method_body(spec, of, node) {
        calls::walk_calls(spec, ctx, body, &qn);
    }
}

/// The body of a method/function to scan for calls: the `body_field` child, or
/// the first `func_body_kinds` (`compound_statement`) child. Matches the
/// hand-written `child_by_field_name("body").or_else(compound_statement)`.
fn method_body<'t>(spec: &LangSpec, of: &ObjcFamilySpec, node: Node<'t>) -> Option<Node<'t>> {
    if let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) {
        return Some(body);
    }
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| kind_in(of.func_body_kinds, c.kind()));
    found
}
