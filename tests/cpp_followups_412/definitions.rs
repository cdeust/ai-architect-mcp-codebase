// cpp_followups_412::definitions: C++ definitions the walker names. A conversion
// function (`operator int() const`) is a Method named `operator` and its type, and the
// calls of its body have it as their caller. An enumerator of an `enum class` is kept
// when the mask blanks the macro statements of a class in the same file.

use super::*;

/// `qualified_name` without its file prefix and its `#n` suffix.
fn plain(qn: &str) -> String {
    let name = qn.split_once("::").map_or(qn, |(_, q)| q);
    name.rsplit_once('#').map_or(name, |(n, _)| n).to_string()
}

/// The ids of the nodes of `label`, as plain qualified names, sorted.
fn plain_names(store: &GraphStore, label: &str) -> Vec<String> {
    let mut out: Vec<String> = store
        .execute_query(&format!("MATCH (n:{label}) RETURN n.id"))
        .expect("query ids")
        .rows
        .into_iter()
        .map(|r| plain(&r[0]))
        .collect();
    out.sort();
    out
}

/// The callers of the call sites named `callee`, as plain qualified names, sorted.
fn callers_of(store: &GraphStore, callee: &str) -> Vec<String> {
    let mut out: Vec<String> = store
        .execute_query(&format!(
            "MATCH (cs:CallSite) WHERE cs.callee_name = '{callee}' RETURN cs.id"
        ))
        .expect("query sites")
        .rows
        .into_iter()
        .map(|r| plain(r[0].rsplit_once("::call@").map_or(&r[0][..], |(c, _)| c)))
        .collect();
    out.sort();
    out
}

/// `src` defines the conversion function `S::{name}`, a Method and no Field, and it
/// is the caller of every `f` site.
fn assert_conversion_function(src: &str, name: &str) {
    let (store, _tmp) = index_and_resolve(&[("a.cpp", src)]);
    let method = format!("S::{name}");
    let methods = plain_names(&store, "Method");
    assert!(
        methods.contains(&method),
        "{method} not in {methods:?} {:?}: {src}",
        plain_names(&store, "Function")
    );
    let fields = plain_names(&store, "Field");
    assert!(
        !fields.iter().any(|f| f.contains("operator")),
        "{fields:?}: {src}"
    );
    let callers = callers_of(&store, "f");
    assert!(
        !callers.is_empty() && callers.iter().all(|c| *c == method),
        "{callers:?}: {src}"
    );
}

#[test]
fn a_conversion_function_is_a_method_named_by_its_type_and_the_caller_of_its_body() {
    for (src, name) in [
        ("struct S { int f(); operator int() const { return f(); } };", "operator int"),
        ("struct S { int f(); operator bool() const { return f(); } };", "operator bool"),
        (
            "struct S { int f(); explicit operator S*() { f(); return this; } };",
            "operator S*",
        ),
        (
            "namespace std { struct string {}; }\nstruct S { int f(); operator std::string() const { f(); return {}; } };",
            "operator std::string",
        ),
        (
            "struct S { int f(); template <class U> operator U() const { f(); return U(); } };",
            "operator U",
        ),
        (
            "struct S { int f(); operator const char*() const { f(); return \"\"; } };",
            "operator const char*",
        ),
        (
            "struct S { int f(); operator unsigned int() const { return f(); } };",
            "operator unsigned int",
        ),
        (
            "struct S { int f(); operator S&() { f(); return *this; } };",
            "operator S&",
        ),
        (
            "struct S { int f(); operator int() const; };\nS::operator int() const { return f(); }",
            "operator int",
        ),
    ] {
        assert_conversion_function(src, name);
    }
}

/// A conversion function declared in its class is a prototype Method, not a Field.
#[test]
fn a_conversion_function_declared_in_its_class_is_a_method() {
    let src = "struct S { operator bool() const; explicit operator S*(); };";
    let (store, _tmp) = index_and_resolve(&[("a.cpp", src)]);
    let methods = plain_names(&store, "Method");
    assert_eq!(methods, ["S::operator S*", "S::operator bool"], "{src}");
    assert!(plain_names(&store, "Field").is_empty(), "{src}");
}

/// `enum class` holds `class`, but its body is no class body: an enumerator alone in
/// it is no macro statement, whatever its case.
#[test]
fn an_enumerator_of_an_enum_class_survives_the_mask_of_a_class_macro_statement() {
    let src = "template <int N> class K {\npublic:\n  ETL_STATIC_ASSERT((N > 0U), \"zero\");\n  int x;\n};\n\
               enum class E { B_Y };\nenum class F : int { A_X };\nenum class G { A_X, B_Y };\n";
    let (store, _tmp) = index_and_resolve(&[("e.h", src)]);
    assert!(plain_names(&store, "Field").contains(&"K::x".to_string()));
    let constants = plain_names(&store, "Constant");
    for c in ["E::B_Y", "F::A_X", "G::A_X", "G::B_Y"] {
        assert!(constants.iter().any(|q| q == c), "{c} not in {constants:?}");
    }
}
