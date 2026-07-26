// parser::spec::walkers — the generic tree-sitter walkers (ADR-0055 §1).
//
// `walk_defs` / `walk_calls` / `walk_imports` / `walk_embedded` consume a
// `&LangSpec` (node-kind data) and a `&dyn LanguageConventions` (behavior) and
// produce the EXISTING, unchanged `ParseResult` / `ExtractedNode` /
// `ExtractedRef` contract. One implementation each, replacing the per-language
// hand-written walkers one language at a time behind the accuracy gate.
//
// The traversal deliberately mirrors the hand-written Go walker it replaces
// (top-level dispatch in source order; stack-based DFS for imports / values /
// calls) so the migration is provable at *exact* parity, node-for-node and
// ref-for-ref, before the old walker is deleted (ADR-0055 §5, step 3).

use tree_sitter::{Node, Parser};

use super::lang_spec::LangSpec;
use crate::parser::{
    collect_error_ranges, count_parse_errors, node_field_text, node_text, parse_with_timeout, qual,
    ExtractedNode, ExtractedRef, ParseResult, LABEL_CALL_SITE, LABEL_CONSTANT, LABEL_FIELD,
    LABEL_FUNCTION, LABEL_IMPORT, LABEL_METHOD, LABEL_STRUCT, LABEL_TRAIT, LABEL_TYPE_ALIAS,
};

/// Mutable state threaded through a single file's walk. `next_seq` is the
/// per-file monotonic counter the conventions use to disambiguate overloads
/// (Go's `#seq` suffix) and to key call sites.
pub(crate) struct WalkCtx<'a> {
    source: &'a str,
    nodes: Vec<ExtractedNode>,
    refs: Vec<ExtractedRef>,
    next_seq: u64,
}

impl<'a> WalkCtx<'a> {
    fn next_seq(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }
}

/// Parses `source` with `spec`'s grammar and extracts the uniform
/// `ParseResult`. This is the table-driven replacement for a language's
/// hand-written `parse_<lang>_file` entry point.
///
/// Preconditions: `spec.ts_language` is the grammar matching `source`'s
/// language; `file_path` is the file's repo-relative id (the top scope).
/// Postconditions: returns `Ok(ParseResult)` whose `nodes`/`refs` are exactly
/// what the generic walkers emit for `source`, plus the shared parse-error
/// signals; `Err` only on grammar-set or parse-timeout failure. Invariant:
/// the public `ExtractedNode`/`ExtractedRef` contract is unchanged.
pub(crate) fn parse_with_spec(
    spec: &LangSpec,
    source: &str,
    file_path: &str,
) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = (spec.ts_language)();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set {:?} language: {e}", spec.language))?;
    let tree = parse_with_timeout(&mut parser, source)?;

    let mut ctx = WalkCtx {
        source,
        nodes: Vec::new(),
        refs: Vec::new(),
        next_seq: 0,
    };
    walk_defs(spec, &mut ctx, tree.root_node(), file_path);
    Ok(ParseResult {
        nodes: ctx.nodes,
        refs: ctx.refs,
        parse_errors: count_parse_errors(tree.root_node()),
        error_ranges: collect_error_ranges(tree.root_node()),
    })
}

fn kind_in(kinds: &[&str], k: &str) -> bool {
    kinds.contains(&k)
}

fn line_of(node: Node) -> u64 {
    node.start_position().row as u64 + 1
}

fn end_line_of(node: Node) -> u64 {
    node.end_position().row as u64 + 1
}

/// Top-level definition walker: dispatches each child of `parent` to the
/// concern its node kind names in `spec`, then re-parses any embedded regions.
pub(crate) fn walk_defs(spec: &LangSpec, ctx: &mut WalkCtx, parent: Node, scope: &str) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        let k = child.kind();
        if kind_in(spec.skip_node_kinds, k) {
            continue;
        } else if kind_in(spec.import_node_kinds, k) {
            walk_imports(spec, ctx, child, scope);
        } else if kind_in(spec.type_decl_node_kinds, k) {
            walk_type_decl(spec, ctx, child, scope);
        } else if kind_in(spec.function_node_kinds, k) {
            emit_function(spec, ctx, child, scope);
        } else if kind_in(spec.method_node_kinds, k) {
            emit_method(spec, ctx, child, scope);
        } else if kind_in(spec.value_decl_node_kinds, k) {
            walk_value_decl(spec, ctx, child, scope);
        }
    }
    walk_embedded(spec, ctx, parent, scope);
}

fn emit_function(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
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
        properties: spec.conventions.function_props(ctx.source, node),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node.child_by_field_name(spec.body_field) {
        walk_calls(spec, ctx, body, &qn);
    }
}

fn emit_method(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let receiver_text = node_field_text(ctx.source, node, spec.receiver_field);
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
        visibility: spec.conventions.visibility_of(&name),
        properties: vec![("receiver_type".to_string(), recv_type)],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: scope_qn.clone(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node.child_by_field_name(spec.body_field) {
        walk_calls(spec, ctx, body, &qn);
    }
}

fn walk_type_decl(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if kind_in(spec.type_spec_node_kinds, child.kind()) {
            emit_type_spec(spec, ctx, child, scope);
        }
    }
}

fn emit_type_spec(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let mut label = LABEL_TYPE_ALIAS;
    let mut struct_type: Option<Node> = None;
    if let Some(ty) = node.child_by_field_name(spec.type_field) {
        if spec.struct_type_kind == Some(ty.kind()) {
            label = LABEL_STRUCT;
            struct_type = Some(ty);
        } else if spec.interface_type_kind == Some(ty.kind()) {
            label = LABEL_TRAIT;
        }
    }
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
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
    if let Some(st) = struct_type {
        walk_fields(spec, ctx, st, &qn);
    }
}

/// Emits one `Field` node + `HasField` edge per declared name inside a struct.
/// A field declaration may declare several names (`X, Y int`), so every child
/// carrying the `name` field is emitted — matching the grammar's `multiple`
/// name field.
fn walk_fields(spec: &LangSpec, ctx: &mut WalkCtx, container_parent: Node, owner_qn: &str) {
    let mut c1 = container_parent.walk();
    for container in container_parent.children(&mut c1) {
        if !kind_in(spec.field_container_kinds, container.kind()) {
            continue;
        }
        let mut c2 = container.walk();
        for fd in container.children(&mut c2) {
            if !kind_in(spec.field_node_kinds, fd.kind()) {
                continue;
            }
            let type_text = node_field_text(ctx.source, fd, spec.type_field);
            let mut names: Vec<Node> = Vec::new();
            let mut c3 = fd.walk();
            if c3.goto_first_child() {
                loop {
                    if c3.field_name() == Some(spec.name_field) {
                        names.push(c3.node());
                    }
                    if !c3.goto_next_sibling() {
                        break;
                    }
                }
            }
            for name_node in names {
                let fname = node_text(ctx.source, name_node);
                if fname.is_empty() {
                    continue;
                }
                let fqn = qual(owner_qn, &fname);
                let mut props = Vec::new();
                if !type_text.is_empty() {
                    props.push(("type_annotation".to_string(), type_text.clone()));
                }
                ctx.nodes.push(ExtractedNode {
                    label: LABEL_FIELD.to_string(),
                    name: fname.clone(),
                    qualified_name: fqn.clone(),
                    start_line: line_of(fd),
                    end_line: end_line_of(fd),
                    visibility: spec.conventions.visibility_of(&fname),
                    properties: props,
                });
                ctx.refs.push(ExtractedRef {
                    kind: "HasField".to_string(),
                    from_qualified_name: owner_qn.to_string(),
                    to_qualified_name: fqn,
                });
            }
        }
    }
}

/// Emits one `Constant` node + `Defines` edge per value-declaration name the
/// conventions accept (`is_constant_name`). Stack DFS mirrors the hand-written
/// walker so grouped `const ( ... )` blocks are fully descended.
fn walk_value_decl(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if kind_in(spec.value_spec_node_kinds, n.kind()) {
            let mut cursor = n.walk();
            for child in n.children(&mut cursor) {
                if child.kind() != spec.value_name_kind {
                    continue;
                }
                let name = node_text(ctx.source, child);
                // mutation note (§12): the `||` → `&&` mutant here is EQUIVALENT
                // under Go — `value_name_kind` (`identifier`) nodes are never
                // empty and Go's `is_constant_name` is constant-true, so both
                // operands are always false and the guard never fires either
                // way. The guard becomes observable when a language whose
                // `is_constant_name` can return false migrates (e.g. Python's
                // UPPER_SNAKE filter), whose fixtures will then kill the mutant.
                if name.is_empty() || !spec.conventions.is_constant_name(&name) {
                    continue;
                }
                let qn = qual(scope, &name);
                ctx.nodes.push(ExtractedNode {
                    label: LABEL_CONSTANT.to_string(),
                    name: name.clone(),
                    qualified_name: qn.clone(),
                    start_line: line_of(child),
                    end_line: end_line_of(child),
                    visibility: spec.conventions.visibility_of(&name),
                    properties: Vec::new(),
                });
                ctx.refs.push(ExtractedRef {
                    kind: "Defines".to_string(),
                    from_qualified_name: scope.to_string(),
                    to_qualified_name: qn,
                });
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

/// Import walker: descends `import_node` for each import spec and emits the
/// `Import` node + import edge the conventions shape. Stack DFS matches the
/// hand-written walker (single `import "x"` and grouped `import ( ... )`).
pub(crate) fn walk_imports(spec: &LangSpec, ctx: &mut WalkCtx, import_node: Node, scope: &str) {
    let mut stack = vec![import_node];
    while let Some(n) = stack.pop() {
        if kind_in(spec.import_spec_kinds, n.kind()) {
            if let Some(entry) = spec.conventions.import_entry(ctx.source, spec, n, scope) {
                ctx.nodes.push(ExtractedNode {
                    label: LABEL_IMPORT.to_string(),
                    name: entry.display_name,
                    qualified_name: entry.qualified_name,
                    start_line: line_of(n),
                    end_line: end_line_of(n),
                    visibility: entry.visibility,
                    properties: entry.properties,
                });
                ctx.refs.push(ExtractedRef {
                    kind: spec.conventions.import_ref_kind().to_string(),
                    from_qualified_name: scope.to_string(),
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

/// Call walker: emits one `CallSite` node + `Calls` edge per call expression
/// the conventions accept. Stack DFS matches the hand-written walker so the
/// `seq` counter (which keys call-site QNs) is assigned in identical order.
pub(crate) fn walk_calls(spec: &LangSpec, ctx: &mut WalkCtx, root: Node, caller_qn: &str) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if kind_in(spec.call_node_kinds, n.kind()) {
            if let Some(callee) = spec.conventions.call_callee(ctx.source, n) {
                let seq = ctx.next_seq();
                let site_qn = spec.conventions.call_site_qn(
                    caller_qn,
                    n.start_position().row + 1,
                    n.start_position().column + 1,
                    seq,
                );
                ctx.nodes.push(ExtractedNode {
                    label: LABEL_CALL_SITE.to_string(),
                    name: callee.clone(),
                    qualified_name: site_qn,
                    start_line: line_of(n),
                    end_line: end_line_of(n),
                    visibility: "public".to_string(),
                    properties: vec![("callee_name".to_string(), callee.clone())],
                });
                ctx.refs.push(ExtractedRef {
                    kind: "Calls".to_string(),
                    from_qualified_name: caller_qn.to_string(),
                    to_qualified_name: callee,
                });
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

/// Embedded-language walker: for each embedded rule, locate every script node
/// under `parent`, re-parse its content child with the embedded language's
/// grammar, and run the generic def walker on the inner tree under `scope`.
/// Empty `spec.embedded` (all ten core languages) makes this a no-op.
pub(crate) fn walk_embedded(spec: &LangSpec, ctx: &mut WalkCtx, parent: Node, scope: &str) {
    if spec.embedded.is_empty() {
        return;
    }
    for emb in spec.embedded {
        let inner_spec = match super::registry::lang_spec(emb.embedded_language) {
            Some(s) => s,
            None => continue,
        };
        let mut stack = vec![parent];
        while let Some(n) = stack.pop() {
            if n.kind() == emb.script_node_kind {
                if let Some(content) = find_child_of_kind(n, emb.content_node_kind) {
                    reparse_embedded(inner_spec, ctx, content, scope);
                }
            }
            let mut cursor = n.walk();
            for c in n.children(&mut cursor) {
                stack.push(c);
            }
        }
    }
}

fn find_child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// Re-parses the byte slice of `content` with `inner_spec`'s grammar and walks
/// the inner tree. The inner nodes/refs are appended to the same `WalkCtx`, so
/// embedded symbols land in the host file's `ParseResult` under `scope`.
fn reparse_embedded(inner_spec: &LangSpec, ctx: &mut WalkCtx, content: Node, scope: &str) {
    let text = node_text(ctx.source, content);
    let lang: tree_sitter::Language = (inner_spec.ts_language)();
    let mut parser = Parser::new();
    if parser.set_language(&lang).is_err() {
        return;
    }
    let tree = match parse_with_timeout(&mut parser, &text) {
        Ok(t) => t,
        Err(_) => return,
    };
    // Inner walk runs on a scoped sub-context so the embedded source's byte
    // offsets resolve against `text`, then its output is merged into `ctx`.
    let mut inner = WalkCtx {
        source: &text,
        nodes: Vec::new(),
        refs: Vec::new(),
        next_seq: ctx.next_seq,
    };
    walk_defs(inner_spec, &mut inner, tree.root_node(), scope);
    ctx.next_seq = inner.next_seq;
    ctx.nodes.append(&mut inner.nodes);
    ctx.refs.append(&mut inner.refs);
}
