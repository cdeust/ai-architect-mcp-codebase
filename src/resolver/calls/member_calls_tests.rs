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

fn scope(path: &str) -> Vec<String> {
    path.split("::")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn a_declared_type_is_the_class_of_the_innermost_scope_that_has_one() {
    let classes = classes_of(&[
        "etl::a::timer_data",
        "etl::b::timer_data",
        "etl::timer_data",
        "timer_data",
    ]);
    let holds = |scope_path: &str, class: &str| {
        classes
            .family_in("timer_data", "", &scope(scope_path))
            .holds(class)
    };
    assert!(holds("etl::a", "etl::a::timer_data"));
    assert!(!holds("etl::a", "etl::b::timer_data"));
    assert!(!holds("etl::a", "etl::timer_data"));
    assert!(holds("etl::c", "etl::timer_data"));
    assert!(!holds("etl::c", "timer_data"));
    assert!(holds("", "timer_data") && !holds("", "etl::timer_data"));
}

#[test]
fn a_declared_type_no_enclosing_scope_declares_keeps_the_suffix_reading() {
    let classes = classes_of(&["shapes::Box", "other::Box"]);
    let family = classes.family_in("Box", "", &scope("a"));
    assert!(family.holds("shapes::Box") && family.holds("other::Box"));
}

#[test]
fn a_leading_colon_names_the_class_of_that_full_path_only() {
    assert!(names_class("a::Box", "::a::Box"));
    assert!(!names_class("x::a::Box", "::a::Box"));
    assert!(names_class("x::a::Box", "a::Box"));
}

#[test]
fn a_multi_segment_type_is_looked_up_from_the_innermost_scope_too() {
    let classes = classes_of(&["etl::inner::Box", "other::inner::Box"]);
    let family = classes.family_in("inner::Box", "", &scope("etl::x"));
    assert!(family.holds("etl::inner::Box") && !family.holds("other::inner::Box"));
}
