use super::*;
use crate::indexer::cargo_targets::CrateRoot;
use std::fs;

fn tree(files: &[(&str, &str)]) -> (tempfile::TempDir, BTreeSet<PathBuf>) {
    let dir = tempfile::tempdir().expect("temp dir");
    for (rel, body) in files {
        let path = dir.path().join(rel);
        fs::create_dir_all(path.parent().expect("has parent")).expect("mkdir");
        fs::write(path, body).expect("write");
    }
    let indexed = files.iter().map(|(rel, _)| PathBuf::from(rel)).collect();
    (dir, indexed)
}

fn known(roots: &[(&str, TargetKind)]) -> TargetMap {
    TargetMap::Known {
        target_dirs: BTreeSet::new(),
        target_files: roots.iter().map(|(r, _)| PathBuf::from(r)).collect(),
        crate_roots: roots
            .iter()
            .map(|(entry, kind)| CrateRoot {
                entry: PathBuf::from(entry),
                default_features: BTreeSet::new(),
                kind: *kind,
                lib_name: None,
            })
            .collect(),
        crate_names: BTreeSet::new(),
    }
}

fn contexts(
    files: &[(&str, &str)],
    roots: &[(&str, TargetKind)],
) -> BTreeMap<String, &'static str> {
    let (dir, indexed) = tree(files);
    analyse(dir.path(), &known(roots), &indexed)
        .into_iter()
        .map(|(file, context)| (file.to_string_lossy().into_owned(), context))
        .collect()
}

use TargetKind::{Bench, Example, Production, Test};

#[test]
fn each_target_kind_gives_its_entry_file_its_class() {
    let map = contexts(
        &[
            ("src/lib.rs", ""),
            ("src/main.rs", ""),
            ("tests/common.rs", ""),
            ("benches/b.rs", ""),
            ("examples/e.rs", ""),
            ("build.rs", ""),
        ],
        &[
            ("src/lib.rs", Production),
            ("src/main.rs", Production),
            ("tests/common.rs", Test),
            ("benches/b.rs", Bench),
            ("examples/e.rs", Example),
            ("build.rs", Production),
        ],
    );
    assert_eq!(map["src/lib.rs"], "production");
    assert_eq!(map["src/main.rs"], "production");
    assert_eq!(map["tests/common.rs"], "test");
    assert_eq!(map["benches/b.rs"], "bench");
    assert_eq!(map["examples/e.rs"], "example");
    assert_eq!(map["build.rs"], "production");
}

#[test]
fn a_module_of_a_test_target_is_test_code() {
    let map = contexts(
        &[
            ("tests/all.rs", "mod support;\n"),
            ("tests/support/mod.rs", "mod deep;\n"),
            ("tests/support/deep.rs", ""),
        ],
        &[("tests/all.rs", Test)],
    );
    assert_eq!(map["tests/support/mod.rs"], "test");
    assert_eq!(map["tests/support/deep.rs"], "test");
}

#[test]
fn a_file_declared_only_under_cfg_test_is_test_code_below_a_lib() {
    let map = contexts(
        &[
            ("src/lib.rs", "#[cfg(test)]\nmod tests;\nmod util;\n"),
            ("src/tests.rs", "mod cases;\n"),
            ("src/tests/cases.rs", ""),
            ("src/util.rs", ""),
        ],
        &[("src/lib.rs", Production)],
    );
    assert_eq!(map["src/tests.rs"], "test");
    assert_eq!(map["src/tests/cases.rs"], "test");
    assert_eq!(map["src/util.rs"], "production");
}

#[test]
fn a_file_named_tests_declared_as_a_plain_mod_is_production() {
    let map = contexts(
        &[("src/lib.rs", "mod tests;\n"), ("src/tests.rs", "")],
        &[("src/lib.rs", Production)],
    );
    assert_eq!(map["src/tests.rs"], "production");
}

#[test]
fn any_production_path_wins_over_a_test_path() {
    let map = contexts(
        &[
            ("src/lib.rs", "mod shared;\n"),
            (
                "tests/t.rs",
                "#[path = \"../src/shared.rs\"]\nmod shared;\n",
            ),
            ("src/shared.rs", ""),
        ],
        &[("src/lib.rs", Production), ("tests/t.rs", Test)],
    );
    assert_eq!(map["src/shared.rs"], "production");
}

#[test]
fn a_file_two_non_production_kinds_share_is_not_decided() {
    let map = contexts(
        &[
            ("tests/t.rs", "#[path = \"../shared.rs\"]\nmod shared;\n"),
            ("benches/b.rs", "#[path = \"../shared.rs\"]\nmod shared;\n"),
            ("shared.rs", ""),
        ],
        &[("tests/t.rs", Test), ("benches/b.rs", Bench)],
    );
    assert!(!map.contains_key("shared.rs"));
}

#[test]
fn a_file_no_declaration_reaches_and_a_feature_gate_alone_decide_nothing_wrong() {
    let map = contexts(
        &[
            ("src/lib.rs", "#[cfg(feature = \"x\")]\nmod extra;\n"),
            ("src/extra.rs", ""),
            ("kani/proofs.rs", ""),
        ],
        &[("src/lib.rs", Production)],
    );
    assert_eq!(map["src/extra.rs"], "production");
    assert!(!map.contains_key("kani/proofs.rs"));
}

#[test]
fn cfg_any_test_and_cfg_not_test_do_not_make_a_module_test_code() {
    let map = contexts(
        &[
            (
                "src/lib.rs",
                "#[cfg(any(test, feature = \"x\"))]\nmod a;\n#[cfg(not(test))]\nmod b;\n",
            ),
            ("src/a.rs", ""),
            ("src/b.rs", ""),
        ],
        &[("src/lib.rs", Production)],
    );
    assert_eq!(map["src/a.rs"], "production");
    assert_eq!(map["src/b.rs"], "production");
}

#[test]
fn an_unknown_map_decides_nothing() {
    let (dir, indexed) = tree(&[("src/lib.rs", "")]);
    let unknown = TargetMap::Unknown {
        detail: "no cargo".into(),
    };
    assert!(analyse(dir.path(), &unknown, &indexed).is_empty());
}

#[test]
fn a_target_kind_comes_from_the_cargo_kind_list_and_defaults_to_production() {
    use crate::indexer::cargo_targets::parse_metadata_json;
    let root = "/tmp/proj";
    let json = format!(
        r#"{{"packages":[{{"targets":[
            {{"src_path":"{root}/src/lib.rs","kind":["lib"]}},
            {{"src_path":"{root}/tests/a.rs","kind":["test"]}},
            {{"src_path":"{root}/benches/b.rs","kind":["bench"]}},
            {{"src_path":"{root}/examples/e.rs","kind":["example"]}},
            {{"src_path":"{root}/build.rs","kind":["custom-build"]}},
            {{"src_path":"{root}/src/other.rs"}}
        ]}}]}}"#
    );
    let TargetMap::Known { crate_roots, .. } = parse_metadata_json(&json, Path::new(root)) else {
        panic!("known");
    };
    let kinds: Vec<TargetKind> = crate_roots.iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        [Production, Test, Bench, Example, Production, Production]
    );
}
