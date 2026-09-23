use super::*;
use std::fs;

/// Writes `files` under a temp root and returns it with the indexed set.
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

fn known(roots: &[(&str, &[&str])]) -> TargetMap {
    TargetMap::Known {
        target_dirs: BTreeSet::new(),
        target_files: roots.iter().map(|(r, _)| PathBuf::from(r)).collect(),
        crate_roots: roots
            .iter()
            .map(|(entry, features)| CrateRoot {
                entry: PathBuf::from(entry),
                default_features: features.iter().map(|f| f.to_string()).collect(),
            })
            .collect(),
    }
}

fn gated_paths(result: &BTreeMap<PathBuf, String>) -> Vec<&str> {
    result.keys().map(|p| p.to_str().expect("utf-8")).collect()
}

#[test]
fn a_disabled_feature_gates_its_module_and_every_module_under_it() {
    let (dir, indexed) = tree(&[
        (
            "src/lib.rs",
            "#[cfg(feature = \"extra\")]\npub mod extra;\n#[cfg(test)]\nmod tests;\n",
        ),
        ("src/extra.rs", "mod inner;\n"),
        ("src/extra/inner.rs", "pub fn f() {}\n"),
        ("src/tests.rs", "\n"),
    ]);
    let result = find_feature_gated(dir.path(), &known(&[("src/lib.rs", &[])]), &indexed);
    // `Path` orders by component: `extra` < `extra.rs`.
    assert_eq!(gated_paths(&result), ["src/extra/inner.rs", "src/extra.rs"]);
    assert!(result[Path::new("src/extra.rs")]
        .contains("#[cfg(feature = \"extra\")] on `mod extra;` in src/lib.rs"));
    assert!(result[Path::new("src/extra/inner.rs")]
        .starts_with("declared in src/extra.rs, itself compiled out"));
}

#[test]
fn a_module_another_crate_root_compiles_is_not_gated() {
    let (dir, indexed) = tree(&[
        ("src/lib.rs", "mod util;\n"),
        ("src/main.rs", "#[cfg(feature = \"cli\")]\nmod util;\n"),
        ("src/util.rs", "\n"),
    ]);
    let map = known(&[("src/lib.rs", &[]), ("src/main.rs", &[])]);
    assert!(find_feature_gated(dir.path(), &map, &indexed).is_empty());
}

#[test]
fn not_of_a_default_feature_gates_and_the_default_feature_itself_does_not() {
    let (dir, indexed) = tree(&[
        (
            "src/lib.rs",
            "#[cfg(not(feature = \"std\"))]\nmod no_std;\n#[cfg(feature = \"std\")]\nmod with_std;\n",
        ),
        ("src/no_std.rs", "\n"),
        ("src/with_std.rs", "\n"),
    ]);
    let map = known(&[("src/lib.rs", &["default", "std"])]);
    let result = find_feature_gated(dir.path(), &map, &indexed);
    assert_eq!(gated_paths(&result), ["src/no_std.rs"]);
}

#[test]
fn nested_non_root_files_resolve_under_their_stem_directory_and_path_attributes() {
    let (dir, indexed) = tree(&[
        ("src/lib.rs", "mod a;\n"),
        ("src/a.rs", "#[cfg(feature = \"x\")]\nmod b;\n#[cfg(feature = \"x\")]\n#[path = \"elsewhere/c.rs\"]\nmod c;\n"),
        ("src/a/b/mod.rs", "\n"),
        ("src/elsewhere/c.rs", "\n"),
    ]);
    let result = find_feature_gated(dir.path(), &known(&[("src/lib.rs", &[])]), &indexed);
    assert_eq!(
        gated_paths(&result),
        ["src/a/b/mod.rs", "src/elsewhere/c.rs"]
    );
}

#[test]
fn an_unknown_target_map_classifies_nothing() {
    let (dir, indexed) = tree(&[
        ("src/lib.rs", "#[cfg(feature = \"extra\")]\nmod extra;\n"),
        ("src/extra.rs", "\n"),
    ]);
    assert!(find_feature_gated(
        dir.path(),
        &TargetMap::Unknown {
            detail: "test fixture".into()
        },
        &indexed
    )
    .is_empty());
}
