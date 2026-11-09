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
//   assignment (also through `(x)`, `x!`, `x as T`, `x satisfies T`, `<T>x`), no
//   `++`/`--`, no parameter (bare arrow parameters included), no destructuring
//   pattern, no `catch` or `for .. in/of` binding, no function or class
//   declaration of that name;
// - `T` itself is not rebound on the way to the program: no function scope that
//   encloses the call binds or writes `T` (`function f(Worker: typeof Other)`),
//   and the program holds at most one binding of `T` (its class declaration).
//
// Cost: the binding table of each function scope (and of the program) is built
// once per file in one DFS (`ReceiverScopes`, cached in the walk context), so a
// function with thousands of `console.log(..)` calls is walked once, not once
// per call (review of PR #479).
//
// Anything else yields no hint, so the resolver keeps its by-name path. A hint
// that is wrong would bind a call to a method it does not reach (a wrong single
// edge is worse than none), which is why a shadowed or reassigned name declines
// even when a finer scope analysis could type it.
//
// source: tree-sitter-typescript 0.23.2 node-types.json — `call_expression.function`,
// `member_expression.object`, `variable_declarator.name`/`value`,
// `new_expression.constructor`, `assignment_expression.left`,
// `augmented_assignment_expression.left`, `update_expression.argument`,
// `arrow_function.parameter`, `class_declaration.name` (a `type_identifier`).

use std::collections::HashMap;

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
    "import_specifier",
    "namespace_import",
    "import_clause",
];

/// Expression wrappers that leave their operand an assignment target:
/// `(w) = ..`, `w! = ..`, `(w as any) = ..`, `(w satisfies T) = ..`,
/// `(<any>w) = ..`. An occurrence under them is classified as its outermost
/// wrapper would be.
const TARGET_WRAPPER_KINDS: &[&str] = &[
    "parenthesized_expression",
    "non_null_expression",
    "as_expression",
    "satisfies_expression",
    "type_assertion",
];

/// Class declarations, whose `name` is a `type_identifier` in both grammars.
const CLASS_DECLARATION_KINDS: &[&str] = &["class_declaration", "abstract_class_declaration"];

/// Per-tree cache of the binding tables of function scopes and of the program,
/// keyed by node id: each table is built in one DFS of its scope, so the walk
/// of a file costs O(nodes × function nesting depth) whatever its call count.
/// Valid for ONE tree only: node ids are unique within a tree, not across trees.
#[derive(Default)]
pub(crate) struct ReceiverScopes {
    tables: HashMap<usize, HashMap<String, NameUse>>,
}

/// Every binding or write of one name inside a scope (nested functions
/// included).
#[derive(Default)]
struct NameUse {
    /// Declarators, parameters, patterns, assignment targets and declarations.
    bindings: u32,
    /// The first `variable_declarator` of the name, when there is one.
    declared: Option<Declared>,
}

/// What `declaration_reaches` and the constructor check need of a declarator.
struct Declared {
    end_byte: usize,
    /// Byte span of the block that holds the declaration; `None` at the root.
    block: Option<(usize, usize)>,
    /// `T` when the value is `new T(..)` with `T` a plain identifier.
    constructor: Option<String>,
}

impl ReceiverScopes {
    /// The `receiver_hint` and `receiver_hint_via` properties of the call
    /// `call_node`, or none. See the module header for the rule.
    ///
    /// postcondition: empty unless the receiver's type is a `new T(..)` the
    /// enclosing function proves; otherwise exactly
    /// `[("receiver_hint", T), ("receiver_hint_via", "constructed")]`.
    pub(crate) fn constructed_receiver_properties(
        &mut self,
        source: &str,
        call_node: Node,
    ) -> Vec<(String, String)> {
        match self.constructed_receiver_type(source, call_node) {
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

    /// The class name `T` of `x` in `x.m(..)` when the enclosing function binds
    /// `x` once to `new T(..)` and no scope on the way to the program rebinds
    /// `T`; `None` otherwise.
    fn constructed_receiver_type(&mut self, source: &str, call_node: Node) -> Option<String> {
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
        let ty = {
            let use_ = self.table(source, scope).get(name)?;
            let declared = use_.declared.as_ref().filter(|_| use_.bindings == 1)?;
            if !declaration_reaches(declared, call_node) {
                return None;
            }
            declared.constructor.clone()?
        };
        if self.constructor_rebound(source, scope, &ty) {
            return None;
        }
        Some(ty)
    }

    /// True when the constructor's name `ty` may not denote the file's class
    /// `ty` at the call: a function scope enclosing the call binds or writes
    /// it (`function f(Worker: typeof Other) { new Worker() }`), or the program
    /// holds more than its one declaration (`Worker = Other`).
    fn constructor_rebound(&mut self, source: &str, scope: Node, ty: &str) -> bool {
        let mut current = Some(scope);
        let mut root = scope;
        while let Some(s) = current {
            if self.table(source, s).contains_key(ty) {
                return true;
            }
            root = s;
            current = enclosing_function(s);
        }
        while let Some(p) = root.parent() {
            root = p;
        }
        self.table(source, root)
            .get(ty)
            .is_some_and(|use_| use_.bindings > 1)
    }

    fn table(&mut self, source: &str, scope: Node) -> &HashMap<String, NameUse> {
        self.tables
            .entry(scope.id())
            .or_insert_with(|| binding_table(source, scope))
    }
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

/// Every name bound or written anywhere in `scope` (nested functions
/// included), in one DFS. Reads are not recorded.
fn binding_table(source: &str, scope: Node) -> HashMap<String, NameUse> {
    let mut table: HashMap<String, NameUse> = HashMap::new();
    let mut stack = vec![scope];
    while let Some(n) = stack.pop() {
        if is_name_node(n) {
            let role = binding_role(n);
            if !matches!(role, Role::Read) {
                let use_ = table.entry(text(source, n).to_string()).or_default();
                use_.bindings += 1;
                if let Role::Declarator(d) = role {
                    if use_.declared.is_none() {
                        use_.declared = Some(declared(source, d));
                    }
                }
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    table
}

fn declared(source: &str, declarator: Node) -> Declared {
    let block = declarator
        .parent()
        .and_then(|declaration| declaration.parent())
        .map(|b| (b.start_byte(), b.end_byte()));
    let constructor = declarator
        .child_by_field_name("value")
        .filter(|v| v.kind() == "new_expression")
        .and_then(|v| v.child_by_field_name("constructor"))
        .filter(|c| c.kind() == "identifier")
        .map(|c| text(source, c).to_string());
    Declared {
        end_byte: declarator.end_byte(),
        block,
        constructor,
    }
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
    if n.kind() == "type_identifier" {
        return match n.parent() {
            Some(p) if CLASS_DECLARATION_KINDS.contains(&p.kind()) && is_field_of(p, "name", n) => {
                Role::OtherBinding
            }
            _ => Role::Read,
        };
    }
    let target = unwrap_target(n);
    let Some(parent) = target.parent() else {
        return Role::Read;
    };
    let is_field = |field: &str| is_field_of(parent, field, target);
    match parent.kind() {
        "variable_declarator" if is_field("name") => Role::Declarator(parent),
        "assignment_expression" | "augmented_assignment_expression" if is_field("left") => {
            Role::OtherBinding
        }
        "update_expression" if is_field("argument") => Role::OtherBinding,
        "arrow_function" if is_field("parameter") => Role::OtherBinding,
        "for_in_statement" if !is_field("left") => Role::Read,
        "catch_clause" if !is_field("parameter") => Role::Read,
        kind if BINDING_PARENT_KINDS.contains(&kind) => Role::OtherBinding,
        _ if n.kind() == "shorthand_property_identifier_pattern" => Role::OtherBinding,
        _ => Role::Read,
    }
}

/// The outermost wrapper of `n` among `TARGET_WRAPPER_KINDS` of which `n` is
/// the operand, or `n` itself.
fn unwrap_target(n: Node) -> Node {
    let mut current = n;
    while let Some(p) = current.parent() {
        if !TARGET_WRAPPER_KINDS.contains(&p.kind())
            || p.named_child(0).map(|c| c.id()) != Some(current.id())
        {
            break;
        }
        current = p;
    }
    current
}

fn is_field_of(parent: Node, field: &str, child: Node) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|c| c.id() == child.id())
}

/// True for the node kinds that can spell a local variable's or a class's
/// name.
fn is_name_node(n: Node) -> bool {
    matches!(
        n.kind(),
        "identifier" | "shorthand_property_identifier_pattern" | "type_identifier"
    )
}

/// True when the declarator ends before `call` starts and the declaration's
/// own block contains `call`: the name `call` reads is this declaration's.
fn declaration_reaches(declared: &Declared, call: Node) -> bool {
    if declared.end_byte > call.start_byte() {
        return false;
    }
    let Some((start, end)) = declared.block else {
        return false;
    };
    start <= call.start_byte() && call.end_byte() <= end
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
                return ReceiverScopes::default().constructed_receiver_properties(src, n);
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
            (
                "function f() { let w = new A(); (w) = new B(); w.run(); }",
                "parenthesized write",
            ),
            (
                "function f() { let w = new A(); w! = new B(); w.run(); }",
                "non-null write",
            ),
            (
                "function f() { let w = new A(); (w as any) = new B(); w.run(); }",
                "as write",
            ),
            (
                "function f() { let w = new A(); (w)++; w.run(); }",
                "parenthesized update",
            ),
            (
                "function f() { const w = new A(); { class w { static run() {} } w.run(); } }",
                "class declaration",
            ),
            (
                "function f() { const w = new A(); g(w => 1); w.run(); }",
                "bare arrow parameter",
            ),
            (
                "class A {} class B {} function f(A: typeof B) { const w = new A(); w.run(); }",
                "constructor parameter",
            ),
            (
                "class A {} function o(A) { function f() { const w = new A(); w.run(); } }",
                "constructor bound by an outer function",
            ),
            (
                "class A {} class B {} A = B; function f() { const w = new A(); w.run(); }",
                "constructor reassigned in the program",
            ),
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
    fn a_file_local_class_constructed_in_a_function_carries_the_class() {
        let src = "class A { run() {} } function f() { const w = new A(); w.run(); }";
        assert_eq!(hint_of(src, "w.run"), constructed("A"));
    }

    #[test]
    fn reads_through_wrappers_do_not_decline() {
        let src = "function f() { const w = new A(); g((w as any), w!); w.run(); }";
        assert_eq!(hint_of(src, "w.run"), constructed("A"));
    }

    /// Review of PR #479: the hint used to rescan the whole enclosing function
    /// for every member call. Each scope's table is now built once, whatever
    /// the number of calls in it.
    #[test]
    fn each_scope_is_scanned_once_per_tree() {
        let body = "console.log(i); ".repeat(2_000);
        let src = format!("function f() {{ const w = new A(); {body} w.run(); }}");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .expect("typescript grammar");
        let tree = parser.parse(&src, None).expect("parse");
        let mut scopes = ReceiverScopes::default();
        let mut hints = 0;
        let mut calls = 0;
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if n.kind() == "call_expression" {
                calls += 1;
                if !scopes.constructed_receiver_properties(&src, n).is_empty() {
                    hints += 1;
                }
            }
            let mut cursor = n.walk();
            for c in n.children(&mut cursor) {
                stack.push(c);
            }
        }
        assert_eq!(calls, 2_001);
        assert_eq!(hints, 1, "only w.run carries a hint");
        // `f` and the program: one table each.
        assert_eq!(scopes.tables.len(), 2);
    }

    #[test]
    fn reads_of_the_name_elsewhere_do_not_decline() {
        let src = "function f() { const w = new A(); log(w); const o = { w }; return w.run(); }";
        assert_eq!(hint_of(src, "w.run"), constructed("A"));
    }
}
