// parser::spec::rust_live_binding: which of the several bindings of one name
// is live at a call (issue #350).
//
// A receiver hint needs the type of the binding the call names. A name bound
// once has one candidate. A name bound more than once (`let s = a(); let s =
// b();`) has the one that is lexically live at the call: the binding whose
// scope holds the call and whose binding point ends before the call starts,
// the latest such point winning. Every other case is declined, never guessed:
// a form this module does not cover, no binding reaching the call, or (in the
// callers) a live binding with no type.
//
// The scope of each form follows the Rust reference (patterns and scopes,
// https://doc.rust-lang.org/reference/names/scopes.html) as read on
// tree-sitter-rust 0.24.2 src/node-types.json:
//   let           the enclosing block, after the whole declaration
//   fn parameter  the body, after the parameter list
//   closure param the closure body, after the parameter list
//   for           the loop body, after the iterated value
//   if/while let  the condition after the pattern, and the consequence or the
//                 body, never the `else`
//   match arm     the whole arm (guard included), after the arm's pattern

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use super::rust_item_binds::is_ancestor;
use super::rust_scope::{collect_identifiers, OnceBound};

const BODY_FIELD: &str = "body";
const CONDITION_FIELD: &str = "condition";
const CONSEQUENCE_FIELD: &str = "consequence";
const VALUE_FIELD: &str = "value";
const PATTERN_FIELD: &str = "pattern";

/// For every name bound by two or more of `bindings` (declaring node and
/// pattern pairs of one function) and not in `rebound`, the binding live at
/// `call`. A name with no live binding, or one with a form not covered, is
/// absent: the caller then has no hint for it.
pub(super) fn live_bindings<'t>(
    source: &str,
    call: Node<'t>,
    bindings: &[(Node<'t>, Node<'t>)],
    rebound: &HashSet<String>,
) -> Vec<OnceBound<'t>> {
    let mut by_name: HashMap<String, Vec<(Node<'t>, Node<'t>)>> = HashMap::new();
    for &(declaration, pattern) in bindings {
        let mut names = HashSet::new();
        collect_identifiers(source, pattern, &mut names);
        for name in names {
            by_name
                .entry(name)
                .or_default()
                .push((declaration, pattern));
        }
    }
    by_name
        .into_iter()
        .filter(|(name, sites)| sites.len() >= 2 && !rebound.contains(name))
        .filter_map(|(name, sites)| {
            live_of(call, &sites).map(|(declaration, pattern)| OnceBound {
                name,
                declaration,
                pattern,
            })
        })
        .collect()
}

/// The binding among `sites` that reaches `call` with the latest binding
/// point; `None` when one of them has a form not covered or none reaches.
fn live_of<'t>(call: Node<'t>, sites: &[(Node<'t>, Node<'t>)]) -> Option<(Node<'t>, Node<'t>)> {
    let mut live: Option<(usize, (Node<'t>, Node<'t>))> = None;
    for &(declaration, pattern) in sites {
        let (reaches, point) = scope_of(declaration, call)?;
        if reaches && live.is_none_or(|(best, _)| point > best) {
            live = Some((point, (declaration, pattern)));
        }
    }
    live.map(|(_, site)| site)
}

/// `(the binding reaches call, its binding point)`, or `None` for a form this
/// module does not cover.
fn scope_of<'t>(declaration: Node<'t>, call: Node<'t>) -> Option<(bool, usize)> {
    let after = |point: usize| point <= call.start_byte();
    match declaration.kind() {
        "let_declaration" => {
            let block = declaration.parent().filter(|p| p.kind() == "block")?;
            let point = declaration.end_byte();
            Some((is_ancestor(block, call) && after(point), point))
        }
        "for_expression" => {
            let body = declaration.child_by_field_name(BODY_FIELD)?;
            let point = declaration.child_by_field_name(VALUE_FIELD)?.end_byte();
            Some((is_ancestor(body, call) && after(point), point))
        }
        "let_condition" => {
            let owner = condition_owner(declaration)?;
            let condition = owner.child_by_field_name(CONDITION_FIELD)?;
            if !is_ancestor(condition, declaration) {
                return None;
            }
            let then_field = if owner.kind() == "if_expression" {
                CONSEQUENCE_FIELD
            } else {
                BODY_FIELD
            };
            let then = owner.child_by_field_name(then_field)?;
            let point = declaration.end_byte();
            let inside = is_ancestor(condition, call) || is_ancestor(then, call);
            Some((inside && after(point), point))
        }
        "match_arm" => {
            let pattern = declaration.child_by_field_name(PATTERN_FIELD)?;
            let inner = pattern.named_child(0).unwrap_or(pattern);
            let point = inner.end_byte();
            Some((is_ancestor(declaration, call) && after(point), point))
        }
        _ => parameter_scope(declaration, call),
    }
}

/// A function parameter or a closure parameter (typed or not): scope is the
/// body of the function or closure, after its parameter list.
fn parameter_scope<'t>(declaration: Node<'t>, call: Node<'t>) -> Option<(bool, usize)> {
    let list = declaration.parent()?;
    let owner = list.parent()?;
    if !matches!(
        (list.kind(), owner.kind()),
        ("parameters", "function_item") | ("closure_parameters", "closure_expression")
    ) {
        return None;
    }
    let body = owner.child_by_field_name(BODY_FIELD)?;
    let point = list.end_byte();
    Some((is_ancestor(body, call) && point <= call.start_byte(), point))
}

/// The `if` or `while` expression a `let` condition belongs to.
fn condition_owner(condition: Node) -> Option<Node> {
    let mut current = condition.parent();
    while let Some(node) = current {
        if matches!(node.kind(), "if_expression" | "while_expression") {
            return Some(node);
        }
        current = node.parent();
    }
    None
}
