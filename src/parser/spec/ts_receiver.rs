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
//   pattern, no `catch` or `for .. in/of` binding, no function declaration or
//   named function expression, no class declaration or named class expression
//   of that name;
// - `T` itself is not rebound on the way to the program: no function scope that
//   encloses the call binds or writes `T` (`function f(Worker: typeof Other)`,
//   `const g = function Worker() {..}`), and the program holds at most one
//   binding of `T` (its class declaration);
// - `new T(..)` is an instance of `T` whose `m` is an instance method: when the
//   file declares `T`, `T` has no `static` member `m` (the instance call would
//   reach an inherited `m`, not `static m`), and no constructor on its `extends`
//   chain returns a value (`constructor() { return new Other(); }` replaces the
//   instance). A base the file does not declare (imported, or not a plain
//   identifier) cannot be checked, so it declines.
//
// Cost: ONE depth-first pass over the tree, on the first query (`FileFacts`,
// cached in the walk context). Each frame carries its parent, its two next
// ancestors and its enclosing function spans down the stack, so no node climbs
// with `Node::parent()` (which tree-sitter answers by descending from the root,
// review of PR #479), and no scope is rescanned for a nested one. A query is two
// hash lookups and two binary searches over the occurrences of one name.
//
// Anything else yields no hint, so the resolver keeps its by-name path. A hint
// that is wrong would bind a call to a method it does not reach (a wrong single
// edge is worse than none), which is why a shadowed or reassigned name declines
// even when a finer scope analysis could type it.
//
// source: tree-sitter-typescript 0.23.2 node-types.json — `call_expression.function`,
// `member_expression.object`/`property`, `variable_declarator.name`/`value`,
// `new_expression.constructor`, `assignment_expression.left`,
// `augmented_assignment_expression.left`, `update_expression.argument`,
// `arrow_function.parameter`, `function_expression.name` (an `identifier`),
// `class_declaration.name` and `class.name` (a `type_identifier`), `class.body`,
// `class_heritage` > `extends_clause.value`, `type_assertion` (children
// `type_arguments` then the operand), `method_definition.name`/`body`,
// `public_field_definition.name`.

use std::collections::{HashMap, HashSet};

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

/// Class declarations and class expressions, whose `name` is a
/// `type_identifier` in both grammars.
const CLASS_KINDS: &[&str] = &["class_declaration", "abstract_class_declaration", "class"];

/// A byte span `[start, end)`.
type Span = (usize, usize);

/// Per-tree cache of the facts the hint reads, built in one pass on the first
/// query. Valid for ONE tree only: node ids are unique within a tree, not
/// across trees.
#[derive(Default)]
pub(crate) struct ReceiverScopes {
    facts: Option<FileFacts>,
    /// Number of passes over a tree (one per `ReceiverScopes`).
    #[cfg(test)]
    builds: usize,
    /// Nodes visited by those passes.
    #[cfg(test)]
    visits: usize,
}

#[derive(Default)]
struct FileFacts {
    /// Every binding or write of a name in the file, sorted by start byte.
    names: HashMap<String, Vec<Occurrence>>,
    /// Call node id → (innermost, outermost) enclosing function span. Calls
    /// outside every function are absent.
    calls: HashMap<usize, (Span, Span)>,
    /// What the file says of each class it declares by that name.
    classes: HashMap<String, Vec<ClassFacts>>,
}

/// One binding or write of a name.
struct Occurrence {
    start: usize,
    /// Set when the occurrence is the `name` of a `variable_declarator`.
    declared: Option<Declared>,
}

/// What `declaration_reaches` and the constructor check need of a declarator.
struct Declared {
    end_byte: usize,
    /// Byte span of the block that holds the declaration; `None` at the root.
    block: Option<Span>,
    /// `T` when the value is `new T(..)` with `T` a plain identifier.
    constructor: Option<String>,
}

#[derive(Default)]
struct ClassFacts {
    /// Names of the class's `static` methods and fields.
    statics: HashSet<String>,
    /// A constructor of the class returns a value (it may replace `this`).
    ctor_returns_value: bool,
    base: Base,
}

#[derive(Default)]
enum Base {
    #[default]
    None,
    /// `extends B` with `B` a plain identifier.
    Named(String),
    /// `extends <expression>`: a mixin call, a member access, ...
    Unknown,
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
    /// `x` once to `new T(..)`, no scope on the way to the program rebinds `T`,
    /// and `m` is not a static member of `T`; `None` otherwise.
    fn constructed_receiver_type(&mut self, source: &str, call_node: Node) -> Option<String> {
        let function = call_node.child_by_field_name("function")?;
        if function.kind() != "member_expression" {
            return None;
        }
        let object = function.child_by_field_name("object")?;
        if object.kind() != "identifier" {
            return None;
        }
        let method = text(source, function.child_by_field_name("property")?);
        let name = text(source, object);
        let facts = self.facts(source, call_node);
        let &(inner, outer) = facts.calls.get(&call_node.id())?;
        let [only] = within(facts.names.get(name)?, inner) else {
            return None;
        };
        let declared = only.declared.as_ref()?;
        if !declaration_reaches(declared, call_node) {
            return None;
        }
        let ty = declared.constructor.as_deref()?;
        if facts.rebound(ty, outer) || !facts.instance_method_of(ty, method) {
            return None;
        }
        Some(ty.to_string())
    }

    fn facts(&mut self, source: &str, call_node: Node) -> &FileFacts {
        if self.facts.is_none() {
            // Once per tree: the climb to the root is paid on the first query
            // only.
            let mut root = call_node;
            while let Some(p) = root.parent() {
                root = p;
            }
            let (facts, visits) = build_facts(source, root);
            #[cfg(test)]
            {
                self.builds += 1;
                self.visits += visits;
            }
            #[cfg(not(test))]
            let _ = visits;
            self.facts = Some(facts);
        }
        self.facts.get_or_insert_with(FileFacts::default)
    }
}

impl FileFacts {
    /// True when the constructor's name `ty` may not denote the file's class
    /// `ty` at the call: the outermost function enclosing the call binds or
    /// writes it (every enclosing function lies inside that one), or the
    /// program holds more than its one declaration (`Worker = Other`).
    fn rebound(&self, ty: &str, outer: Span) -> bool {
        let occurrences = self.names.get(ty).map_or(&[][..], Vec::as_slice);
        occurrences.len() > 1 || !within(occurrences, outer).is_empty()
    }

    /// True unless the file's class `ty` makes `new ty(..).m` something other
    /// than an instance method of `ty`'s own chain: `m` is static on `ty`, a
    /// constructor on the chain returns a value, or the chain leaves the file.
    /// A `ty` the file does not declare is true: the resolver's gate binds in
    /// the caller's file only, so it finds no class to bind to.
    fn instance_method_of(&self, ty: &str, m: &str) -> bool {
        let Some(own) = self.classes.get(ty) else {
            return true;
        };
        if own.iter().any(|c| c.statics.contains(m)) {
            return false;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut pending = vec![ty];
        while let Some(name) = pending.pop() {
            // A cycle, or a base the file does not declare once.
            if !seen.insert(name) || (name != ty && self.names.get(name).map_or(0, Vec::len) != 1) {
                return false;
            }
            let Some(chain) = self.classes.get(name) else {
                return false;
            };
            for class in chain {
                if class.ctor_returns_value {
                    return false;
                }
                match &class.base {
                    Base::None => {}
                    Base::Named(base) => pending.push(base),
                    Base::Unknown => return false,
                }
            }
        }
        true
    }
}

/// The occurrences that start inside `span` (`occurrences` sorted by start).
fn within(occurrences: &[Occurrence], (start, end): Span) -> &[Occurrence] {
    let lo = occurrences.partition_point(|o| o.start < start);
    let hi = occurrences.partition_point(|o| o.start < end);
    &occurrences[lo..hi]
}

/// One node of the pass, with what its ancestors say about it.
#[derive(Clone, Copy)]
struct Frame<'t> {
    node: Node<'t>,
    parent: Option<Node<'t>>,
    grandparent: Option<Node<'t>>,
    great_grandparent: Option<Node<'t>>,
    /// `(p, t)`: `t` is `node` or the outermost `TARGET_WRAPPER_KINDS` node of
    /// which `node` is the operand, and `p` is `t`'s parent.
    target: Option<(Node<'t>, Node<'t>)>,
    /// (innermost, outermost) function span enclosing `node`.
    scope: Option<(Span, Span)>,
}

/// The facts of the tree under `root`, in one depth-first pass, and the number
/// of nodes visited.
fn build_facts(source: &str, root: Node) -> (FileFacts, usize) {
    let mut facts = FileFacts::default();
    let mut visits = 0;
    let mut stack = vec![Frame {
        node: root,
        parent: None,
        grandparent: None,
        great_grandparent: None,
        target: None,
        scope: None,
    }];
    while let Some(f) = stack.pop() {
        visits += 1;
        let n = f.node;
        if is_name_node(n) {
            let role = binding_role(&f);
            if !matches!(role, Role::Read) {
                let declared = matches!(role, Role::Declarator).then(|| Declared {
                    end_byte: f.parent.map_or(n.end_byte(), |d| d.end_byte()),
                    block: f.great_grandparent.map(span),
                    constructor: f.parent.and_then(|d| constructor_of(source, d)),
                });
                facts
                    .names
                    .entry(text(source, n).to_string())
                    .or_default()
                    .push(Occurrence {
                        start: n.start_byte(),
                        declared,
                    });
            }
        }
        if n.kind() == "call_expression" {
            if let Some(scope) = f.scope {
                facts.calls.insert(n.id(), scope);
            }
        }
        record_class(source, n, &mut facts.classes);
        let operand = if TARGET_WRAPPER_KINDS.contains(&n.kind()) {
            operand(n).map(|o| o.id())
        } else {
            None
        };
        let scope = if FUNCTION_KINDS.contains(&n.kind()) {
            let s = span(n);
            Some((s, f.scope.map_or(s, |(_, outer)| outer)))
        } else {
            f.scope
        };
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(Frame {
                node: c,
                parent: Some(n),
                grandparent: f.parent,
                great_grandparent: f.grandparent,
                target: if operand == Some(c.id()) {
                    f.target
                } else {
                    Some((n, c))
                },
                scope,
            });
        }
    }
    for occurrences in facts.names.values_mut() {
        occurrences.sort_by_key(|o| o.start);
    }
    (facts, visits)
}

/// The operand of a `TARGET_WRAPPER_KINDS` node: the last named child of
/// `type_assertion` (its first is the `type_arguments`), the first otherwise.
fn operand(wrapper: Node) -> Option<Node> {
    if wrapper.kind() == "type_assertion" {
        let last = wrapper.named_child_count().checked_sub(1)?;
        wrapper.named_child(u32::try_from(last).ok()?)
    } else {
        wrapper.named_child(0)
    }
}

/// `T` when the declarator's value is `new T(..)` with `T` a plain identifier.
fn constructor_of(source: &str, declarator: Node) -> Option<String> {
    declarator
        .child_by_field_name("value")
        .filter(|v| v.kind() == "new_expression")
        .and_then(|v| v.child_by_field_name("constructor"))
        .filter(|c| c.kind() == "identifier")
        .map(|c| text(source, c).to_string())
}

/// Records the class `n` declares under its name: a class declaration or a
/// named class expression, and a `const T = class {..}` declarator.
fn record_class(source: &str, n: Node, classes: &mut HashMap<String, Vec<ClassFacts>>) {
    let (name, class) = if CLASS_KINDS.contains(&n.kind()) {
        (n.child_by_field_name("name"), n)
    } else if n.kind() == "variable_declarator" {
        match n.child_by_field_name("value") {
            Some(v) if v.kind() == "class" => (n.child_by_field_name("name"), v),
            _ => return,
        }
    } else {
        return;
    };
    let Some(name) = name else {
        return;
    };
    classes
        .entry(text(source, name).to_string())
        .or_default()
        .push(class_facts(source, class));
}

fn class_facts(source: &str, class: Node) -> ClassFacts {
    let mut facts = ClassFacts::default();
    let mut cursor = class.walk();
    for heritage in class
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "class_heritage")
    {
        let mut inner = heritage.walk();
        for clause in heritage
            .named_children(&mut inner)
            .filter(|c| c.kind() == "extends_clause")
        {
            facts.base = match clause.child_by_field_name("value") {
                Some(v) if v.kind() == "identifier" => Base::Named(text(source, v).to_string()),
                _ => Base::Unknown,
            };
        }
    }
    let Some(body) = class.child_by_field_name("body") else {
        return facts;
    };
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        if !matches!(
            member.kind(),
            "method_definition" | "public_field_definition"
        ) {
            continue;
        }
        let Some(name) = member.child_by_field_name("name") else {
            continue;
        };
        let name = text(source, name);
        let mut tokens = member.walk();
        let is_static = member.children(&mut tokens).any(|t| t.kind() == "static");
        if is_static {
            facts.statics.insert(name.to_string());
        } else if member.kind() == "method_definition" && name == "constructor" {
            facts.ctor_returns_value |= member
                .child_by_field_name("body")
                .is_some_and(returns_value);
        }
    }
    facts
}

/// True when a `return <value>;` of `body` itself (not of a nested function or
/// class) exists.
fn returns_value(body: Node) -> bool {
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "return_statement" && n.named_child_count() > 0 {
            return true;
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            if !FUNCTION_KINDS.contains(&c.kind()) && !CLASS_KINDS.contains(&c.kind()) {
                stack.push(c);
            }
        }
    }
    false
}

/// What one occurrence of a name does.
enum Role {
    /// The `name` of a `variable_declarator` (the frame's parent).
    Declarator,
    /// A parameter, pattern, assignment target or other declaration.
    OtherBinding,
    /// A use that does not bind or write the name.
    Read,
}

fn binding_role(f: &Frame) -> Role {
    let n = f.node;
    if n.kind() == "type_identifier" {
        return match f.parent {
            Some(p) if CLASS_KINDS.contains(&p.kind()) && is_field_of(p, "name", n) => {
                Role::OtherBinding
            }
            _ => Role::Read,
        };
    }
    let Some((parent, target)) = f.target else {
        return Role::Read;
    };
    let is_field = |field: &str| is_field_of(parent, field, target);
    match parent.kind() {
        "variable_declarator" if is_field("name") => Role::Declarator,
        "assignment_expression" | "augmented_assignment_expression" if is_field("left") => {
            Role::OtherBinding
        }
        "update_expression" if is_field("argument") => Role::OtherBinding,
        "arrow_function" if is_field("parameter") => Role::OtherBinding,
        "function_expression" | "generator_function" if is_field("name") => Role::OtherBinding,
        "for_in_statement" if !is_field("left") => Role::Read,
        "catch_clause" if !is_field("parameter") => Role::Read,
        kind if BINDING_PARENT_KINDS.contains(&kind) => Role::OtherBinding,
        _ if n.kind() == "shorthand_property_identifier_pattern" => Role::OtherBinding,
        _ => Role::Read,
    }
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

fn span(n: Node) -> Span {
    (n.start_byte(), n.end_byte())
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
            (
                "function f() { let w = new A(); (<any>w) = new B(); w.run(); }",
                "type-assertion write",
            ),
            (
                "function f() { let w = new A(); (w satisfies A) = new B(); w.run(); }",
                "satisfies write",
            ),
            (
                "class A { run() {} } function f() { const w = new A(); return class w { static s = w.run(); }; }",
                "named class expression",
            ),
            (
                "class A { run() {} } const g = function A() { const w = new A(); w.run(); };",
                "constructor named by a function expression",
            ),
            (
                "class Other { run() {} } class Worker extends Other { static run() {} } function f() { const w = new Worker(); w.run(); }",
                "static member of the constructed class",
            ),
            (
                "class Worker { static run = () => 1; } function f() { const w = new Worker(); w.run(); }",
                "static field of the constructed class",
            ),
            (
                "class Other { run() {} } class Worker { constructor() { return new Other(); } run() {} } function f() { const w = new Worker(); w.run(); }",
                "constructor returns a replacement",
            ),
            (
                "class B { constructor() { return {}; } } class W extends B { run() {} } function f() { const w = new W(); w.run(); }",
                "base constructor returns a replacement",
            ),
            (
                "import { Base } from './b'; class W extends Base { run() {} } function f() { const w = new W(); w.run(); }",
                "base declared elsewhere",
            ),
            (
                "class W extends mix(A) { run() {} } function f() { const w = new W(); w.run(); }",
                "base is an expression",
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
    fn an_in_file_chain_without_replacement_carries_the_class() {
        let src = "class B { constructor() { this.x = 1; this.f = () => { return 2; }; } } \
                   class W extends B { run() {} } \
                   function f() { const w = new W(); w.run(); }";
        assert_eq!(hint_of(src, "w.run"), constructed("W"));
    }

    #[test]
    fn a_static_member_of_another_name_does_not_decline() {
        let src =
            "class W { static make() {} run() {} } function f() { const w = new W(); w.run(); }";
        assert_eq!(hint_of(src, "w.run"), constructed("W"));
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
        assert_eq!(scopes.builds, 1, "one pass per tree");
    }

    /// Review of PR #479 (Codex security finding): nested functions made every
    /// scope's table walk the whole subtree again, 23.5 s on a 270 KB file of
    /// 40 nested functions. The pass now visits each node once, whatever the
    /// nesting, and no query adds a visit.
    #[test]
    fn nested_scopes_are_not_rescanned() {
        let depth = 40;
        let bindings: String = (0..2_000).map(|i| format!("let v{i} = {i}; ")).collect();
        let mut src = String::new();
        for d in 0..depth {
            src.push_str(&format!(
                "function f{d}() {{ const w{d} = new A(); w{d}.run(); "
            ));
        }
        src.push_str(&bindings);
        src.push_str(&"}".repeat(depth));
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .expect("typescript grammar");
        let tree = parser.parse(&src, None).expect("parse");
        let mut scopes = ReceiverScopes::default();
        let mut nodes = 0;
        let mut hints = 0;
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            nodes += 1;
            if n.kind() == "call_expression"
                && !scopes.constructed_receiver_properties(&src, n).is_empty()
            {
                hints += 1;
            }
            let mut cursor = n.walk();
            for c in n.children(&mut cursor) {
                stack.push(c);
            }
        }
        assert_eq!(hints, depth, "every w<d>.run carries A");
        assert_eq!(scopes.builds, 1);
        assert_eq!(scopes.visits, nodes, "each node visited once");
    }

    #[test]
    fn reads_of_the_name_elsewhere_do_not_decline() {
        let src = "function f() { const w = new A(); log(w); const o = { w }; return w.run(); }";
        assert_eq!(hint_of(src, "w.run"), constructed("A"));
    }
}
