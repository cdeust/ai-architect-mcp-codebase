// parser::spec::walkers::declarator — the ONE C-family name search, shared by
// the flat `clike` walker (C) and the hybrid `cpp` walker (C++), driven by the
// `DeclaratorNaming` sub-table both language rows carry.
//
// Issue #106 fixed "the function is named after its last parameter" for C by
// following the `declarator` FIELD chain and never descending into `parameters`,
// with the skip expressed as DATA (`parameters_field`). Issue #123 is the same
// defect in the C++ walker. Rather than a second copy of the same traversal,
// this module holds the mechanism once and each family row supplies the grammar
// data — so a third C-family grammar (ObjC) inherits the corrected behavior by
// adding a row, which is the dedup ADR-0055 §1.2 promises.
//
// All node kinds/fields these functions read come from the caller's
// `DeclaratorNaming`, validated against that grammar's node-types.json by the
// spec guard; nothing is hardcoded here.

use tree_sitter::Node;

use super::super::lang_spec::{DeclaratorNaming, LangSpec};
use super::kind_in;
use crate::parser::{node_field_text, node_text};

/// The first identifier leaf found in a right-to-left DFS of `node` (children
/// pushed left-to-right onto a LIFO stack, so the LAST child's subtree is
/// searched first).
///
/// Preconditions: none. Postconditions: returns the text of the first
/// `identifier_kinds` node popped, or `""` when the subtree holds none.
/// `name_text_kinds` is deliberately NOT consulted here: every caller of this
/// function names a container (a namespace, an anonymous struct/union/enum, a
/// typedef alias), and no container is spelled with an `operator_name` or a
/// `destructor_name` — matching them would be an unreachable branch (§9).
///
/// This order is a deliberate artifact, not "the name": for a declarator it
/// lands on the deepest-rightmost leaf, which is why it must NOT be used to name
/// a function (that is `declarator_name`). It is still the right search for
/// nodes whose declared name genuinely trails their subtree — a C
/// `type_definition` (`typedef struct {…} T;` → `T`) and an anonymous
/// struct/enum specifier's fallback.
pub(super) fn first_identifier(naming: &DeclaratorNaming, source: &str, node: Node) -> String {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if kind_in(naming.identifier_kinds, n.kind()) {
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
/// binds, never a parameter name (issues #106, #123).
///
/// Preconditions: `node` is a declarator (or a declaration's `declarator_field`
/// child). Postconditions: returns the bound name, or `""` when the chain holds
/// no identifier leaf. Invariant: the returned text never comes from inside
/// `naming.parameters_field`.
///
/// C-family declarators nest the name inside wrappers — `int *f(void)` is
/// `pointer_declarator > function_declarator > identifier`, `int (*h)(int)` adds
/// a `parenthesized_declarator`, and C++ `int& g()` adds a
/// `reference_declarator` — so the name is reached by following the
/// `declarator_field` chain down to the identifier leaf.
///
/// The load-bearing part is what it does NOT do: it never descends into
/// `parameters_field`. A LIFO DFS (`first_identifier`) visits the parameter list
/// before the declarator's own name, so `int add(int a, int b)` resolved to `b`.
/// A function whose last parameter is unnamed (`int f(void)`) resolved correctly,
/// which is why the defect survived in both grammars — it is invisible on
/// exactly the signatures a minimal fixture uses.
///
/// `name_text_kinds` terminates the walk on a node that IS the name: C++
/// `destructor_name` (`~Point`) and `operator_name` (`operator+`) each wrap an
/// inner `identifier` whose text would be the wrong name (`Point`, and nothing),
/// so descending past them is what made `operator+` resolve to its parameter.
/// The check is first, before the field chain, because both kinds have children.
///
/// `parenthesized_declarator` and `reference_declarator` carry NO fields at all
/// (verified against tree-sitter-c 0.23.4 and tree-sitter-cpp 0.23.4
/// node-types.json), so a named-children scan handles exactly those — still
/// skipping `parameters`. Every OTHER declarator kind declares `declarator` as a
/// required field, so the two cases partition the grammar and neither needs a
/// fallback into the other.
pub(super) fn declarator_name(naming: &DeclaratorNaming, source: &str, node: Node) -> String {
    if kind_in(naming.name_text_kinds, node.kind()) {
        return node_text(source, node);
    }
    if kind_in(naming.identifier_kinds, node.kind()) {
        return node_text(source, node);
    }
    // When the grammar names a `declarator` field, that field IS where the bound
    // name lives — there is no sibling to fall back to, so its answer is final.
    // (An earlier version fell through to the sibling scan when this returned
    // empty; mutation testing showed the fallback unreachable, since every node
    // kind carrying a `declarator` field declares it REQUIRED — verified in
    // tree-sitter-c/-cpp 0.23.4 node-types.json for pointer_declarator,
    // function_declarator, array_declarator, and init_declarator. Removing the
    // dead branch is the fix; documenting it as an equivalent mutant would not be,
    // coding-standards §9/§12.1.)
    if let Some(inner) = node.child_by_field_name(naming.declarator_field) {
        return declarator_name(naming, source, inner);
    }
    // Fieldless wrappers (parenthesized/reference declarators): scan named
    // children, but never the parameter list.
    let params_id = node
        .child_by_field_name(naming.parameters_field)
        .map(|p| p.id());
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    for child in children {
        if Some(child.id()) == params_id {
            continue;
        }
        let name = declarator_name(naming, source, child);
        if !name.is_empty() {
            return name;
        }
    }
    String::new()
}

/// Whether a member/declaration declarator binds a real function PROTOTYPE
/// (a callable — a C++ method or a C function) rather than a function-POINTER
/// (data — a C++ data member or a C variable). Issue #135, and its C analog
/// `int (*signal_handler)(int)`.
///
/// Preconditions: `node` is the `declarator_field` child of a member/declaration
/// (a `function_declarator`, `pointer_declarator`, `init_declarator`, …).
/// `func_declarator_kind` is the grammar's function-declarator kind.
/// Postconditions: `true` iff, following the declarator chain toward the bound
/// name, a `function_declarator` is reached AND no `indirection_declarator_kinds`
/// wrapper sits BETWEEN that function_declarator and the name.
///
/// The two shapes are structurally identical at the top — both spell the
/// outermost declarator as a `function_declarator` — so a plain
/// "is there a function_declarator anywhere" test cannot tell them apart:
///   - `void f(int)`     → `function_declarator > name` ⇒ prototype
///   - `void (*cb)(int)` → `function_declarator > (…) > * > name` ⇒ data (a
///     pointer BELOW the function_declarator)
///   - `int  *f(int)`    → `* > function_declarator > name` ⇒ prototype (the
///     pointer is the RETURN type, ABOVE the function_declarator)
///
/// So the discriminator is position: an indirection wrapper counts only once a
/// `function_declarator` has already been entered on the path to the name. The
/// walk follows `declarator_field` and, for the fieldless
/// `parenthesized_declarator`, its named children (never `parameters_field`) —
/// the same chain `declarator_name` follows.
pub(super) fn binds_function_prototype(
    naming: &DeclaratorNaming,
    func_declarator_kind: &str,
    node: Node,
) -> bool {
    classify_prototype(naming, func_declarator_kind, node, false, false).unwrap_or(false)
}

/// The recursion behind `binds_function_prototype`. `seen_func` is set once a
/// `function_declarator` has been entered; `ptr_after` once an indirection
/// wrapper is seen WHILE `seen_func` holds. Returns `Some(is_prototype)` at the
/// first name leaf reached, `None` for a subtree that holds no name leaf (so the
/// caller tries the next named child).
fn classify_prototype(
    naming: &DeclaratorNaming,
    func_declarator_kind: &str,
    node: Node,
    seen_func: bool,
    ptr_after: bool,
) -> Option<bool> {
    let k = node.kind();
    let ptr_after = ptr_after || (seen_func && kind_in(naming.indirection_declarator_kinds, k));
    let seen_func = seen_func || k == func_declarator_kind;
    if kind_in(naming.name_text_kinds, k) || kind_in(naming.identifier_kinds, k) {
        return Some(seen_func && !ptr_after);
    }
    if let Some(inner) = node.child_by_field_name(naming.declarator_field) {
        return classify_prototype(naming, func_declarator_kind, inner, seen_func, ptr_after);
    }
    // Fieldless wrappers only (`parenthesized_declarator` / `reference_declarator`):
    // scan named children for the one continuing toward the name. No parameter
    // skip is needed here — the ONLY node carrying a `parameters` field is a
    // `function_declarator`, which always has a `declarator` field and so is
    // consumed by the branch above; a node reaching this loop never has a
    // parameter list. (`declarator_name`'s loop keeps the skip because it is
    // reached in more contexts; here it would be dead code, and a dead
    // parameter-skip is a mutant no test can kill — §9/§12.1.)
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    for child in children {
        if let Some(r) =
            classify_prototype(naming, func_declarator_kind, child, seen_func, ptr_after)
        {
            return Some(r);
        }
    }
    None
}

/// Every `declarator_field` child of `decl`, in source order.
///
/// Preconditions: none. Postconditions: one entry per child whose FIELD NAME is
/// `naming.declarator_field`, in source order; empty when the declaration binds
/// no name (a member that declares only a nested type).
///
/// A single member declaration binds several names through REPEATED `declarator`
/// fields (`int a, b, c;`), so `child_by_field_name` — which returns only the
/// first — silently drops every name but one. The cursor walk collects them all.
pub(super) fn declarator_field_children<'t>(
    naming: &DeclaratorNaming,
    decl: Node<'t>,
) -> Vec<Node<'t>> {
    let mut declarators: Vec<Node> = Vec::new();
    let mut dc = decl.walk();
    if dc.goto_first_child() {
        loop {
            if dc.field_name() == Some(naming.declarator_field) {
                declarators.push(dc.node());
            }
            if !dc.goto_next_sibling() {
                break;
            }
        }
    }
    declarators
}

/// A declaration's name: the `name_field` text, or — when absent (an anonymous
/// struct/union/enum) — the first identifier leaf.
///
/// Preconditions: none. Postconditions: `""` only when the node has neither a
/// `name_field` nor any identifier leaf. Shared by both C-family walkers for
/// namespaces, classes, structs, unions, and enums.
pub(super) fn named_or_first_identifier(
    naming: &DeclaratorNaming,
    spec: &LangSpec,
    source: &str,
    node: Node,
) -> String {
    let field = node_field_text(source, node, spec.name_field);
    if field.is_empty() {
        first_identifier(naming, source, node)
    } else {
        field
    }
}
