// Unit tests of `written_path` (issue #368).

use super::*;
use std::collections::{BTreeMap, BTreeSet};

fn entry(label: &str, qn: &str) -> SymbolEntry {
    SymbolEntry {
        id: qn.to_string(),
        label: label.to_string(),
        qualified_name: qn.to_string(),
    }
}

fn index(modules: &[&str]) -> SymbolIndex {
    SymbolIndex {
        by_name: HashMap::new(),
        by_qn: modules
            .iter()
            .map(|qn| (qn.to_string(), entry("Module", qn)))
            .collect(),
        by_parent_module: HashMap::new(),
    }
}

fn unknown() -> CrateEvidence {
    CrateEvidence {
        known: false,
        crate_names: BTreeSet::new(),
        targets: BTreeMap::new(),
        owners: BTreeMap::new(),
    }
}

/// Two crates: the library `fx` (src/) and the test target tests/it.rs.
fn two_crates() -> CrateEvidence {
    let own = |files: &[&str], entry: &str| {
        files
            .iter()
            .map(|f| (f.to_string(), BTreeSet::from([entry.to_string()])))
            .collect::<Vec<_>>()
    };
    let mut owners = BTreeMap::new();
    owners.extend(own(
        &["src/lib.rs", "src/b.rs", "src/c/mod.rs"],
        "src/lib.rs",
    ));
    owners.extend(own(&["tests/it.rs", "tests/b.rs"], "tests/it.rs"));
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

/// `WrittenPath::of` with no `use` declaration anywhere.
fn of<'e>(
    idx: &'e SymbolIndex,
    evidence: &'e CrateEvidence,
    caller: &str,
    hint: &str,
) -> Option<WrittenPath<'e>> {
    let imports: &'static ModuleImports = Box::leak(Box::default());
    WrittenPath::of(
        &PathFacts {
            idx,
            evidence,
            imports,
        },
        caller,
        hint,
    )
}

/// A caller and the path its binding writes.
struct Site<'a> {
    caller: &'a str,
    hint: &'a str,
}

fn admits(ev: &CrateEvidence, idx: &SymbolIndex, site: Site, method: &str) -> bool {
    let Site { caller, hint } = site;
    of(idx, ev, caller, hint)
        .unwrap_or_else(|| panic!("{hint} from {caller} declined"))
        .admits(&entry("Method", method))
}

#[test]
fn a_relative_path_admits_only_the_owner_it_names() {
    let (ev, idx) = (unknown(), index(&[]));
    let caller = "src/lib.rs::run";
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "b::Set"
        },
        "src/b.rs::Set::m"
    ));
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "b::Set"
        },
        "src/lib.rs::b::Set::m"
    ));
    assert!(!admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "b::Set"
        },
        "src/lib.rs::a::Set::m"
    ));
    assert!(!admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "b::Set"
        },
        "src/a.rs::Set::m"
    ));
}

#[test]
fn a_mod_rs_file_and_generic_owners_read_like_their_module() {
    let (ev, idx) = (unknown(), index(&[]));
    let caller = "src/lib.rs::run";
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "c::Set"
        },
        "src/c/mod.rs::Set::m"
    ));
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "c::Gen"
        },
        "src/c.rs::Gen<T>::m"
    ));
}

#[test]
fn a_crate_path_is_read_from_the_crate_root_exactly() {
    let (ev, idx) = (two_crates(), index(&[]));
    let caller = "src/b.rs::run";
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "crate::a::Set"
        },
        "src/lib.rs::a::Set::m"
    ));
    assert!(!admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "crate::a::Set"
        },
        "src/lib.rs::x::a::Set::m"
    ));
    // tests/b.rs is another crate: a test target, not the library.
    assert!(!admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "crate::b::Set"
        },
        "tests/b.rs::Set::m"
    ));
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "crate::b::Set"
        },
        "src/b.rs::Set::m"
    ));
}

#[test]
fn a_relative_path_stays_in_the_caller_crate() {
    let (ev, idx) = (two_crates(), index(&[]));
    assert!(!admits(
        &ev,
        &idx,
        Site {
            caller: "src/lib.rs::run",
            hint: "b::Set"
        },
        "tests/b.rs::Set::m"
    ));
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller: "tests/it.rs::t",
            hint: "b::Set"
        },
        "tests/b.rs::Set::m"
    ));
}

#[test]
fn a_library_path_is_read_from_that_library_root() {
    let (ev, idx) = (two_crates(), index(&[]));
    let caller = "tests/it.rs::t";
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "fx::b::Set"
        },
        "src/b.rs::Set::m"
    ));
    assert!(!admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "fx::b::Set"
        },
        "tests/b.rs::Set::m"
    ));
    assert!(!admits(
        &ev,
        &idx,
        Site {
            caller,
            hint: "fx::Set"
        },
        "src/b.rs::Set::m"
    ));
}

#[test]
fn super_climbs_out_of_the_caller_inline_module() {
    let (ev, idx) = (unknown(), index(&["src/lib.rs::c", "src/lib.rs::c::d"]));
    let from_c = "src/lib.rs::c::run";
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller: from_c,
            hint: "super::a::Set"
        },
        "src/lib.rs::a::Set::m"
    ));
    assert!(!admits(
        &ev,
        &idx,
        Site {
            caller: from_c,
            hint: "super::a::Set"
        },
        "src/lib.rs::c::a::Set::m"
    ));
    let from_d = "src/lib.rs::c::d::run";
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller: from_d,
            hint: "super::super::a::Set"
        },
        "src/lib.rs::a::Set::m"
    ));
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller: from_d,
            hint: "super::Set"
        },
        "src/lib.rs::c::Set::m"
    ));
}

#[test]
fn super_from_the_root_of_a_file_declines() {
    let (ev, idx) = (unknown(), index(&["src/lib.rs::c"]));
    assert!(of(&idx, &ev, "src/lib.rs::run", "super::a::Set").is_none());
    assert!(of(&idx, &ev, "src/lib.rs::c::run", "super::super::Set").is_none());
}

#[test]
fn a_method_caller_is_not_mistaken_for_a_module() {
    // `src/lib.rs::Set::run`: `Set` is the impl type, not a module.
    let (ev, idx) = (unknown(), index(&[]));
    assert!(of(&idx, &ev, "src/lib.rs::Set::run", "super::Set").is_none());
}

#[test]
fn self_reads_like_a_relative_path_and_an_empty_rest_declines() {
    let (ev, idx) = (unknown(), index(&[]));
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller: "src/lib.rs::run",
            hint: "self::b::Set"
        },
        "src/b.rs::Set::m"
    ));
    assert!(of(&idx, &ev, "src/lib.rs::run", "crate::").is_none());
}

#[test]
fn path_segments_drop_generic_arguments() {
    assert_eq!(path_segments("a::Gen<a::B>::C"), ["a", "Gen", "C"]);
    assert_eq!(path_segments("::std::fmt"), ["std", "fmt"]);
}

#[test]
fn self_is_read_exactly_at_the_caller_module() {
    // A suffix `b::Set` would also match `x::b::Set`; `self::` names one module.
    let (ev, idx) = (unknown(), index(&["src/lib.rs::c"]));
    let site = || Site {
        caller: "src/lib.rs::run",
        hint: "self::b::Set",
    };
    assert!(!admits(&ev, &idx, site(), "src/x/b.rs::Set::m"));
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller: "src/lib.rs::c::run",
            hint: "self::Set"
        },
        "src/lib.rs::c::Set::m"
    ));
    assert!(!admits(
        &ev,
        &idx,
        Site {
            caller: "src/lib.rs::c::run",
            hint: "self::Set"
        },
        "src/lib.rs::Set::m"
    ));
}

#[test]
fn super_climbs_out_of_a_file_module() {
    let (ev, idx) = (unknown(), index(&[]));
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller: "src/a.rs::run",
            hint: "super::Set"
        },
        "src/lib.rs::Set::m"
    ));
    assert!(admits(
        &ev,
        &idx,
        Site {
            caller: "src/a/b.rs::run",
            hint: "super::Set"
        },
        "src/a/mod.rs::Set::m"
    ));
    assert!(of(&idx, &ev, "src/a.rs::run", "super::super::Set").is_none());
}
