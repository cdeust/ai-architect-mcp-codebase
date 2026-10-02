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
fn the_commas_of_generic_arguments_do_not_separate_bases() {
    assert_eq!(
        split_bases("etl::iterator<tag, const T>, public Other<A<B, C>, D>,Last"),
        [
            "etl::iterator<tag, const T>",
            " public Other<A<B, C>, D>",
            "Last"
        ]
    );
    assert_eq!(split_bases(""), [""]);
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

fn scope(path: &str) -> Vec<String> {
    path.split("::")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// A caller in `file` of a function of the namespaces `path`.
fn in_function<'a>(file: &'a str, path: &str) -> Caller<'a> {
    Caller {
        file,
        scope: scope(path),
        in_class: false,
    }
}

/// A caller in `file` that is a method of the class `path`.
fn in_method<'a>(file: &'a str, path: &str) -> Caller<'a> {
    Caller {
        file,
        scope: scope(path),
        in_class: true,
    }
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
fn a_declared_type_is_the_class_of_the_innermost_scope_that_has_one() {
    let classes = classes_of(&[
        "etl::a::timer_data",
        "etl::b::timer_data",
        "etl::timer_data",
        "timer_data",
    ]);
    let holds = |caller: Caller, class: &str| classes.family_in("timer_data", &caller).holds(class);
    assert!(holds(in_method("", "etl::a"), "etl::a::timer_data"));
    assert!(!holds(in_method("", "etl::a"), "etl::b::timer_data"));
    assert!(!holds(in_method("", "etl::a"), "etl::timer_data"));
    assert!(holds(in_function("", "etl"), "etl::timer_data"));
    assert!(!holds(in_function("", "etl"), "timer_data"));
    assert!(
        holds(in_function("", ""), "timer_data") && !holds(in_function("", ""), "etl::timer_data")
    );
}

#[test]
fn a_declared_type_no_enclosing_scope_declares_keeps_the_suffix_reading() {
    let classes = classes_of(&["shapes::Box", "other::Box"]);
    let family = classes.family_in("Box", &in_function("", "a"));
    assert!(family.holds("shapes::Box") && family.holds("other::Box"));
}

#[test]
fn a_scope_between_the_declaring_one_and_the_caller_may_supply_the_name() {
    // A function of a namespace `a::n` inside the namespace `a` that declares `Box`
    // stays on the suffix reading (a using-declaration of `a::n` may bring another
    // `Box` in); a method of a class `a::D` whose bases the graph holds, and which
    // declare no `Box`, binds the `a::Box`.
    let mut classes = classes_of(&["a::Box", "other::Box", "a::D", "a::E", "a::Base"]);
    with_base(&mut classes, "a::E", "Base");
    for (caller, bound) in [
        (in_function("", "a::n"), false),
        (in_method("", "a::D"), true),
        (in_method("", "a::E"), true),
    ] {
        let family = classes.family_in("Box", &caller);
        assert!(family.holds("a::Box"));
        assert_eq!(!family.holds("other::Box"), bound, "{:?}", caller.scope);
    }
}

#[test]
fn a_member_type_a_base_declares_is_not_the_outer_class() {
    let mut classes = classes_of(&["Box", "Base::Box", "D", "Base"]);
    with_base(&mut classes, "D", "Base");
    let family = classes.family_in("Box", &in_method("", "D"));
    assert!(
        family.holds("Base::Box") && family.holds("Box"),
        "suffix reading: stays open"
    );
}

#[test]
fn a_base_outside_the_graph_may_declare_the_name() {
    let mut classes = classes_of(&["a::Box", "other::Box", "a::D"]);
    with_base(&mut classes, "a::D", "std::vector");
    let family = classes.family_in("Box", &in_method("", "a::D"));
    assert!(family.holds("other::Box"));
}

#[test]
fn a_typedef_a_source_file_writes_is_seen_by_that_file_only() {
    let mut classes = classes_of(&["etl::iset", "etl::imap", "etl::set_", "etl::map_"]);
    with_base(&mut classes, "etl::set_", "iset");
    with_base(&mut classes, "etl::map_", "imap");
    alias(&mut classes, "D::D", "set.cpp", "etl::set_");
    alias(&mut classes, "D", "map.cpp", "etl::map_");
    let family = classes.family_in("D", &in_function("set.cpp", ""));
    assert!(family.holds("etl::iset") && !family.holds("etl::imap"));
    let family = classes.family_in("D", &in_function("map.cpp", ""));
    assert!(family.holds("etl::imap") && !family.holds("etl::iset"));
    let family = classes.family_in("D", &in_function("other.cpp", ""));
    assert!(!family.holds("etl::iset") && !family.holds("etl::imap"));
}

#[test]
fn a_typedef_of_the_callers_file_at_the_scope_path_hides_a_namesake_class() {
    // ETLCPP `test_queue_memory_model_small.cpp`: `pop::QueueInt` is the typedef of
    // the file; a class of another file named `QueueInt` is no candidate.
    let mut classes = classes_of(&["etl::queue", "x::QueueInt"]);
    alias(&mut classes, "pop::QueueInt", "q.cpp", "etl::queue");
    let family = classes.family_in("QueueInt", &in_function("q.cpp", "pop"));
    assert!(family.holds("etl::queue") && !family.holds("x::QueueInt"));
}

#[test]
fn a_typedef_of_another_source_file_is_no_declaration_in_scope() {
    let mut classes = classes_of(&["ns::D", "x::E"]);
    alias(&mut classes, "D", "other.cpp", "x::E");
    let family = classes.family_in("D", &in_function("set.cpp", ""));
    assert!(family.holds("ns::D") && !family.holds("x::E"));
}

#[test]
fn a_typedef_a_header_writes_is_seen_by_every_file() {
    let mut classes = classes_of(&["etl::iset"]);
    alias(&mut classes, "D", "types.h", "etl::iset");
    let family = classes.family_in("D", &in_function("any.cpp", ""));
    assert!(family.holds("etl::iset"));
}

#[test]
fn a_class_a_source_file_declares_is_not_declared_in_another_file() {
    let mut classes = classes_of(&["a::Box", "Box"]);
    classes
        .class_files
        .insert("a::Box".into(), vec!["x.cpp".into()]);
    classes
        .class_files
        .insert("Box".into(), vec!["box.h".into()]);
    let in_x = classes.family_in("Box", &in_function("x.cpp", "a"));
    assert!(in_x.holds("a::Box") && !in_x.holds("Box"));
    let in_y = classes.family_in("Box", &in_function("y.cpp", "a"));
    assert!(
        in_y.holds("a::Box") && in_y.holds("Box"),
        "no declaring scope sees it: suffix reading"
    );
}

#[test]
fn a_leading_colon_names_the_class_of_that_full_path_only() {
    assert!(names_class("a::Box", "::a::Box"));
    assert!(!names_class("x::a::Box", "::a::Box"));
    assert!(names_class("x::a::Box", "a::Box"));
}

#[test]
fn a_multi_segment_type_is_looked_up_from_the_innermost_scope_too() {
    let classes = classes_of(&["etl::inner::Box", "other::inner::Box", "etl::x"]);
    let family = classes.family_in("inner::Box", &in_method("", "etl::x"));
    assert!(family.holds("etl::inner::Box") && !family.holds("other::inner::Box"));
}
