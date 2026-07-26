// parser::spec::walkers::clike_names — the flat C-family NAME resolution: which
// leaf of a wrapped declarator is the declared name, and whether a shared
// `declaration` kind is a function prototype.
//
// C names its declarations through nested declarators (`int (*handler)(int)`,
// `int add(int a, int b)`), so "the name" is the result of a specific,
// parity-critical search order rather than a field read. Those searches are
// separated here from the `clike` walker's emit plumbing (a pure move, §4.1
// 500-line cap); every function's search order is unchanged, which the C
// parity corpus pins.

use tree_sitter::Node;

use super::super::families::CFamilySpec;
use super::super::lang_spec::LangSpec;
use super::kind_in;
use crate::parser::{node_field_text, node_text};

/// The first identifier leaf (`identifier_kinds`) found in a right-to-left DFS of
/// `node`. This reproduces the hand-written `find_identifier`'s stack-DFS order
/// EXACTLY (children pushed left-to-right onto a LIFO stack, so the last child's
/// subtree is searched first). That order is load-bearing: for a function
/// declarator `add(int a, int b)` it returns the LAST parameter name (`b`), not
/// the function name — a pre-existing defect the migration preserves for parity
/// (filed as issue #106). Not "the name" — "the name the old walker produced".
pub(super) fn find_identifier(cf: &CFamilySpec, source: &str, node: Node) -> String {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if kind_in(cf.identifier_kinds, n.kind()) {
            return node_text(source, n);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    String::new()
}

/// The DECLARED name of a declarator: the identifier the declarator actually
/// binds, never a parameter name (issue #106).
///
/// C declarators nest the name inside wrappers — `int *f(void)` is
/// `pointer_declarator > function_declarator > identifier`, and `int (*h)(int)`
/// adds a `parenthesized_declarator` — so the name is reached by following the
/// `declarator_field` chain down to the identifier leaf.
///
/// The load-bearing part is what it does NOT do: it never descends into
/// `parameters_field`. `find_identifier`'s LIFO DFS visited the parameter list
/// before the declarator's own name, so `int add(int a, int b)` resolved to
/// `b`. A function whose last parameter is unnamed (`int f(void)`) resolved
/// correctly, which is why the defect survived — it is invisible on exactly the
/// signatures a minimal fixture uses.
///
/// `parenthesized_declarator` carries no fields at all (verified against
/// tree-sitter-c 0.23.4 node-types.json), so the field chain is followed first
/// and a named-children scan is the fallback — still skipping `parameters`.
pub(super) fn declarator_name(cf: &CFamilySpec, source: &str, node: Node) -> String {
    if kind_in(cf.identifier_kinds, node.kind()) {
        return node_text(source, node);
    }
    if let Some(inner) = node.child_by_field_name(cf.declarator_field) {
        let name = declarator_name(cf, source, inner);
        if !name.is_empty() {
            return name;
        }
    }
    // Fieldless wrappers (parenthesized_declarator): scan named children, but
    // never the parameter list.
    let params_id = node
        .child_by_field_name(cf.parameters_field)
        .map(|p| p.id());
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    for child in children {
        if Some(child.id()) == params_id {
            continue;
        }
        let name = declarator_name(cf, source, child);
        if !name.is_empty() {
            return name;
        }
    }
    String::new()
}

/// The first `field_identifier` leaf found in a right-to-left DFS of `node`,
/// unwrapping pointer/array/function declarators to the bare field name
/// (`int *p` → `p`, `char buf[8]` → `buf`, `int (*h)(int)` → `h`). Same LIFO-DFS
/// order as `find_identifier` — reproduces the hand-written `find_field_identifier`.
pub(super) fn find_field_identifier(cf: &CFamilySpec, source: &str, node: Node) -> String {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == cf.field_identifier_kind {
            return node_text(source, n);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    String::new()
}

/// A `func_decl_kinds` node is a function prototype iff it carries a function
/// declarator directly, or inside an `init_declarator` (`int f(void) = …`).
/// A plain variable declaration (`int x;`) carries neither and is not a prototype.
pub(super) fn is_c_function_prototype(cf: &CFamilySpec, node: Node) -> bool {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == cf.func_declarator_kind {
            return true;
        }
        if c.kind() == cf.init_declarator_kind {
            let mut ic = c.walk();
            for gc in c.children(&mut ic) {
                if gc.kind() == cf.func_declarator_kind {
                    return true;
                }
            }
        }
    }
    false
}

/// The declaration's name: the `name` field's text, or — when absent (an
/// anonymous struct/union/enum) — the first identifier leaf (LIFO DFS). Matches
/// the hand-written `node_field_text(node,"name")`-then-`find_identifier` fallback.
pub(super) fn named_or_first_identifier(
    cf: &CFamilySpec,
    spec: &LangSpec,
    source: &str,
    node: Node,
) -> String {
    let field = node_field_text(source, node, spec.name_field);
    if field.is_empty() {
        find_identifier(cf, source, node)
    } else {
        field
    }
}
