use super::*;

#[test]
fn a_method_belongs_to_the_class_of_its_qualified_name() {
    assert_eq!(
        owner_of("b.cpp::etl::bloom::width#1").as_deref(),
        Some("etl::bloom")
    );
    assert_eq!(owner_of("b.cpp::freefn#2").as_deref(), None);
}
