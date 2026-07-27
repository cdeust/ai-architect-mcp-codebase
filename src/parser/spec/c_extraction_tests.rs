// parser::spec::c_extraction_tests — targeted pins for C inline-type extraction
// that the corpus-wide parity suite leaves unexercised.
//
// Added because a mutation run over `walkers/clike.rs` (during the #123/#124 C++
// work, which refactored that file) reported a SURVIVOR:
//
//   src/parser/spec/walkers/clike.rs: replace && with || in emit_inline_type
//
// The expression is `is_anonymous && !alias.is_empty()`, which decides whether an
// inline struct is emitted under the TYPEDEF ALIAS instead of its own tag. The
// mutant survived because the parity corpus covers only the two ends of the
// matrix — an anonymous body with an alias (`typedef struct { … } Anon;`) and a
// named body with no alias (`struct Tagged { … } var;`) — and never the case where
// BOTH are present. That case is exactly where `&&` and `||` disagree, and
// `emit_typedef`'s doc comment asserts an answer for it ("`Tag` the struct and `T`
// the alias are genuinely two names") that no test checked.
//
// A surviving mutant on an unexercised behaviour is a missing test, not an
// equivalent mutant (coding-standards §12.1).

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language};

const PATH: &str = "a.c";

fn parse(src: &str) -> (Vec<ExtractedNode>, Vec<ExtractedRef>) {
    let r = parse_file(src, PATH, Language::C).expect("c parse must not hard-fail");
    assert_eq!(r.parse_errors, 0, "fixture must parse clean:\n{src}");
    (r.nodes, r.refs)
}

fn records(nodes: &[ExtractedNode]) -> Vec<String> {
    let mut out: Vec<String> = nodes
        .iter()
        .map(|n| {
            format!(
                "{}|{}|{}|{:?}",
                n.label, n.name, n.qualified_name, n.properties
            )
        })
        .collect();
    out.sort();
    out
}

fn refs_of(refs: &[ExtractedRef], kind: &str) -> Vec<String> {
    let mut out: Vec<String> = refs
        .iter()
        .filter(|r| r.kind == kind)
        .map(|r| format!("{} -> {}", r.from_qualified_name, r.to_qualified_name))
        .collect();
    out.sort();
    out
}

/// `typedef struct Tag { int x; } T;` declares TWO names: the struct tag `Tag`
/// and the alias `T`. Both must surface, the struct under its own tag — the alias
/// override exists only for an ANONYMOUS body, whose sole usable name is the
/// alias. Emitting the struct as `T` would put two nodes on one qualified name
/// (the alias `Constant` is suppressed in the anonymous case for exactly that
/// reason), which is what the surviving `||` mutant did.
#[test]
fn c_named_inline_struct_with_a_typedef_alias_keeps_both_names() {
    let (nodes, refs) = parse("typedef struct Tag { int x; } T;\n");
    assert_eq!(
        records(&nodes),
        vec![
            "Constant|T|a.c::T|[(\"typedef\", \"true\")]".to_string(),
            "Field|x|a.c::Tag::x|[(\"type_annotation\", \"int\")]".to_string(),
            "Struct|Tag|a.c::Tag|[]".to_string(),
        ],
        "the struct keeps its TAG and the alias is its own Constant"
    );
    assert_eq!(
        refs_of(&refs, "Defines"),
        vec!["a.c -> a.c::T".to_string(), "a.c -> a.c::Tag".to_string()],
        "both names are defined by the file"
    );
    assert_eq!(
        refs_of(&refs, "HasField"),
        vec!["a.c::Tag -> a.c::Tag::x".to_string()],
        "the field belongs to the tag, not to the alias"
    );
}

/// The other diagonal of the same matrix, kept next to it so the pair documents
/// the whole decision: an ANONYMOUS body with an alias IS emitted under the
/// alias, and then no separate alias `Constant` is emitted — two nodes on one
/// qualified name would be a duplicated primary key.
#[test]
fn c_anonymous_inline_struct_is_emitted_under_its_alias_only_once() {
    let (nodes, refs) = parse("typedef struct { int x; } T;\n");
    assert_eq!(
        records(&nodes),
        vec![
            "Field|x|a.c::T::x|[(\"type_annotation\", \"int\")]".to_string(),
            "Struct|T|a.c::T|[]".to_string(),
        ],
        "the alias IS the struct: one node on a.c::T, and no typedef Constant"
    );
    assert_eq!(
        refs_of(&refs, "Defines"),
        vec!["a.c -> a.c::T".to_string()],
        "exactly one Defines edge for the one name"
    );
}

/// #135 (C analog): `int (*signal_handler)(int) = 0;` at file scope is a
/// function-POINTER variable, not a prototype. Its `init_declarator` wraps a
/// `function_declarator`, so the pre-fix `is_c_function_prototype` saw the
/// function declarator and emitted a bogus prototype `Function`. A
/// `pointer_declarator` sits between that `function_declarator` and the name, so
/// it is DATA — and the flat C walker does not model file-scope variables, so it
/// emits NOTHING. A real prototype (`int add(int a);`) and a plain global
/// (`int g;`) are the controls: the first STAYS a prototype, the second was
/// already skipped.
#[test]
fn c_function_pointer_variable_is_not_a_prototype() {
    let (nodes, _refs) = parse("int (*signal_handler)(int) = 0;\nint add(int a);\nint g;\n");
    assert_eq!(
        records(&nodes),
        vec!["Function|add|a.c::add#1|[(\"is_prototype\", \"true\")]".to_string()],
        "#135: only the real prototype `add` is emitted; the function-pointer \
         variable and the plain global are not graph nodes"
    );
}

/// #135 negative control: a pointer-RETURN prototype (`int *make(int n);`) STAYS
/// a prototype. The pointer is the return type — ABOVE the `function_declarator`,
/// not between it and the name — so the discriminator must keep it callable.
#[test]
fn c_pointer_return_prototype_stays_a_prototype() {
    let (nodes, _refs) = parse("int *make(int n);\n");
    assert_eq!(
        records(&nodes),
        vec!["Function|make|a.c::make#1|[(\"is_prototype\", \"true\")]".to_string()],
        "#135: a pointer-return prototype is still a callable prototype"
    );
}
