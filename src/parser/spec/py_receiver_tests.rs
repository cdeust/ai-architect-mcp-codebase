// parser::spec::py_receiver_tests — the hint is exactly what the file proves
// (issue #444): one case per rule of the module header, each declining form
// with the reason it declines.

use super::*;

/// The hint of the first call spelled `callee` in `src`.
fn hint_of(src: &str, callee: &str) -> Vec<(String, String)> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("python grammar");
    let tree = parser.parse(src, None).expect("parse");
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == "call"
            && n.child_by_field_name("function")
                .is_some_and(|f| text(src, f) == callee)
        {
            return PyReceiverScopes::default().constructed_receiver_properties(src, n);
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

const WORKER: &str = "class Worker:\n    def run(self, n):\n        return n\n\n";

/// `WORKER` followed by a function `f` whose body is `body` (one statement
/// per line, indented here).
fn in_function(body: &[&str]) -> String {
    let mut src = format!("{WORKER}def f(arg):\n");
    for line in body {
        src.push_str("    ");
        src.push_str(line);
        src.push('\n');
    }
    src
}

#[test]
fn a_local_bound_once_to_a_class_of_the_file_carries_the_class() {
    for body in [
        vec!["w = Worker()", "w.run(3)"],
        // Reads of the local and of the class do not unbind either.
        vec!["w = Worker()", "print(w, Worker)", "return w.run(3)"],
        vec!["w = Worker()", "use(key=w)", "w.count = 1", "w.run(3)"],
        vec!["w: Worker = Worker()", "w.run(3)"],
        // One binding in the function: the call cannot run before it.
        vec!["if arg:", "    w = Worker()", "    w.run(3)"],
        vec!["w = Worker()", "return [w.run(i) for i in arg]"],
        vec!["w = Worker()", "with w:", "    w.run(3)"],
    ] {
        assert_eq!(
            hint_of(&in_function(&body), "w.run"),
            constructed("Worker"),
            "{body:?}"
        );
    }
}

#[test]
fn an_annotation_naming_the_class_is_a_read_of_it() {
    let src =
        format!("{WORKER}def f(other: Worker) -> list[Worker]:\n    w = Worker()\n    w.run(1)\n");
    assert_eq!(hint_of(&src, "w.run"), constructed("Worker"));
}

#[test]
fn a_local_the_function_binds_in_any_other_way_carries_nothing() {
    for (body, why) in [
        (vec!["w = Worker()", "w = other()", "w.run(3)"], "rebound"),
        (vec!["w = Worker()", "w += 1", "w.run(3)"], "augmented"),
        (vec!["w = make()", "w.run(3)"], "not a plain constructor"),
        (
            vec!["w = mod.Worker()", "w.run(3)"],
            "qualified constructor",
        ),
        (vec!["w = Worker", "w.run(3)"], "the class, not an instance"),
        (vec!["w = v = Worker()", "w.run(3)"], "chained assignment"),
        (vec!["w, v = Worker(), 1", "w.run(3)"], "tuple target"),
        (
            vec!["w = Worker()", "for w in arg:", "    w.run(3)"],
            "loop target",
        ),
        (
            vec!["w = Worker()", "with arg as w:", "    w.run(3)"],
            "with target",
        ),
        (
            vec![
                "w = Worker()",
                "try:",
                "    pass",
                "except E as w:",
                "    w.run(3)",
            ],
            "except target",
        ),
        (
            vec!["w = Worker()", "import w", "w.run(3)"],
            "import target",
        ),
        (
            vec!["w = Worker()", "if (w := arg):", "    w.run(3)"],
            "walrus",
        ),
        (vec!["w = Worker()", "del w", "w.run(3)"], "deleted"),
        (
            vec!["global w", "w = Worker()", "w.run(3)"],
            "declared global",
        ),
        (
            vec!["w = Worker()", "return [w.run(1) for w in arg]"],
            "comprehension variable",
        ),
        (
            vec![
                "w = Worker()",
                "def inner():",
                "    nonlocal w",
                "    w = 1",
                "w.run(3)",
            ],
            "rebound by a nested function",
        ),
        (
            vec!["w = Worker()", "return lambda: w.run(3)"],
            "call in a nested function",
        ),
        (
            vec!["w = Worker()", "def w(): pass", "w.run(3)"],
            "nested def of that name",
        ),
        (
            vec![
                "w = Worker()",
                "match arg:",
                "    case w:",
                "        w.run(3)",
            ],
            "match capture",
        ),
    ] {
        assert_eq!(hint_of(&in_function(&body), "w.run"), vec![], "{why}");
    }
}

#[test]
fn a_parameter_carries_nothing() {
    assert_eq!(hint_of(&in_function(&["arg.run(3)"]), "arg.run"), vec![]);
    let src = format!("{WORKER}def f(w=Worker()):\n    w.run(3)\n");
    assert_eq!(hint_of(&src, "w.run"), vec![]);
}

#[test]
fn a_call_at_module_level_carries_nothing() {
    let src = format!("{WORKER}w = Worker()\nw.run(3)\n");
    assert_eq!(hint_of(&src, "w.run"), vec![]);
}

#[test]
fn a_constructor_name_that_is_not_the_file_class_carries_nothing() {
    let use_it = "def f():\n    w = Worker()\n    w.run(3)\n";
    for (src, why) in [
        (format!("from workers import Worker\n{use_it}"), "imported"),
        (format!("{WORKER}Worker = other\n{use_it}"), "rebound in the file"),
        (
            format!("{WORKER}from other import Worker\n{use_it}"),
            "import of the same name",
        ),
        (format!("{WORKER}{WORKER}{use_it}"), "declared twice"),
        (format!("{WORKER}from other import *\n{use_it}"), "star import"),
        (
            format!("{WORKER}def f(Worker):\n    w = Worker()\n    w.run(3)\n"),
            "a parameter of that name",
        ),
        (
            format!(
                "{WORKER}def outer():\n    Worker = other\n    def f():\n        w = Worker()\n        w.run(3)\n"
            ),
            "rebound by an enclosing function",
        ),
        (
            format!("def Worker():\n    return 1\n{use_it}"),
            "a function, not a class",
        ),
    ] {
        assert_eq!(hint_of(&src, "w.run"), vec![], "{why}");
    }
}

#[test]
fn a_class_that_may_not_build_a_plain_instance_carries_nothing() {
    let use_it = "def f():\n    w = Worker()\n    w.run(3)\n";
    let body = "    def run(self, n):\n        return n\n";
    for (class, why) in [
        (format!("@registered\nclass Worker:\n{body}"), "decorated"),
        (
            format!("class Worker(metaclass=Meta):\n{body}"),
            "metaclass",
        ),
        (
            format!("class Worker:\n{body}    def __new__(cls):\n        return other()\n"),
            "__new__",
        ),
        (
            format!("class Worker:\n{body}    def __getattribute__(self, k):\n        return other\n"),
            "__getattribute__",
        ),
        (
            format!("from base import Base\nclass Worker(Base):\n{body}"),
            "a base outside the file",
        ),
        (
            format!("class Worker(pkg.Base):\n{body}"),
            "a base that is not a name",
        ),
        (
            format!(
                "class Base:\n    def __new__(cls):\n        return other()\nclass Worker(Base):\n{body}"
            ),
            "__new__ on a base",
        ),
        (
            "class Worker:\n    def stop(self):\n        pass\n".to_string(),
            "the method is not the class's own",
        ),
        (
            format!("class Worker:\n{body}    run = other\n"),
            "the method is bound twice",
        ),
        (
            format!("class Worker:\n{body}    def __init__(self):\n        self.run = other\n"),
            "an instance attribute of that name",
        ),
        (
            format!("class Worker:\n{body}Worker.run = other\n"),
            "patched from outside",
        ),
        (
            format!("class Worker:\n{body}    def swap(self):\n        self.__class__ = Other\n"),
            "__class__ written",
        ),
    ] {
        assert_eq!(hint_of(&format!("{class}{use_it}"), "w.run"), vec![], "{why}");
    }
}

#[test]
fn a_base_class_of_the_file_without_hooks_keeps_the_hint() {
    let src = "class Base:\n    def stop(self):\n        pass\n\
               class Worker(Base):\n    def run(self):\n        self.run()\n\
               def f():\n    w = Worker()\n    w.run()\n";
    assert_eq!(hint_of(src, "w.run"), constructed("Worker"));
}

/// Every node kind and field name the module matches on exists in the
/// grammar it is compiled against: a renamed kind would silently never match.
#[test]
fn every_kind_and_field_the_module_names_exists_in_the_grammar() {
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let kinds = READ_PARENT_KINDS
        .iter()
        .chain(FUNCTION_KINDS)
        .copied()
        .chain(READ_FIELDS.iter().map(|(kind, _)| *kind))
        .chain([
            "identifier",
            "attribute",
            "assignment",
            "call",
            "class_definition",
            "decorated_definition",
            "delete_statement",
            "wildcard_import",
        ]);
    for kind in kinds {
        assert_ne!(language.id_for_node_kind(kind, true), 0, "kind {kind}");
    }
    let fields = READ_FIELDS.iter().map(|(_, field)| *field).chain([
        "function",
        "object",
        "attribute",
        "left",
        "right",
        "name",
        "superclasses",
    ]);
    for field in fields {
        assert!(language.field_id_for_name(field).is_some(), "field {field}");
    }
}
