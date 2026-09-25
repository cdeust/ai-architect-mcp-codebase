// parser::spec::rust_cfg_twin_tests: issue #353. Two items of one name under
// mutually exclusive `#[cfg]` predicates are two nodes, each named by its gate;
// an item that has no twin keeps its plain qualified name.
//
// source: issue #353, measured on the 0.13.0 release: `#[cfg(feature = "fast")]
// fn pick` beside `#[cfg(not(feature = "fast"))] fn pick` gave ONE `pick` node
// (the first), and every call to `pick` resolved to it at 0.95.

use crate::parser::{parse_file, ExtractedNode, Language, ParseResult};

const FILE: &str = "src/lib.rs";

fn parse(source: &str) -> ParseResult {
    parse_file(source, FILE, Language::Rust).expect("parse")
}

fn nodes<'a>(result: &'a ParseResult, label: &str, name: &str) -> Vec<&'a ExtractedNode> {
    result
        .nodes
        .iter()
        .filter(|n| n.label == label && n.name == name)
        .collect()
}

fn prop(node: &ExtractedNode, key: &str) -> Option<String> {
    node.properties
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

const TWIN_FNS: &str = "#[cfg(feature = \"fast\")]\npub fn pick() -> u32 {\n    1\n}\n\n\
                        #[cfg(not(feature = \"fast\"))]\npub fn pick() -> u32 {\n    2\n}\n\n\
                        pub fn caller() -> u32 {\n    pick()\n}\n";

/// The issue's reproduction: before the fix one `pick` node existed, spanning
/// lines 2 to 4, and the twin under `not(feature = "fast")` had none.
#[test]
fn twin_functions_are_two_nodes_each_named_by_its_gate() {
    let result = parse(TWIN_FNS);
    let picks = nodes(&result, "Function", "pick");
    let mut qns: Vec<&str> = picks.iter().map(|n| n.qualified_name.as_str()).collect();
    qns.sort_unstable();
    assert_eq!(
        qns,
        [
            "src/lib.rs::pick#cfg(feature=fast)",
            "src/lib.rs::pick#cfg(not(feature=fast))"
        ]
    );
    let gates: Vec<Option<String>> = picks.iter().map(|n| prop(n, "cfg_gate")).collect();
    assert!(
        gates.contains(&Some("feature=fast".to_string())),
        "{gates:?}"
    );
    assert!(
        gates.contains(&Some("not(feature=fast)".to_string())),
        "{gates:?}"
    );
}

/// The twin nodes keep the item's own lines, so each still points at its source.
#[test]
fn each_twin_keeps_its_own_lines() {
    let result = parse(TWIN_FNS);
    let by_gate = |gate: &str| {
        nodes(&result, "Function", "pick")
            .into_iter()
            .find(|n| prop(n, "cfg_gate").as_deref() == Some(gate))
            .map(|n| (n.start_line, n.end_line))
    };
    assert_eq!(by_gate("feature=fast"), Some((2, 4)));
    assert_eq!(by_gate("not(feature=fast)"), Some((7, 9)));
}

/// An item with no twin, gated or not, keeps its plain id and gets no property:
/// the output of a file without twins is what it always was.
#[test]
fn an_item_without_a_twin_keeps_its_plain_qualified_name() {
    let result = parse(TWIN_FNS);
    let caller = &nodes(&result, "Function", "caller")[0];
    assert_eq!(caller.qualified_name, "src/lib.rs::caller");
    assert_eq!(prop(caller, "cfg_gate"), None);

    let gated_alone = parse("#[cfg(test)]\nmod tests {\n    fn helper() {}\n}\nfn helper() {}\n");
    for n in nodes(&gated_alone, "Function", "helper") {
        assert!(!n.qualified_name.contains("#cfg("), "{}", n.qualified_name);
    }
}

/// A call site in a twin-free position still names its caller by the plain id;
/// the call site of `caller` to `pick` carries no discriminator of its own.
#[test]
fn the_call_site_of_a_caller_without_a_gate_has_a_plain_caller() {
    let result = parse(TWIN_FNS);
    let sites: Vec<&ExtractedNode> = result
        .nodes
        .iter()
        .filter(|n| n.label == "CallSite" && prop(n, "callee_name").as_deref() == Some("pick"))
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(
        prop(sites[0], "caller_qn").as_deref(),
        Some("src/lib.rs::caller")
    );
}

/// Duplicates under the SAME gate are not twins: they keep today's behaviour
/// (one qualified name, the persistence layer keeps the first).
#[test]
fn duplicates_under_the_same_gate_are_not_renamed() {
    let result = parse("#[cfg(unix)]\nfn f() {}\n#[cfg(unix)]\nfn f() {}\n");
    let fs = nodes(&result, "Function", "f");
    assert_eq!(fs.len(), 2);
    assert!(fs.iter().all(|n| n.qualified_name == "src/lib.rs::f"));
}

/// Twin methods of one `impl`, and twin methods across two gated `impl` blocks.
#[test]
fn twin_methods_are_told_apart_in_one_impl_and_across_two_impls() {
    let one = parse(
        "struct S;\nimpl S {\n    #[cfg(unix)]\n    fn m(&self) {}\n    #[cfg(not(unix))]\n    fn m(&self) {}\n}\n",
    );
    let mut qns: Vec<&str> = nodes(&one, "Method", "m")
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();
    qns.sort_unstable();
    assert_eq!(
        qns,
        [
            "src/lib.rs::S::m#cfg(not(unix))",
            "src/lib.rs::S::m#cfg(unix)"
        ]
    );

    let two = parse(
        "struct S;\n#[cfg(unix)]\nimpl S {\n    fn m(&self) {}\n}\n#[cfg(not(unix))]\nimpl S {\n    fn m(&self) {}\n}\n",
    );
    let mut qns: Vec<&str> = nodes(&two, "Method", "m")
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();
    qns.sort_unstable();
    assert_eq!(
        qns,
        [
            "src/lib.rs::S::m#cfg(not(unix))",
            "src/lib.rs::S::m#cfg(unix)"
        ]
    );
}

/// Twin structs are two nodes, and the fields of each hang under its own twin.
#[test]
fn twin_structs_are_two_nodes_and_their_fields_follow_them() {
    let result =
        parse("#[cfg(unix)]\nstruct S { a: u8 }\n#[cfg(not(unix))]\nstruct S { a: u16 }\n");
    let mut qns: Vec<&str> = nodes(&result, "Struct", "S")
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();
    qns.sort_unstable();
    assert_eq!(
        qns,
        ["src/lib.rs::S#cfg(not(unix))", "src/lib.rs::S#cfg(unix)"]
    );
    let mut fields: Vec<&str> = nodes(&result, "Field", "a")
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "src/lib.rs::S#cfg(not(unix))::a",
            "src/lib.rs::S#cfg(unix)::a"
        ]
    );
}

/// The Kani case: the same constant under `cfg(kani)` and `cfg(not(kani))`.
#[test]
fn twin_constants_under_cfg_kani_stay_distinct() {
    let result = parse(
        "#[cfg(kani)]\npub const MAX_TASKS: usize = 4;\n#[cfg(not(kani))]\npub const MAX_TASKS: usize = 64;\n",
    );
    let mut qns: Vec<&str> = nodes(&result, "Constant", "MAX_TASKS")
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();
    qns.sort_unstable();
    assert_eq!(
        qns,
        [
            "src/lib.rs::MAX_TASKS#cfg(kani)",
            "src/lib.rs::MAX_TASKS#cfg(not(kani))"
        ]
    );
}

/// A twin inline module passes its suffix into its children, so the children
/// of the two modules are distinct without a discriminator of their own.
#[test]
fn twin_inline_modules_scope_their_children_under_their_own_suffix() {
    let result = parse(
        "#[cfg(unix)]\nmod m {\n    pub fn f() {}\n}\n#[cfg(not(unix))]\nmod m {\n    pub fn f() {}\n}\n",
    );
    let mut fns: Vec<&str> = nodes(&result, "Function", "f")
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();
    fns.sort_unstable();
    assert_eq!(
        fns,
        [
            "src/lib.rs::m#cfg(not(unix))::f",
            "src/lib.rs::m#cfg(unix)::f"
        ]
    );
    let mut mods: Vec<&str> = nodes(&result, "Module", "m")
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();
    mods.sort_unstable();
    assert_eq!(
        mods,
        ["src/lib.rs::m#cfg(not(unix))", "src/lib.rs::m#cfg(unix)"]
    );
}

/// A derive on a twin struct is attached to that twin's id.
#[test]
fn a_derive_on_a_twin_struct_names_the_twin() {
    let result = parse(
        "#[cfg(unix)]\n#[derive(Debug)]\nstruct S;\n#[cfg(not(unix))]\n#[derive(Clone)]\nstruct S;\n",
    );
    let from: Vec<&str> = result
        .refs
        .iter()
        .filter(|r| r.kind == "DeriveImplements")
        .map(|r| r.from_qualified_name.as_str())
        .collect();
    assert_eq!(from.len(), 2);
    assert!(from.iter().all(|f| f.contains("#cfg(")), "{from:?}");
}

/// The suffix never breaks the id parsers: no `::`, no `.`, no trailing digits.
#[test]
fn a_twin_id_holds_no_id_separator_inside_its_suffix() {
    let result = parse(
        "#[cfg(target_os = \"a.b\")]\nfn f() {}\n#[cfg(not(target_os = \"a.b\"))]\nfn f() {}\n",
    );
    for n in nodes(&result, "Function", "f") {
        let suffix = n
            .qualified_name
            .rsplit_once("#cfg(")
            .map(|(_, tail)| tail.to_string())
            .expect("a twin id");
        assert!(
            !suffix.contains("::") && !suffix.contains('.') && !suffix.contains('#'),
            "{suffix}"
        );
        assert!(n.qualified_name.ends_with(')'), "{}", n.qualified_name);
    }
}
