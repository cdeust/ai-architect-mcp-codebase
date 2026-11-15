use super::*;
use crate::resolver::calls::includes::Include;

#[test]
fn a_class_is_named_by_its_path_or_by_a_suffix_of_it() {
    assert!(names_class("etl::bloom", "bloom"));
    assert!(names_class("etl::bloom", "etl::bloom"));
    assert!(!names_class("etl::bloom", "loom"));
    assert!(!names_class("etl::bloom", "other::bloom"));
}

#[test]
fn a_base_is_read_without_access_specifier_generics_or_root() {
    assert_eq!(class_path(" public Base<T> "), "Base");
    assert_eq!(class_path("::ns::Base"), "ns::Base");
    assert_eq!(class_path("virtual ns::Base<A, B>"), "ns::Base");
    assert_eq!(class_path("public_base"), "public_base");
    assert_eq!(class_path("virtual_base<T>"), "virtual_base");
}

#[test]
fn a_family_holds_the_bases_at_any_depth_and_survives_a_cycle() {
    let mut classes = CppClasses::default();
    for (path, bases) in [("ns::a", "b"), ("ns::b", "c"), ("ns::c", "a")] {
        let last = path.rsplit("::").next().unwrap().to_string();
        classes
            .by_last
            .entry(last)
            .or_default()
            .push(path.to_string());
        classes
            .bases
            .insert(path.to_string(), vec![bases.to_string()]);
    }
    let family = classes.family("a", "");
    assert!(family.holds("ns::a") && family.holds("ns::b") && family.holds("ns::c"));
    assert!(!family.holds("ns::d"));
}

fn classes_of(paths: &[&str]) -> CppClasses {
    let mut classes = CppClasses::default();
    for path in paths {
        let last = path.rsplit("::").next().unwrap().to_string();
        classes
            .by_last
            .entry(last)
            .or_default()
            .push(path.to_string());
    }
    classes
}

fn with_base(classes: &mut CppClasses, class: &str, base: &str) {
    classes
        .bases
        .insert(class.to_string(), vec![base.to_string()]);
}

fn alias(classes: &mut CppClasses, path: &str, file: &str, target: &str) {
    let last = path.rsplit("::").next().unwrap().to_string();
    classes.aliases.entry(last).or_default().push(Alias {
        path: path.to_string(),
        file: file.to_string(),
        target: target.to_string(),
    });
}

#[test]
fn a_typedef_a_source_file_writes_is_seen_by_that_file_only() {
    let mut classes = classes_of(&["etl::iset", "etl::imap", "etl::set_", "etl::map_"]);
    with_base(&mut classes, "etl::set_", "iset");
    with_base(&mut classes, "etl::map_", "imap");
    alias(&mut classes, "D::D", "set.cpp", "etl::set_");
    alias(&mut classes, "D", "map.cpp", "etl::map_");
    let family = classes.family("D", "set.cpp");
    assert!(family.holds("etl::iset") && !family.holds("etl::imap"));
    let family = classes.family("D", "map.cpp");
    assert!(family.holds("etl::imap") && !family.holds("etl::iset"));
    let family = classes.family("D", "other.cpp");
    assert!(!family.holds("etl::iset") && !family.holds("etl::imap"));
}

#[test]
fn a_typedef_of_another_source_file_is_no_declaration_in_scope() {
    let mut classes = classes_of(&["ns::D", "x::E"]);
    alias(&mut classes, "D", "other.cpp", "x::E");
    let family = classes.family("D", "set.cpp");
    assert!(family.holds("ns::D") && !family.holds("x::E"));
}

#[test]
fn a_typedef_a_header_writes_is_seen_by_every_file() {
    let mut classes = classes_of(&["etl::iset"]);
    alias(&mut classes, "D", "types.h", "etl::iset");
    let family = classes.family("D", "any.cpp");
    assert!(family.holds("etl::iset"));
}

#[test]
fn a_typedef_of_the_callers_file_hides_a_header_typedef_of_the_same_name() {
    // The file's `D` is the one in scope: the header's `D` of the same path is no
    // alternative (`alias_targets` keeps the aliases of `from` when it writes any).
    let mut classes = classes_of(&["x::Local", "x::Header"]);
    alias(&mut classes, "D", "types.h", "x::Header");
    alias(&mut classes, "D", "set.cpp", "x::Local");
    let family = classes.family("D", "set.cpp");
    assert!(family.holds("x::Local") && !family.holds("x::Header"));
    let family = classes.family("D", "other.cpp");
    assert!(family.holds("x::Header") && !family.holds("x::Local"));
}

fn including(pairs: &[(&str, &str)]) -> IncludeGraph {
    let ids = ["x.cpp", "y.cpp", "z.cpp", "h.h"]
        .iter()
        .map(|f| (*f).to_string())
        .collect();
    let mut imports: HashMap<String, Vec<Include>> = HashMap::new();
    for (file, included) in pairs {
        imports
            .entry((*file).to_string())
            .or_default()
            .push(Include {
                path: (*included).to_string(),
                computed: false,
            });
    }
    IncludeGraph::build(&imports, &ids)
}

#[test]
fn a_source_file_sees_what_the_source_files_it_includes_declare() {
    let classes = CppClasses {
        includes: including(&[("y.cpp", "x.cpp"), ("z.cpp", "h.h"), ("h.h", "x.cpp")]),
        ..CppClasses::default()
    };
    assert!(classes.sees("y.cpp", "x.cpp"), "directly");
    assert!(classes.sees("z.cpp", "x.cpp"), "through a header");
    assert!(classes.sees("x.cpp", "x.cpp"), "itself");
    assert!(
        classes.sees("y.cpp", "any.h"),
        "a header is seen by every file"
    );
    assert!(
        classes.sees("x.cpp", "y.cpp"),
        "a file included by the declaring file: the alias is written, then it is included"
    );
    assert!(
        classes.sees("h.h", "z.cpp"),
        "a header the declaring file includes"
    );
    assert!(
        !classes.sees("y.cpp", "z.cpp"),
        "two files that include the same file are not one translation unit"
    );
}

#[test]
fn a_typedef_of_an_included_source_file_is_in_scope() {
    let mut classes = classes_of(&["lib::S", "other::S"]);
    alias(&mut classes, "T", "x.cpp", "lib::S");
    classes.includes = including(&[("y.cpp", "x.cpp")]);
    let family = classes.family("T", "y.cpp");
    assert!(family.holds("lib::S") && !family.holds("other::S"));
    let family = classes.family("T", "z.cpp");
    assert!(!family.holds("lib::S"), "z.cpp does not include x.cpp");
}

/// A caller in `file`, written in `scope`, whose line is unknown.
fn at<'a>(file: &'a str, scope: &'a str) -> Caller<'a> {
    Caller {
        file,
        scope,
        line: u64::MAX,
    }
}

fn using_declaration(classes: &mut CppClasses, scope: &str, file: &str, name: &str) {
    classes.usings.push(Using {
        scope: scope.to_string(),
        file: file.to_string(),
        name: name.to_string(),
        namespace: String::new(),
    });
}

fn using_directive(classes: &mut CppClasses, scope: &str, file: &str, namespace: &str) {
    classes.usings.push(Using {
        scope: scope.to_string(),
        file: file.to_string(),
        name: String::new(),
        namespace: namespace.to_string(),
    });
}

#[test]
fn a_name_is_looked_up_in_the_scopes_of_the_caller_innermost_first() {
    assert_eq!(enclosing_scopes("a::b::C"), ["a::b::C", "a::b", "a", ""]);
    assert_eq!(enclosing_scopes(""), [""]);
}

#[test]
fn a_declared_type_names_the_class_of_the_innermost_scope_that_holds_one() {
    // `namespace a { int f(Box& b) { return b.m(); } }` with `a::Box` and two
    // namesakes: C++ names `a::Box` (§6.4.1), and the path suffix named all three.
    let classes = classes_of(&["Box", "a::Box", "other::Box"]);
    let family = classes.declared_family("Box", &at("x.cpp", "a"));
    assert!(family.holds("a::Box"));
    assert!(!family.holds("Box") && !family.holds("other::Box"));
}

#[test]
fn a_declared_type_of_a_method_is_looked_up_in_its_class_then_its_namespaces() {
    let classes = classes_of(&["a::C::Box", "a::Box"]);
    assert!(classes
        .declared_family("Box", &at("x.cpp", "a::C"))
        .holds("a::C::Box"));
    assert!(classes
        .declared_family("Box", &at("x.cpp", "a"))
        .holds("a::Box"));
}

#[test]
fn a_declared_type_no_scope_of_the_caller_holds_keeps_the_path_suffix_reading() {
    let classes = classes_of(&["shapes::Box"]);
    let family = classes.declared_family("Box", &at("x.cpp", "a"));
    assert!(
        family.holds("shapes::Box"),
        "the reading of main: one class of that name binds"
    );
    let classes = classes_of(&["shapes::Box", "other::Box"]);
    let family = classes.declared_family("Box", &at("x.cpp", "a"));
    assert!(
        family.holds("shapes::Box") && family.holds("other::Box"),
        "and several keep the site open"
    );
}

#[test]
fn a_class_of_a_scope_of_the_caller_hides_a_namesake_deeper_in_another_namespace() {
    let classes = classes_of(&["a::Box", "x::a::Box"]);
    let family = classes.declared_family("Box", &at("x.cpp", "a"));
    assert!(family.holds("a::Box") && !family.holds("x::a::Box"));
}

#[test]
fn the_bases_of_the_class_found_by_scope_are_of_its_family() {
    let mut classes = classes_of(&["a::Box", "a::Base", "other::Box"]);
    with_base(&mut classes, "a::Box", "Base");
    let family = classes.declared_family("Box", &at("x.cpp", "a"));
    assert!(family.holds("a::Base") && !family.holds("other::Box"));
}

#[test]
fn a_name_an_alias_introduces_keeps_the_path_suffix_reading() {
    // Which translation unit reaches which alias is `alias_targets`' reading
    // (#438), which the lookup by scope does not model: the alias and the class
    // of that name are both possible, as on `main`.
    let mut classes = classes_of(&["other::Impl", "far::Handle"]);
    alias(&mut classes, "a::Handle", "x.cpp", "other::Impl");
    let family = classes.declared_family("Handle", &at("x.cpp", "a"));
    assert!(family.holds("other::Impl") && family.holds("far::Handle"));
}

#[test]
fn a_member_type_a_base_of_the_callers_class_declares_is_the_class_found() {
    // `T` in a method of `D : Base` is `Base::T`, the member type it inherits,
    // not the global `T`.
    let mut classes = classes_of(&["Base::T", "T", "D", "Base"]);
    with_base(&mut classes, "D", "Base");
    let family = classes.declared_family("T", &at("x.cpp", "D"));
    assert!(family.holds("Base::T") && !family.holds("T"));
}

#[test]
fn a_member_type_two_bases_declare_keeps_the_path_suffix_reading() {
    let mut classes = classes_of(&["L::T", "R::T", "D"]);
    classes
        .bases
        .insert("D".to_string(), vec!["L".to_string(), "R".to_string()]);
    let family = classes.declared_family("T", &at("x.cpp", "D"));
    assert!(family.holds("L::T") && family.holds("R::T"));
}

#[test]
fn a_declared_type_written_with_a_qualifier_keeps_the_path_suffix_reading() {
    // A namespace alias is not tracked, so a written qualifier is read as before.
    let classes = classes_of(&["x::other::Box", "a::other::Box"]);
    let family = classes.declared_family("other::Box", &at("x.cpp", "a"));
    assert!(family.holds("x::other::Box") && family.holds("a::other::Box"));
}

#[test]
fn a_using_declaration_of_a_scope_walked_keeps_the_path_suffix_reading() {
    // `namespace a { struct Box; namespace b { using other::Box; … } }`: C++ names
    // `other::Box` in `a::b`, and the graph holds no edge from the using to it, so
    // the two classes keep the site open instead of binding `a::Box`.
    let mut classes = classes_of(&["a::Box", "other::Box"]);
    using_declaration(&mut classes, "a::b", "x.cpp", "Box");
    let family = classes.declared_family("Box", &at("x.cpp", "a::b"));
    assert!(family.holds("a::Box") && family.holds("other::Box"));
}

#[test]
fn a_using_directive_on_a_namespace_that_holds_a_namesake_keeps_the_suffix_reading() {
    let mut classes = classes_of(&["a::Box", "other::Box"]);
    using_directive(&mut classes, "a::b", "x.cpp", "other");
    let family = classes.declared_family("Box", &at("x.cpp", "a::b"));
    assert!(family.holds("a::Box") && family.holds("other::Box"));
}

#[test]
fn a_using_directive_on_a_namespace_without_the_name_does_not_stop_the_lookup() {
    let mut classes = classes_of(&["a::Box", "other::Box"]);
    using_directive(&mut classes, "a::b", "x.cpp", "empty");
    let family = classes.declared_family("Box", &at("x.cpp", "a::b"));
    assert!(family.holds("a::Box") && !family.holds("other::Box"));
}

#[test]
fn a_using_of_a_scope_the_lookup_never_walked_does_not_stop_it() {
    let mut classes = classes_of(&["a::Box", "other::Box"]);
    using_declaration(&mut classes, "z", "x.cpp", "Box");
    let family = classes.declared_family("Box", &at("x.cpp", "a"));
    assert!(family.holds("a::Box") && !family.holds("other::Box"));
}

#[test]
fn a_using_the_callers_file_does_not_see_does_not_stop_the_lookup() {
    let mut classes = classes_of(&["a::Box", "other::Box"]);
    using_declaration(&mut classes, "a", "y.cpp", "Box");
    let family = classes.declared_family("Box", &at("x.cpp", "a"));
    assert!(
        family.holds("a::Box") && !family.holds("other::Box"),
        "a using of another source file is no declaration in scope"
    );
}

#[test]
fn a_using_is_read_from_the_id_of_its_import_node() {
    let declaration = using_of("x.cpp::a::b::using:other::Box").expect("a declaration");
    assert_eq!(
        (
            declaration.scope.as_str(),
            declaration.file.as_str(),
            declaration.name.as_str(),
            declaration.namespace.as_str()
        ),
        ("a::b", "x.cpp", "Box", "")
    );
    let directive = using_of("x.cpp::using:namespace other::deep").expect("a directive");
    assert_eq!(
        (
            directive.scope.as_str(),
            directive.name.as_str(),
            directive.namespace.as_str()
        ),
        ("", "", "other::deep"),
        "a using at file scope is written in the global scope"
    );
    assert!(
        using_of("x.cpp::include:stdio.h").is_none(),
        "an include is no using"
    );
}

#[test]
fn a_directive_on_a_namespace_written_relative_to_its_scope_keeps_the_suffix_reading() {
    // `namespace a::b { namespace inner { struct Box; } using namespace inner; }`:
    // the directive writes `inner`, the class is `a::b::inner::Box`.
    let mut classes = classes_of(&["a::Box", "a::b::inner::Box"]);
    using_directive(&mut classes, "a::b", "x.cpp", "inner");
    let family = classes.declared_family("Box", &at("x.cpp", "a::b"));
    assert!(family.holds("a::Box") && family.holds("a::b::inner::Box"));
}

#[test]
fn a_base_that_names_several_classes_keeps_the_path_suffix_reading() {
    // `a::D : Base` with `a::Base` and `unrelated::Base`: which one C++ names is
    // not read here, so the member type of `unrelated::Base` is not chosen.
    let mut classes = classes_of(&[
        "T",
        "unrelated::Base::T",
        "a::Base",
        "unrelated::Base",
        "a::D",
    ]);
    with_base(&mut classes, "a::D", "Base");
    let family = classes.declared_family("T", &at("x.cpp", "a::D"));
    assert!(family.holds("T") && family.holds("unrelated::Base::T"));
}

#[test]
fn an_alias_of_a_scope_walked_keeps_the_path_suffix_reading() {
    // `namespace a { using Box = other::Impl; }` with a global `Box`: C++ names the
    // alias, so the global class is not chosen.
    let mut classes = classes_of(&["Box", "other::Impl"]);
    alias(&mut classes, "a::Box", "x.cpp", "other::Impl");
    let family = classes.declared_family("Box", &at("x.cpp", "a"));
    assert!(family.holds("Box") && family.holds("other::Impl"));
}

#[test]
fn a_member_class_declared_after_the_call_is_not_the_class_found() {
    // `struct C { int f(Box& b) { return b.m(); } struct Box {}; };`: the parameter's
    // `Box` is the global one; `C::Box` is declared after it.
    let mut classes = classes_of(&["Box", "C", "C::Box"]);
    classes
        .declared_at
        .insert("C::Box".to_string(), vec![("x.cpp".to_string(), 9)]);
    let caller = Caller {
        file: "x.cpp",
        scope: "C",
        line: 4,
    };
    let family = classes.declared_family("Box", &caller);
    assert!(family.holds("Box") && family.holds("C::Box"));
    let later = Caller { line: 12, ..caller };
    assert!(classes.declared_family("Box", &later).holds("C::Box"));
    assert!(!classes.declared_family("Box", &later).holds("Box"));
}

#[test]
fn a_using_of_a_scope_whose_name_ends_in_using_is_read_whole() {
    let declaration = using_of("x.cpp::housing::using:other::Box").expect("a declaration");
    assert_eq!(
        (declaration.scope.as_str(), declaration.name.as_str()),
        ("housing", "Box")
    );
}

#[test]
fn many_namesakes_that_name_their_own_name_as_a_base_are_expanded_once() {
    // Review 1 of #542: `namespace nI { struct A : A {}; }` for many I made
    // `class_and_bases` cubic in the number of classes named `A`. Each seed is
    // now expanded once, so this finishes at once instead of hanging.
    let paths: Vec<String> = (0..2000).map(|i| format!("n{i}::A")).collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let mut classes = classes_of(&refs);
    for path in &paths {
        with_base(&mut classes, path, "A");
    }
    for path in paths.iter().take(200) {
        let scope = path.as_str();
        let _ = classes.declared_family("m", &at("x.cpp", scope));
        assert!(
            classes.class_and_bases(scope).is_none(),
            "the base `A` names 2000 classes"
        );
    }
}
