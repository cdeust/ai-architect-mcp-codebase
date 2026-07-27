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

use super::super::family_specs::ObjcFamilySpec;
use super::super::lang_spec::LangSpec;
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
            c_constructs::emit_c_struct(spec, of, ctx, child, scope, None);
        } else if kind_in(of.enum_kinds, k) {
            c_constructs::emit_c_enum(spec, of, ctx, child, scope, None);
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

/// The method's FULL selector (issue #128): every selector keyword, each followed
/// by `:` when it takes an argument.
///
/// Preconditions: `node` is a `method_kinds` declaration/definition.
/// Postconditions: `"draw"` for a unary selector, `"setX:y:z:"` for a keyword
/// selector, `""` when no selector keyword is present. The result is the ObjC
/// selector as the language spells it, so it matches what a message SEND resolves
/// to — the asymmetry #128 reports.
///
/// The shape, dumped from tree-sitter-objc 3.0.2 rather than inferred:
/// `- (void)setX:(int)x y:(int)y;` is a `method_declaration` whose DIRECT children
/// are, in order, `method_type`, `identifier("setX")`, `method_parameter(":(int)x")`,
/// `identifier("y")`, `method_parameter(":(int)y")`. So each selector keyword is a
/// bare `identifier` child, and it takes an argument exactly when the NEXT direct
/// child is a `method_parameter`. `- (void)draw;` has the single `identifier`
/// child and no `method_parameter`, hence no colon.
///
/// Only DIRECT children are considered: a `method_parameter` contains its own
/// `identifier` (the parameter's name, `x`), which is not part of the selector.
///
/// The pre-#128 code took the FIRST bare identifier and stopped, yielding `setX`.
/// The grammar also declares a `keyword_declarator` node, but this version does
/// NOT produce one for either form (verified on both a declaration and a
/// definition), so no branch is written for it (§9: no unreachable code).
/// source: tree-sitter-objc 3.0.2 src/node-types.json + the parsed AST.
fn method_selector(of: &ObjcFamilySpec, source: &str, node: Node) -> String {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    let mut selector = String::new();
    for (i, child) in children.iter().enumerate() {
        if child.kind() != of.plain_identifier_kind {
            continue;
        }
        selector.push_str(&node_text(source, *child));
        let takes_argument = children
            .get(i + 1)
            .is_some_and(|next| kind_in(of.method_parameter_kinds, next.kind()));
        if takes_argument {
            selector.push(':');
        }
    }
    selector
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
