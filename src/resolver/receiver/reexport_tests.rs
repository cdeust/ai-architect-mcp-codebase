// Unit tests of `reexport` (issue #373).

use super::*;
use crate::resolver::receiver::imports::ModuleImports;
use std::collections::{BTreeMap, BTreeSet};

/// The library `fx` (src/) and the test target tests/it.rs.
fn two_crates() -> CrateEvidence {
    let mut owners = BTreeMap::new();
    for file in ["src/lib.rs", "src/task.rs"] {
        owners.insert(file.to_string(), BTreeSet::from(["src/lib.rs".to_string()]));
    }
    owners.insert(
        "tests/it.rs".to_string(),
        BTreeSet::from(["tests/it.rs".to_string()]),
    );
    CrateEvidence {
        known: true,
        crate_names: BTreeSet::from(["fx".to_string()]),
        targets: BTreeMap::from([
            ("src/lib.rs".to_string(), Some("fx".to_string())),
            ("tests/it.rs".to_string(), None),
        ]),
        owners,
    }
}

fn with_imports(ev: &CrateEvidence, rows: &[(&str, &str, bool)]) -> ModuleImports {
    let mut imports = ModuleImports::default();
    for (scope, path, is_glob) in rows {
        imports.insert(
            ev,
            scope,
            ImportRow {
                path: path.to_string(),
                alias: String::new(),
                is_glob: *is_glob,
            },
        );
    }
    imports
}

fn segs(path: &str) -> Vec<String> {
    path_segments(path)
}

fn followed(
    ev: &CrateEvidence,
    imports: &ModuleImports,
    caller: &str,
    start: Anchor,
) -> Vec<Anchor> {
    let idx = SymbolIndex {
        by_name: HashMap::new(),
        by_qn: HashMap::new(),
        by_parent_module: HashMap::new(),
    };
    let facts = PathFacts {
        idx: &idx,
        evidence: ev,
        imports,
    };
    let mut out = follow(&facts, caller, start);
    out.sort_by_key(|a| format!("{a:?}"));
    out
}

#[test]
fn a_root_re_export_of_a_module_leads_to_that_module() {
    let ev = two_crates();
    let imports = with_imports(&ev, &[("src/lib.rs", "task::Set", false)]);
    let out = followed(&ev, &imports, "src/task.rs", Anchor::CrateRoot(segs("Set")));
    assert_eq!(out, [Anchor::CrateRoot(segs("task::Set"))]);
}

#[test]
fn a_root_re_export_of_an_external_path_leads_where_no_owner_is() {
    let ev = two_crates();
    let imports = with_imports(&ev, &[("src/lib.rs", "ext::Set", false)]);
    let out = followed(&ev, &imports, "src/task.rs", Anchor::CrateRoot(segs("Set")));
    assert_eq!(out, [Anchor::CrateRoot(segs("ext::Set"))]);
}

#[test]
fn a_glob_adds_its_path_beside_the_definition() {
    let ev = two_crates();
    let imports = with_imports(&ev, &[("src/lib.rs", "task", true)]);
    let out = followed(&ev, &imports, "src/task.rs", Anchor::CrateRoot(segs("Set")));
    assert_eq!(
        out,
        [
            Anchor::CrateRoot(segs("Set")),
            Anchor::CrateRoot(segs("task::Set"))
        ]
    );
}

#[test]
fn a_use_of_itself_ends() {
    let ev = two_crates();
    let imports = with_imports(&ev, &[("src/lib.rs", "crate::Set", false)]);
    let out = followed(&ev, &imports, "src/task.rs", Anchor::CrateRoot(segs("Set")));
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn a_cycle_of_globs_and_re_exports_ends_without_a_bound() {
    // `a` re-exports `b::Set`, `b` re-exports `a::Set`, and each globs the
    // other: every path is seen once, then following stops.
    let ev = two_crates();
    let imports = with_imports(
        &ev,
        &[
            ("src/a.rs", "crate::b::Set", false),
            ("src/b.rs", "crate::a::Set", false),
            ("src/a.rs", "super::b", true),
            ("src/b.rs", "super::a", true),
        ],
    );
    let out = followed(&ev, &imports, "src/a.rs", Anchor::CrateRoot(segs("a::Set")));
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn a_library_path_reads_only_that_library_s_imports() {
    let ev = two_crates();
    let imports = with_imports(
        &ev,
        &[
            ("src/lib.rs", "crate::task::Set", false),
            ("tests/it.rs", "other::Set", false),
        ],
    );
    let start = Anchor::Library {
        lib: "fx".to_string(),
        segments: segs("Set"),
    };
    let out = followed(&ev, &imports, "tests/it.rs", start);
    assert_eq!(
        out,
        [Anchor::Library {
            lib: "fx".to_string(),
            segments: segs("task::Set")
        }]
    );
    // The caller's own crate reads the test root's imports, not the library's.
    let own = followed(&ev, &imports, "tests/it.rs", Anchor::CrateRoot(segs("Set")));
    assert_eq!(own, [Anchor::CrateRoot(segs("other::Set"))]);
}

#[test]
fn a_path_is_read_from_its_module() {
    let ev = two_crates();
    let module = segs("a::b");
    let at = |path: &str| anchor_in(&ev, None, &module, &segs(path));
    assert_eq!(at("c::Set"), Some(Anchor::CrateRoot(segs("a::b::c::Set"))));
    assert_eq!(at("self::Set"), Some(Anchor::CrateRoot(segs("a::b::Set"))));
    assert_eq!(at("super::Set"), Some(Anchor::CrateRoot(segs("a::Set"))));
    assert_eq!(at("crate::x::Set"), Some(Anchor::CrateRoot(segs("x::Set"))));
    assert_eq!(at("super::super::super::Set"), None);
    assert_eq!(
        at("fx::Set"),
        Some(Anchor::Library {
            lib: "fx".to_string(),
            segments: segs("Set")
        })
    );
    assert_eq!(
        anchor_in(&ev, Some("fx"), &module, &segs("crate::Set")),
        Some(Anchor::Library {
            lib: "fx".to_string(),
            segments: segs("Set")
        })
    );
}
