use super::*;

#[test]
fn a_method_belongs_to_the_class_of_its_qualified_name() {
    assert_eq!(
        owner_of("b.cpp::etl::bloom::width#1").as_deref(),
        Some("etl::bloom")
    );
    assert_eq!(owner_of("b.cpp::freefn#2").as_deref(), None);
}

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
    let mut imports: HashMap<String, Vec<String>> = HashMap::new();
    for (file, included) in pairs {
        imports
            .entry((*file).to_string())
            .or_default()
            .push((*included).to_string());
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
        !classes.sees("x.cpp", "y.cpp"),
        "an include is not symmetric"
    );
    assert!(
        !classes.sees("h.h", "z.cpp"),
        "nor is a file seen by a header it includes"
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
