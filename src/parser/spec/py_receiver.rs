// parser::spec::py_receiver — the type of a Python local receiver that the
// enclosing function states by construction (issue #444).
//
// `w = Worker()` then `w.run(3)`: the callee reaches the resolver as `w.run`,
// bound by name only, so with two classes owning `run` the call stayed
// unresolved. This is the Python twin of `ts_receiver`: it attaches
// `receiver_hint = T` with `receiver_hint_via = constructed`, limited to what
// the file proves on its own.
//
// - The receiver is a plain identifier `x` and the call sits in a function
//   (`def` or `lambda`); module-level code gets no hint, a global can be
//   rebound from anywhere.
// - In the call's nearest enclosing function, the only occurrence of `x` that
//   is not a plain read is one statement `x = T(..)` with `T` a plain
//   identifier. A name assigned anywhere in a Python function is local to the
//   whole function (reference: "Naming and binding", section 4.2.2), so a
//   call that runs at all runs after that one binding, wherever it is written.
//   Any other occurrence declines: a second assignment, an augmented one, a
//   parameter, a `for`/`with`/`except`/`import` target, a comprehension
//   variable, `:=`, `del`, `global`/`nonlocal`, a nested `def` or `class` of
//   that name, a `match` capture.
// - `T` is the class of this file and nothing else: the file binds `T` once,
//   by a `class T` statement, no function enclosing the call binds it, and the
//   file has no `from m import *`.
// - `T(..)` builds an instance of `T` and `x.m` is `T`'s own `m`: neither `T`
//   nor a class on its base chain is decorated, takes a class keyword
//   (`metaclass=`), or binds `__new__` or `__getattribute__`; every base is a
//   class this file declares once (a base it cannot see declines); `T`'s body
//   binds `m` exactly once; and no statement of the file writes an attribute
//   named `m` or `__class__` (`self.run = fn`, `Worker.run = fn`).
//
// "Not a plain read" is decided by an allowlist of read positions
// (`is_read`): an occurrence in a position the list does not name counts as a
// binding. A grammar construct this module has never heard of therefore
// declines instead of being read through. Reflection (`setattr`, `globals()`,
// `exec`) is not seen, as in `ts_receiver`.
//
// Cost: ONE depth-first pass over the tree, on the first query, cached in the
// walk context; a query is hash lookups and binary searches.
//
// A wrong hint would bind a call to a method it does not reach (a wrong single
// edge is worse than none), which is why every doubtful case declines.
//
// source: tree-sitter-python 0.25.0 node-types.json — `call.function`,
// `attribute.object`/`attribute`, `assignment.left`/`right`,
// `class_definition.name`/`superclasses`, `decorated_definition.definition`,
// `function_definition`, `lambda`, `delete_statement`, `wildcard_import`, and
// the kinds and field names listed in `READ_FIELDS` and `READ_PARENT_KINDS`.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::graph_store::RECEIVER_HINT_VIA_CONSTRUCTED;

/// Node kinds that open a function scope.
const FUNCTION_KINDS: &[&str] = &["function_definition", "lambda"];

/// Names whose binding on a class or one of its bases means `T(..)` may not
/// build a plain instance of `T`, or `x.m` may not reach `T.m`.
/// source: Python data model, sections 3.3.1 (`__new__`) and 3.3.2
/// (`__getattribute__`).
const INSTANCE_HOOKS: &[&str] = &["__new__", "__getattribute__"];

/// The attribute whose assignment changes an instance's class.
/// source: Python data model, section 3.2 ("Special attributes").
const CLASS_ATTRIBUTE: &str = "__class__";

/// A byte span `[start, end)`.
type Span = (usize, usize);

/// Per-tree cache of the facts the hint reads, built in one pass on the first
/// query. Valid for ONE tree only: node ids are unique within a tree.
#[derive(Default)]
pub(crate) struct PyReceiverScopes {
    facts: Option<FileFacts>,
}

#[derive(Default)]
struct FileFacts {
    /// Every occurrence of a name that is not a plain read, sorted by start.
    names: HashMap<String, Vec<Occurrence>>,
    /// Call node id → (innermost, outermost) enclosing function span. Calls
    /// outside every function are absent.
    calls: HashMap<usize, (Span, Span)>,
    /// The classes the file declares, by name.
    classes: HashMap<String, Vec<ClassFacts>>,
    /// Attribute names some statement writes or deletes (`a.m = ..`).
    attribute_writes: HashSet<String>,
    /// The file has a `from m import *`: any name may be bound by it.
    star_import: bool,
}

/// One occurrence of a name that is not a plain read.
struct Occurrence {
    start: usize,
    /// `T` when the occurrence is the target of a statement `x = T(..)`.
    constructor: Option<String>,
}

struct ClassFacts {
    span: Span,
    decorated: bool,
    /// The base names; `None` when a base is not a plain identifier or the
    /// class statement takes a keyword (`metaclass=..`).
    bases: Option<Vec<String>>,
}

impl PyReceiverScopes {
    /// The `receiver_hint` and `receiver_hint_via` properties of the call
    /// `call_node`, or none. See the module header for the rule.
    ///
    /// postcondition: empty unless the receiver's type is a `T(..)` the
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

    fn constructed_receiver_type(&mut self, source: &str, call_node: Node) -> Option<String> {
        let function = call_node.child_by_field_name("function")?;
        if function.kind() != "attribute" {
            return None;
        }
        let object = function.child_by_field_name("object")?;
        if object.kind() != "identifier" {
            return None;
        }
        let method = text(source, function.child_by_field_name("attribute")?);
        let name = text(source, object);
        let facts = self.facts(source, call_node);
        if facts.star_import {
            return None;
        }
        let &(inner, outer) = facts.calls.get(&call_node.id())?;
        let [only] = within(facts.names.get(name)?, inner) else {
            return None;
        };
        let ty = only.constructor.as_deref()?;
        if !facts.is_the_file_class(ty, outer) || !facts.builds_instance_with_own(ty, method) {
            return None;
        }
        Some(ty.to_string())
    }

    fn facts(&mut self, source: &str, call_node: Node) -> &FileFacts {
        if self.facts.is_none() {
            // Once per tree: the climb to the root is paid on the first query.
            let mut root = call_node;
            while let Some(p) = root.parent() {
                root = p;
            }
            self.facts = Some(build_facts(source, root));
        }
        self.facts.get_or_insert_with(FileFacts::default)
    }
}

impl FileFacts {
    fn occurrences(&self, name: &str) -> &[Occurrence] {
        self.names.get(name).map_or(&[][..], Vec::as_slice)
    }

    /// The file's one declaration of a class `name`, when the file binds
    /// `name` once and that binding is the class statement.
    fn sole_class(&self, name: &str) -> Option<&ClassFacts> {
        match (self.occurrences(name), self.classes.get(name)?.as_slice()) {
            ([_], [class]) => Some(class),
            _ => None,
        }
    }

    /// True when `ty` at the call names the class this file declares: the
    /// file binds it once, by its class statement, and the outermost function
    /// enclosing the call (every enclosing one lies inside it) does not.
    fn is_the_file_class(&self, ty: &str, outer: Span) -> bool {
        self.sole_class(ty).is_some() && within(self.occurrences(ty), outer).is_empty()
    }

    /// True when `ty(..)` builds a plain instance of `ty` and `.m` on it is
    /// the `m` that `ty`'s own body binds once. See the module header.
    fn builds_instance_with_own(&self, ty: &str, m: &str) -> bool {
        if self.attribute_writes.contains(m) || self.attribute_writes.contains(CLASS_ATTRIBUTE) {
            return false;
        }
        let Some(own) = self.sole_class(ty) else {
            return false;
        };
        if within(self.occurrences(m), own.span).len() != 1 {
            return false;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut pending = vec![ty];
        while let Some(name) = pending.pop() {
            if !seen.insert(name) {
                // A cycle cannot be a class chain.
                return false;
            }
            let Some(class) = self.sole_class(name) else {
                return false;
            };
            let hooked = INSTANCE_HOOKS
                .iter()
                .any(|hook| !within(self.occurrences(hook), class.span).is_empty());
            if class.decorated || hooked {
                return false;
            }
            match &class.bases {
                Some(bases) => pending.extend(bases.iter().map(String::as_str)),
                None => return false,
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

/// One node of the pass, with what its parent says about it.
struct Frame<'t> {
    node: Node<'t>,
    parent_kind: &'t str,
    /// The field of the parent this node fills, if any.
    field: Option<&'t str>,
    /// The node sits under a `del` statement.
    deleted: bool,
    /// (innermost, outermost) function span enclosing the node.
    scope: Option<(Span, Span)>,
}

/// The facts of the tree under `root`, in one depth-first pass.
fn build_facts(source: &str, root: Node) -> FileFacts {
    let mut facts = FileFacts::default();
    // Target identifier node id → `T`, for each statement `x = T(..)`; filled
    // when the statement is visited, read when its target is (a child, later).
    let mut constructed: HashMap<usize, String> = HashMap::new();
    let mut stack = vec![Frame {
        node: root,
        parent_kind: "",
        field: None,
        deleted: false,
        scope: None,
    }];
    while let Some(f) = stack.pop() {
        record(source, &f, &mut facts, &mut constructed);
        let n = f.node;
        let scope = if FUNCTION_KINDS.contains(&n.kind()) {
            let s = span(n);
            Some((s, f.scope.map_or(s, |(_, outer)| outer)))
        } else {
            f.scope
        };
        let deleted = f.deleted || n.kind() == "delete_statement";
        let mut cursor = n.walk();
        if !cursor.goto_first_child() {
            continue;
        }
        loop {
            stack.push(Frame {
                node: cursor.node(),
                parent_kind: n.kind(),
                field: cursor.field_name(),
                deleted,
                scope,
            });
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    for occurrences in facts.names.values_mut() {
        occurrences.sort_by_key(|o| o.start);
    }
    facts
}

/// Records what the node of `f` contributes to `facts`.
fn record(
    source: &str,
    f: &Frame,
    facts: &mut FileFacts,
    constructed: &mut HashMap<usize, String>,
) {
    let n = f.node;
    let read = is_read(f.parent_kind, f.field) && !f.deleted;
    match n.kind() {
        "identifier" if !read && !names_no_variable(f) => {
            let name = text(source, n).to_string();
            facts.names.entry(name).or_default().push(Occurrence {
                start: n.start_byte(),
                constructor: constructed.remove(&n.id()),
            });
        }
        "attribute" if !read => {
            if let Some(attribute) = n.child_by_field_name("attribute") {
                facts
                    .attribute_writes
                    .insert(text(source, attribute).to_string());
            }
        }
        "assignment" if f.parent_kind == "expression_statement" => {
            if let Some((target, ty)) = constructed_target(source, n) {
                constructed.insert(target, ty);
            }
        }
        "call" => {
            if let Some(scope) = f.scope {
                facts.calls.insert(n.id(), scope);
            }
        }
        "class_definition" => record_class(source, n, f, &mut facts.classes),
        "wildcard_import" => facts.star_import = true,
        _ => {}
    }
}

/// True when a node in `field` of a `parent_kind` node is evaluated for its
/// value and nothing else: it is not bound, written, deleted or declared
/// there. The list is an allowlist: a position it does not name is not a read.
fn is_read(parent_kind: &str, field: Option<&str>) -> bool {
    READ_FIELDS.contains(&(parent_kind, field.unwrap_or("")))
        || READ_PARENT_KINDS.contains(&parent_kind)
}

/// (parent kind, field) pairs under which a node is only evaluated.
const READ_FIELDS: &[(&str, &str)] = &[
    ("attribute", "object"),
    ("call", "function"),
    ("keyword_argument", "value"),
    ("assignment", "right"),
    ("augmented_assignment", "right"),
    ("named_expression", "value"),
    ("subscript", "value"),
    ("subscript", "subscript"),
    ("pair", "key"),
    ("pair", "value"),
    ("if_statement", "condition"),
    ("elif_clause", "condition"),
    ("while_statement", "condition"),
    ("for_statement", "right"),
    ("for_in_clause", "right"),
    ("with_item", "value"),
    ("lambda", "body"),
    ("list_comprehension", "body"),
    ("set_comprehension", "body"),
    ("generator_expression", "body"),
];

/// Parent kinds every child of which is only evaluated.
const READ_PARENT_KINDS: &[&str] = &[
    "argument_list",
    "return_statement",
    "expression_statement",
    "expression_list",
    "tuple",
    "list",
    "set",
    "parenthesized_expression",
    "binary_operator",
    "boolean_operator",
    "comparison_operator",
    "not_operator",
    "unary_operator",
    "conditional_expression",
    "await",
    "yield",
    "slice",
    "assert_statement",
    "raise_statement",
    "if_clause",
    "interpolation",
    "list_splat",
    "dictionary_splat",
    "type",
    "generic_type",
];

/// True when the identifier names no variable: the attribute name of
/// `a.name` (its writes are recorded on the `attribute` node) or the keyword
/// of an argument `f(name=..)`.
fn names_no_variable(f: &Frame) -> bool {
    matches!(
        (f.parent_kind, f.field),
        ("attribute", Some("attribute")) | ("keyword_argument", Some("name"))
    )
}

/// (target identifier node id, `T`) when `assignment` is `x = T(..)` with `x`
/// and `T` plain identifiers.
fn constructed_target(source: &str, assignment: Node) -> Option<(usize, String)> {
    let target = assignment.child_by_field_name("left")?;
    let value = assignment.child_by_field_name("right")?;
    if target.kind() != "identifier" || value.kind() != "call" {
        return None;
    }
    let constructor = value.child_by_field_name("function")?;
    if constructor.kind() != "identifier" {
        return None;
    }
    Some((target.id(), text(source, constructor).to_string()))
}

/// Records the class statement `n` under its name.
fn record_class(source: &str, n: Node, f: &Frame, classes: &mut HashMap<String, Vec<ClassFacts>>) {
    let Some(name) = n.child_by_field_name("name") else {
        return;
    };
    let bases = match n.child_by_field_name("superclasses") {
        None => Some(Vec::new()),
        Some(list) => {
            let mut cursor = list.walk();
            list.named_children(&mut cursor)
                .map(|base| (base.kind() == "identifier").then(|| text(source, base).to_string()))
                .collect()
        }
    };
    classes
        .entry(text(source, name).to_string())
        .or_default()
        .push(ClassFacts {
            span: span(n),
            decorated: f.parent_kind == "decorated_definition",
            bases,
        });
}

fn span(n: Node) -> Span {
    (n.start_byte(), n.end_byte())
}

fn text<'s>(source: &'s str, n: Node) -> &'s str {
    source.get(n.start_byte()..n.end_byte()).unwrap_or("")
}

#[cfg(test)]
#[path = "py_receiver_tests.rs"]
mod tests;
