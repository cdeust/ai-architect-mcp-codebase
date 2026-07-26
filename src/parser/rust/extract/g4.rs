// parser::rust::extract::g4 — see ../extract/mod.rs.

use super::super::*; // parent module: Ctx, TS_* consts, kept helpers
use super::*;
use crate::parser::*; // ExtractedNode, ExtractedRef, node_text, qual, LABEL_*, …
use tree_sitter::Node; // sibling extract fns (glob re-export)

// ---------------------------------------------------------------------------
// Module extraction
// ---------------------------------------------------------------------------

pub(super) fn extract_mod(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_MODULE.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: extract_visibility(ctx.source, node),
        properties: vec![],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node.child_by_field_name("body") {
        if body.kind() == TS_DECL_LIST {
            extract_top_level(ctx, body, &qn);
        }
    }
}

// ---------------------------------------------------------------------------
// Call-site extraction
// ---------------------------------------------------------------------------

pub(super) fn extract_call_sites(ctx: &mut ExtractCtx, body: Node, caller_qn: &str) {
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        match node.kind() {
            TS_CALL_EXPR => extract_single_call_site(ctx, node, caller_qn),
            TS_MACRO_INVOCATION => extract_macro_call_site(ctx, node, caller_qn),
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

/// Emit a CallSite node for a `name!(...)` macro invocation. The callee_name
/// is stored with a trailing `!` so the resolver's Layer 4 pass can cheaply
/// distinguish macros from regular function calls. Q8 (Defines_File_Function)
/// does not match CallSite nodes and is unaffected. Q14 (unresolved external
/// refs in file F) consults the CallSite table; macro CallSites must be
/// flagged so they aren't counted as unresolved — the resolver wires them
/// to StdlibSymbol targets. source: stages/stage-3b-v2.md §5 Layer 4.
pub(super) fn extract_macro_call_site(ctx: &mut ExtractCtx, node: Node, caller_qn: &str) {
    let macro_name = match node.child_by_field_name("macro") {
        Some(n) => node_text(ctx.source, n),
        None => return,
    };
    if macro_name.is_empty() {
        return;
    }
    let line = node.start_position().row as u64 + 1;
    let col = node.start_position().column as u64;
    let start_byte = node.start_byte() as u64;
    let end_byte = node.end_byte() as u64;
    let marker = format!("{macro_name}!");
    // Chained calls share start_byte; the (start, end) span is unique.
    let cs_id = format!("{caller_qn}::call@{line}:{col}#{start_byte}-{end_byte}");
    ctx.nodes.push(ExtractedNode {
        label: LABEL_CALL_SITE.to_string(),
        name: marker.clone(),
        qualified_name: cs_id.clone(),
        start_line: line,
        end_line: line,
        visibility: String::new(),
        properties: vec![
            ("callee_name".to_string(), marker),
            ("caller_qn".to_string(), caller_qn.to_string()),
        ],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: caller_qn.to_string(),
        to_qualified_name: cs_id,
    });
}

pub(super) fn extract_single_call_site(ctx: &mut ExtractCtx, node: Node, caller_qn: &str) {
    let func_node = match node.child_by_field_name("function") {
        Some(f) => f,
        None => return,
    };
    let callee = node_text(ctx.source, func_node);
    // source: Spike B' BUG #10 fix — was `callee.contains('.')` which dropped
    // every method call (obj.method, etc.). Now extract all call_expression
    // nodes; resolver decides what can be resolved.
    if !callee.is_empty() {
        // The call's span keys the CallSite id; chained calls share start_byte
        // so the (start, end) byte span is the unique discriminator.
        push_call_site(ctx, &callee, node, caller_qn);
    }
    // A function passed *by value* as an argument (a higher-order call, e.g.
    // `queue.iter().map(process_order)`) is a real reference to that function,
    // but the argument identifier is not itself a `call_expression`, so the
    // call-site walk in `extract_call_sites` never emits a CallSite for it and
    // the resolver never records the Calls edge. Emit one CallSite per bare
    // identifier / path argument so the resolver can bind it to the referenced
    // function (Calls edge) or type (Uses edge). Arguments that are themselves
    // calls, closures, references, or literals are handled elsewhere or are not
    // function references, and are skipped.
    // source: issue #87 gap 3 (rs-D2) — the #64 head-to-head eval showed
    //   worker.rs::drain, which passes `process_order` to `.map`, was absent
    //   from the callers of `process_order` (recall 0.5 vs the Grep baseline).
    extract_fn_value_arg_call_sites(ctx, node, caller_qn);
}

/// Emit a CallSite for every function-value argument (a bare `identifier` or
/// `scoped_identifier` passed by value) of `call`. See the call site in
/// `extract_single_call_site` for the rationale and source.
///
/// precondition: `call` is a `call_expression` node.
/// postcondition: one CallSite (+ its `Defines` ref) is appended per direct
/// `identifier`/`scoped_identifier` child of the call's `arguments` field; no
/// CallSite is appended when the call has no such arguments. Speculative by
/// design — the resolver drops the reference when the name is a local binding
/// rather than a known function/type, exactly as it drops any other unresolved
/// callee (`len`, `HashMap::new`, …).
fn extract_fn_value_arg_call_sites(ctx: &mut ExtractCtx, call: Node, caller_qn: &str) {
    let args = match call.child_by_field_name(TS_ARGUMENTS) {
        Some(a) => a,
        None => return,
    };
    let mut cursor = args.walk();
    for arg in args.children(&mut cursor) {
        if matches!(arg.kind(), TS_IDENTIFIER | TS_SCOPED_IDENTIFIER) {
            let callee = node_text(ctx.source, arg);
            if !callee.is_empty() {
                push_call_site(ctx, &callee, arg, caller_qn);
            }
        }
    }
}

/// Append one CallSite node (and the `Defines` ref linking it to its caller)
/// for a callee named `callee`, keyed on `span_node`'s source span so the id is
/// unique among the caller's call sites (chained calls share a start byte, so
/// the (start, end) span is the discriminator).
/// precondition: `callee` is non-empty.
fn push_call_site(ctx: &mut ExtractCtx, callee: &str, span_node: Node, caller_qn: &str) {
    let line = span_node.start_position().row as u64 + 1;
    let col = span_node.start_position().column as u64;
    let start_byte = span_node.start_byte() as u64;
    let end_byte = span_node.end_byte() as u64;
    let cs_id = format!("{caller_qn}::call@{line}:{col}#{start_byte}-{end_byte}");
    ctx.nodes.push(ExtractedNode {
        label: LABEL_CALL_SITE.to_string(),
        name: callee.to_string(),
        qualified_name: cs_id.clone(),
        start_line: line,
        end_line: line,
        visibility: String::new(),
        properties: vec![
            ("callee_name".to_string(), callee.to_string()),
            ("caller_qn".to_string(), caller_qn.to_string()),
        ],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: caller_qn.to_string(),
        to_qualified_name: cs_id,
    });
}

// ---------------------------------------------------------------------------
// Supertrait extraction
// ---------------------------------------------------------------------------

pub(super) fn extract_supertraits(source: &str, trait_node: Node) -> Vec<String> {
    let mut supers = Vec::new();
    let bounds = match trait_node.child_by_field_name("bounds") {
        Some(b) => b,
        None => return supers,
    };
    let mut cursor = bounds.walk();
    for child in bounds.children(&mut cursor) {
        if child.kind() == "type_identifier" || child.kind() == "scoped_type_identifier" {
            let text = node_text(source, child);
            if !text.is_empty() {
                supers.push(text);
            }
        }
    }
    supers
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(super) fn extract_visibility(source: &str, node: Node) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == TS_VIS_MOD {
            return node_text(source, child);
        }
    }
    String::new()
}

pub(super) fn has_async_modifier(node: Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == TS_FUNC_MODS {
            let mut inner = child.walk();
            for gc in child.children(&mut inner) {
                if gc.kind() == "async" {
                    return true;
                }
            }
        }
    }
    false
}
