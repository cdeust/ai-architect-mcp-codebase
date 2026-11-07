// parser::spec::ts_receiver — the type of a TypeScript/JavaScript local receiver
// that the enclosing function states by construction (issue #444, first slice).
//
// `const w = new Worker(); w.run()`: the callee reaches the resolver as `w.run`,
// and before this module no TypeScript call site carried the receiver's type, so
// `w.run` was bound by name only — or not at all when two classes own `run`. The
// Rust walkers (`rust_receiver`) and the C++ walkers (`cpp_receiver`) already
// attach a `receiver_hint`; this is the same contract for the TS family, limited
// to what the source proves on its own:
//
// - the receiver is a plain identifier `x` (not `this`, not a chain);
// - in the call's nearest enclosing function, `x` is declared exactly once, by a
//   `const`/`let`/`var` declarator whose value is `new T(..)` with `T` a plain
//   identifier, written before the call, in a block that contains the call;
// - nothing else in that function binds or writes `x`: no second declarator, no
//   assignment, no `++`/`--`, no parameter, no destructuring pattern, no `catch`
//   or `for .. in/of` binding, no function or class declaration of that name.
//
// Anything else yields no hint, so the resolver keeps its by-name path. A hint
// that is wrong would bind a call to a method it does not reach (a wrong single
// edge is worse than none), which is why a shadowed or reassigned name declines
// even when a finer scope analysis could type it.
//
// source: tree-sitter-typescript 0.23.2 node-types.json — `call_expression.function`,
// `member_expression.object`, `variable_declarator.name`/`value`,
// `new_expression.constructor`, `assignment_expression.left`,
// `augmented_assignment_expression.left`, `update_expression.argument`.

use tree_sitter::Node;

use crate::graph_store::RECEIVER_HINT_VIA_CONSTRUCTED;

/// Node kinds that open a new function scope in the TS/TSX grammars.
const FUNCTION_KINDS: &[&str] = &[
    "function_declaration",
    "function_expression",
    "generator_function_declaration",
    "generator_function",
    "arrow_function",
    "method_definition",
];

/// Parent kinds under which an identifier is a binding or a write that is not
/// the single `variable_declarator` this module accepts.
const BINDING_PARENT_KINDS: &[&str] = &[
    "required_parameter",
    "optional_parameter",
    "formal_parameters",
    "rest_pattern",
    "object_pattern",
    "array_pattern",
    "pair_pattern",
    "assignment_pattern",
    "object_assignment_pattern",
    "catch_clause",
    "for_in_statement",
    "update_expression",
    "function_declaration",
    "generator_function_declaration",
    "class_declaration",
    "abstract_class_declaration",
    "import_specifier",
    "namespace_import",
    "import_clause",
];

/// The `receiver_hint` and `receiver_hint_via` properties of the call
/// `call_node`, or none. See the module header for the rule.
///
/// postcondition: empty unless the receiver's type is a `new T(..)` the
/// enclosing function proves; otherwise exactly
/// `[("receiver_hint", T), ("receiver_hint_via", "constructed")]`.
pub(super) fn constructed_receiver_properties(
    source: &str,
    call_node: Node,
) -> Vec<(String, String)> {
    match constructed_receiver_type(source, call_node) {
        Some(ty) => vec![
            ("receiver_hint".to_string(), ty),
            (
                "receiver_hint_via".to_string(),
                RECEIVER_HINT_VIA_CONSTRUCTED.to_string(),
            ),
        ],
        None => Vec::new(),
    }
}

/// The class name `T` of `x` in `x.m(..)` when the enclosing function binds `x`
/// once to `new T(..)`; `None` otherwise.
fn constructed_receiver_type(source: &str, call_node: Node) -> Option<String> {
    let function = call_node.child_by_field_name("function")?;
    if function.kind() != "member_expression" {
        return None;
    }
    let object = function.child_by_field_name("object")?;
    if object.kind() != "identifier" {
        return None;
    }
    let name = text(source, object);
    let scope = enclosing_function(call_node)?;
    let declarator = single_declarator(source, scope, name)?;
    if !declaration_reaches(declarator, call_node) {
        return None;
    }
    let value = declarator.child_by_field_name("value")?;
    if value.kind() != "new_expression" {
        return None;
    }
    let constructor = value.child_by_field_name("constructor")?;
    (constructor.kind() == "identifier").then(|| text(source, constructor).to_string())
}

/// The nearest function-scope node above `node`.
fn enclosing_function(node: Node) -> Option<Node> {
    let mut current = node.parent();
    while let Some(n) = current {
        if FUNCTION_KINDS.contains(&n.kind()) {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// The one `variable_declarator` that declares `name` in `scope`, when no other
/// binding or write of `name` occurs anywhere in `scope` (nested functions
/// included).
fn single_declarator<'t>(source: &str, scope: Node<'t>, name: &str) -> Option<Node<'t>> {
    let mut found: Option<Node<'t>> = None;
    let mut stack = vec![scope];
    while let Some(n) = stack.pop() {
        if is_name_node(n) && text(source, n) == name {
            match binding_role(n) {
                Role::Declarator(d) if found.is_none() => found = Some(d),
                Role::Declarator(_) | Role::OtherBinding => return None,
                Role::Read => {}
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    found
}

/// What one occurrence of the receiver's name does.
enum Role<'t> {
    /// The `name` of this `variable_declarator`.
    Declarator(Node<'t>),
    /// A parameter, pattern, assignment target or other declaration.
    OtherBinding,
    /// A use that does not bind or write the name.
    Read,
}

fn binding_role<'t>(n: Node<'t>) -> Role<'t> {
    let Some(parent) = n.parent() else {
        return Role::Read;
    };
    let is_field = |field: &str| {
        parent
            .child_by_field_name(field)
            .is_some_and(|c| c.id() == n.id())
    };
    match parent.kind() {
        "variable_declarator" if is_field("name") => Role::Declarator(parent),
        "assignment_expression" | "augmented_assignment_expression" if is_field("left") => {
            Role::OtherBinding
        }
        "for_in_statement" if !is_field("left") => Role::Read,
        "catch_clause" if !is_field("parameter") => Role::Read,
        kind if BINDING_PARENT_KINDS.contains(&kind) => Role::OtherBinding,
        _ if n.kind() == "shorthand_property_identifier_pattern" => Role::OtherBinding,
        _ => Role::Read,
    }
}

/// True for the node kinds that can spell a local variable's name.
fn is_name_node(n: Node) -> bool {
    matches!(
        n.kind(),
        "identifier" | "shorthand_property_identifier_pattern"
    )
}

/// True when `declarator` ends before `call` starts and the declaration's own
/// block contains `call`: the name `call` reads is this declaration's.
fn declaration_reaches(declarator: Node, call: Node) -> bool {
    if declarator.end_byte() > call.start_byte() {
        return false;
    }
    let Some(declaration) = declarator.parent() else {
        return false;
    };
    let Some(block) = declaration.parent() else {
        return false;
    };
    block.start_byte() <= call.start_byte() && call.end_byte() <= block.end_byte()
}

fn text<'s>(source: &'s str, n: Node) -> &'s str {
    source.get(n.start_byte()..n.end_byte()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint_of(src: &str, callee: &str) -> Vec<(String, String)> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .expect("typescript grammar");
        let tree = parser.parse(src, None).expect("parse");
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if n.kind() == "call_expression"
                && n.child_by_field_name("function")
                    .is_some_and(|f| text(src, f) == callee)
            {
                return constructed_receiver_properties(src, n);
            }
            let mut cursor = n.walk();
            for c in n.children(&mut cursor) {
                stack.push(c);
            }
        }
        panic!("no call {callee} in fixture");
    }

    fn constructed(ty: &str) -> Vec<(String, String)> {
        vec![
            ("receiver_hint".to_string(), ty.to_string()),
            ("receiver_hint_via".to_string(), "constructed".to_string()),
        ]
    }

    #[test]
    fn const_let_and_var_from_new_carry_the_class() {
        for kw in ["const", "let", "var"] {
            let src = format!("function f() {{ {kw} w = new Worker(1); return w.run(); }}");
            assert_eq!(hint_of(&src, "w.run"), constructed("Worker"), "{kw}");
        }
    }

    #[test]
    fn generic_construction_carries_the_class_name() {
        let src = "const f = () => { const w = new Box<number>(); w.get(); };";
        assert_eq!(hint_of(src, "w.get"), constructed("Box"));
    }

    #[test]
    fn what_the_function_does_not_prove_carries_nothing() {
        let cases = [
            (
                "function f() { let w = new A(); w = new B(); w.run(); }",
                "reassigned",
            ),
            (
                "function f() { const w = new A(); { const w = new B(); w.run(); } }",
                "shadowed",
            ),
            ("function f(w) { w.run(); }", "parameter"),
            ("function f() { const w = make(); w.run(); }", "call value"),
            (
                "function f() { const w = new ns.A(); w.run(); }",
                "qualified ctor",
            ),
            (
                "function f() { w.run(); const w = new A(); }",
                "after the call",
            ),
            (
                "function f() { { const w = new A(); } w.run(); }",
                "inner block",
            ),
            ("function f() { const { w } = o; w.run(); }", "destructured"),
            ("function f() { let w = new A(); w++; w.run(); }", "updated"),
            (
                "function f() { let w = new A(); g(() => { w = 1; }); w.run(); }",
                "closure write",
            ),
            (
                "const w = new A(); function f() { w.run(); }",
                "module scope",
            ),
            ("function f() { const w = new A(); this.w.run(); }", "chain"),
        ];
        for (src, why) in cases {
            let callee = if why == "chain" {
                "this.w.run"
            } else {
                "w.run"
            };
            assert_eq!(hint_of(src, callee), Vec::new(), "{why}");
        }
    }

    #[test]
    fn reads_of_the_name_elsewhere_do_not_decline() {
        let src = "function f() { const w = new A(); log(w); const o = { w }; return w.run(); }";
        assert_eq!(hint_of(src, "w.run"), constructed("A"));
    }
}
