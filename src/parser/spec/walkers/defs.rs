// parser::spec::walkers::defs — the top-level definition dispatcher and the
// function/method/class emitters it drives.
//
// Split out of `walkers/mod.rs` (issue #101, §4.1) as a pure move; the
// `ExtractedNode`/`ExtractedRef` contract is unchanged. `walk_defs` is the
// generic entry the per-concern arms (imports/calls/embedded/types/constants)
// hang off, keeping those arms isolated from this file's emit plumbing.

use tree_sitter::Node;

use super::super::lang_spec::LangSpec;
use super::{
    call_scan_of, calls, class_body_of, constants, embedded, end_line_of, imports, kind_in,
    line_of, types, WalkCtx,
};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_ENUM, LABEL_FUNCTION,
    LABEL_METHOD, LABEL_STRUCT, LABEL_TRAIT,
};

/// Top-level definition walker: dispatches each child of `parent` to the
/// concern its node kind names in `spec`, then re-parses any embedded regions.
/// `enclosing_class` is `Some(class_qn)` while walking a class body — a
/// free-function node inside a class body is emitted as a method (Python), and
/// module-level value declarations are skipped inside a class.
pub(crate) fn walk_defs(
    spec: &LangSpec,
    ctx: &mut WalkCtx,
    parent: Node,
    scope: &str,
    enclosing_class: Option<&str>,
) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        let k = child.kind();
        if kind_in(spec.skip_node_kinds, k) {
            continue;
        } else if kind_in(spec.import_node_kinds, k) {
            imports::walk_imports(spec, ctx, child, scope);
        } else if let Some(label) = class_like_label(spec, k) {
            emit_class(spec, ctx, child, scope, label);
        } else if kind_in(spec.decorated_def_kinds, k) {
            emit_decorated(spec, ctx, child, scope, enclosing_class);
        } else if kind_in(spec.type_decl_node_kinds, k) {
            types::walk_type_decl(spec, ctx, child, scope);
        } else if kind_in(spec.function_node_kinds, k) {
            emit_def(spec, ctx, child, scope, enclosing_class, &[]);
        } else if kind_in(spec.method_node_kinds, k) {
            emit_method_recv(spec, ctx, child, scope);
        } else if kind_in(spec.variant_node_kinds, k) {
            constants::emit_variant(spec, ctx, child, scope);
        } else if kind_in(spec.member_constant_kinds, k) {
            constants::emit_member_constant(spec, ctx, child, scope);
        } else if kind_in(spec.variable_field_kinds, k) {
            constants::emit_variable_fields(spec, ctx, child, scope);
        } else if kind_in(spec.body_wrapper_kinds, k) {
            // A grammar wrapper around further members (Java's
            // `enum_body_declarations`): recurse transparently, same scope and
            // enclosing type, emitting no node of its own.
            walk_defs(spec, ctx, child, scope, enclosing_class);
        } else if kind_in(spec.value_decl_node_kinds, k) && enclosing_class.is_none() {
            constants::walk_value_decl(spec, ctx, child, scope);
        }
    }
    embedded::walk_embedded(spec, ctx, parent, scope);
}

/// Maps a class-like node kind to the label it emits, or `None` if the kind is
/// not a class-like. All three share the same emitter (`emit_class`:
/// inheritance + body recursion); only the label differs. Struct/Trait/Enum
/// are disjoint slices, so at most one arm matches.
fn class_like_label(spec: &LangSpec, k: &str) -> Option<&'static str> {
    if kind_in(spec.class_node_kinds, k) {
        Some(LABEL_STRUCT)
    } else if kind_in(spec.interface_node_kinds, k) {
        Some(LABEL_TRAIT)
    } else if kind_in(spec.enum_node_kinds, k) {
        Some(LABEL_ENUM)
    } else {
        None
    }
}

/// Emits a free function (`Function` + `Defines`) or, inside a class body, a
/// method (`Method` + `HasMethod` with the class as receiver). `decorators`
/// are the already-stripped decorator names from an enclosing decorated def.
fn emit_def(
    spec: &LangSpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    enclosing_class: Option<&str>,
    decorators: &[String],
) {
    let name = spec.conventions.def_name(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let start_line = line_of(node);
    let seq = ctx.next_seq();
    let base_qn = spec.conventions.def_qn(scope, &name, seq);
    let qn = ctx.dedup(base_qn, start_line);

    let mut props = spec.conventions.function_props(ctx.source, node);
    if !decorators.is_empty() {
        props.push(("decorators".to_string(), decorators.join(",")));
    }

    match enclosing_class {
        Some(class_qn) => {
            props.push(("receiver_type".to_string(), class_qn.to_string()));
            ctx.nodes.push(ExtractedNode {
                label: LABEL_METHOD.to_string(),
                name: name.clone(),
                qualified_name: qn.clone(),
                start_line,
                end_line: end_line_of(node),
                visibility: spec.conventions.node_visibility(ctx.source, node, &name),
                properties: props,
            });
            ctx.refs.push(ExtractedRef {
                kind: "HasMethod".to_string(),
                from_qualified_name: scope.to_string(),
                to_qualified_name: qn.clone(),
            });
        }
        None => {
            ctx.nodes.push(ExtractedNode {
                label: LABEL_FUNCTION.to_string(),
                name: name.clone(),
                qualified_name: qn.clone(),
                start_line,
                end_line: end_line_of(node),
                visibility: spec.conventions.node_visibility(ctx.source, node, &name),
                properties: props,
            });
            ctx.refs.push(ExtractedRef {
                kind: "Defines".to_string(),
                from_qualified_name: scope.to_string(),
                to_qualified_name: qn.clone(),
            });
        }
    }

    if let Some(body) = call_scan_of(spec, node) {
        calls::walk_calls(spec, ctx, body, &qn);
    }
}

/// Emits a receiver-scoped method (`Method` + `HasMethod`) for languages whose
/// methods are a distinct node kind carrying a receiver field (Go). The
/// receiver type is parsed from `spec.receiver_field` and scopes the method's
/// QN under `scope::RecvType`.
fn emit_method_recv(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let receiver_text = spec
        .receiver_field
        .map(|rf| node_field_text(ctx.source, node, rf))
        .unwrap_or_default();
    let recv_type = spec.conventions.receiver_type(&receiver_text);
    let scope_qn = if recv_type.is_empty() {
        scope.to_string()
    } else {
        qual(scope, &recv_type)
    };
    let seq = ctx.next_seq();
    let qn = spec.conventions.def_qn(&scope_qn, &name, seq);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.node_visibility(ctx.source, node, &name),
        properties: vec![("receiver_type".to_string(), recv_type)],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: scope_qn.clone(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = call_scan_of(spec, node) {
        calls::walk_calls(spec, ctx, body, &qn);
    }
}

/// Emits a class-like declaration (`label` + `Defines`), its inheritance
/// properties and edges (via the conventions — `bases`/`Extends` for Python,
/// plus `implements`/`Implements` for Java), then recurses into its body with
/// the class as the enclosing scope so member functions become methods.
/// `label` is `Struct`/`Trait`/`Enum` as selected by `class_like_label`.
fn emit_class(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str, label: &'static str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    // Grammars that use one node kind (`class_declaration`) for struct/trait/enum
    // (Kotlin) refine the label by content here; distinct-kind grammars
    // (Go/Python/Java) keep the label `class_like_label` already chose.
    let label = spec.conventions.refine_class_label(ctx.source, node, label);
    let qn = qual(scope, &name);
    let inheritance = spec.conventions.class_inheritance(ctx.source, spec, node);
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.node_visibility(ctx.source, node, &name),
        properties: inheritance.properties,
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    for (kind, to) in inheritance.refs {
        ctx.refs.push(ExtractedRef {
            kind: kind.to_string(),
            from_qualified_name: qn.clone(),
            to_qualified_name: to,
        });
    }
    if let Some(body) = class_body_of(spec, node) {
        walk_defs(spec, ctx, body, &qn, Some(&qn));
    }
}

/// Unwraps a decorated definition: collects the decorator names (stripped of
/// the leading `@`), then dispatches the inner class or function. A decorated
/// class is extracted as a plain class (decorators dropped, matching the
/// pre-migration walker); a decorated function carries its decorators as a
/// property.
fn emit_decorated(
    spec: &LangSpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    enclosing_class: Option<&str>,
) {
    let mut decorators: Vec<String> = Vec::new();
    let mut definition: Option<Node> = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let k = child.kind();
        if spec.decorator_node_kind == Some(k) {
            let text = node_text(ctx.source, child);
            decorators.push(text.trim_start_matches('@').trim().to_string());
        } else if kind_in(spec.class_node_kinds, k) {
            // A decorated class is a plain `Struct` (decorators dropped,
            // matching the pre-migration walker). Python is the only decorated
            // language; its decorated defs are always `class_definition`.
            emit_class(spec, ctx, child, scope, LABEL_STRUCT);
            return;
        } else if kind_in(spec.function_node_kinds, k) {
            definition = Some(child);
        }
    }
    if let Some(func_node) = definition {
        emit_def(spec, ctx, func_node, scope, enclosing_class, &decorators);
    }
}
