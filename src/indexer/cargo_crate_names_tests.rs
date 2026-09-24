// cargo_crate_names_tests: the library crate names `parse_metadata_json` reads
// (issues #348 and #349): a hyphenated package, a `[lib] name` override, every
// library of a workspace, and no binary or test target.

use super::*;

const FIXTURE_ROOT: &str = "/tmp/proj";

fn fixture_json() -> String {
    format!(
        r#"{{"packages":[{{"targets":[
            {{"src_path":"{FIXTURE_ROOT}/src/lib.rs"}},
            {{"src_path":"{FIXTURE_ROOT}/src/main.rs"}}
        ]}}]}}"#
    )
}

fn crate_names_of(json: &str) -> BTreeSet<String> {
    let TargetMap::Known { crate_names, .. } = parse_metadata_json(json, Path::new(FIXTURE_ROOT))
    else {
        panic!("expected a known map");
    };
    crate_names
}

#[test]
fn a_hyphenated_package_is_imported_with_underscores() {
    let json = format!(
        r#"{{"packages":[{{"name":"my-crate","targets":[
            {{"name":"my_crate","kind":["lib"],"src_path":"{FIXTURE_ROOT}/src/lib.rs"}},
            {{"name":"my-crate","kind":["bin"],"src_path":"{FIXTURE_ROOT}/src/main.rs"}}
        ]}}]}}"#
    );
    assert_eq!(
        crate_names_of(&json),
        BTreeSet::from(["my_crate".to_string()])
    );
}

#[test]
fn a_lib_name_override_is_the_crate_name_and_the_package_name_is_not() {
    let json = format!(
        r#"{{"packages":[{{"name":"my-crate","targets":[
            {{"name":"alt","kind":["lib"],"src_path":"{FIXTURE_ROOT}/src/lib.rs"}}
        ]}}]}}"#
    );
    assert_eq!(crate_names_of(&json), BTreeSet::from(["alt".to_string()]));
}

#[test]
fn every_library_of_a_workspace_counts_and_no_binary_or_test_does() {
    let json = format!(
        r#"{{"packages":[
            {{"name":"a","targets":[
                {{"name":"a","kind":["lib"],"src_path":"{FIXTURE_ROOT}/a/src/lib.rs"}},
                {{"name":"it","kind":["test"],"src_path":"{FIXTURE_ROOT}/a/tests/it.rs"}}]}},
            {{"name":"b","targets":[
                {{"name":"b","kind":["proc-macro"],"src_path":"{FIXTURE_ROOT}/b/src/lib.rs"}},
                {{"name":"tool","kind":["bin"],"src_path":"{FIXTURE_ROOT}/b/src/main.rs"}}]}}
        ]}}"#
    );
    assert_eq!(
        crate_names_of(&json),
        BTreeSet::from(["a".to_string(), "b".to_string()])
    );
}

#[test]
fn a_fixture_without_target_names_knows_no_crate() {
    assert!(crate_names_of(&fixture_json()).is_empty());
}
