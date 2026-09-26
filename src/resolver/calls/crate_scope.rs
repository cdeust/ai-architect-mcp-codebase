// resolver::calls::crate_scope: which candidates a receiver type shown by
// `use crate::X` (or `self::`, `super::`) may resolve to (issue #357).
//
// Inside a Cargo test, bench, example or binary target, `crate` names that
// target, not the library. A test file whose root re-exports an external type
// (`pub use some_ext::Set;`) and reaches it through `use crate::Set;` names that
// external type, and the lookup by last segment would match the library's
// unrelated `Set`. So for a file one of whose owning targets is not a library,
// a candidate is kept only when that target's own module tree defines it, or,
// for exactly `crate::X`, when the target's root imports `X` from a path whose
// first segment is a library crate of the repository (then the candidate must
// live in that library's tree). A re-export of any other path adds nothing, so
// the call is declined. A file owned by several targets must pass for each.
//
// A file owned only by library targets is accepted as before. So is every file
// when the Cargo facts are unknown (no `Cargo.toml`, `cargo` missing) or when no
// target reaches the file: without cargo there is no non-library target to
// confuse, and declining would lose every `crate::` edge of a cargo-less tree
// (owner's decision, recorded in the CHANGELOG).
// source: The Rust Reference, "Paths" (`crate` names the root of the current
// crate); Cargo Book, "Cargo Targets" (each target is its own crate).

use std::collections::{BTreeSet, HashMap};

use crate::graph_store::import_roots::CrateEvidence;

/// The target entry files each non-library owner of a file accepts candidates
/// from; empty when nothing restricts the candidates.
pub(super) struct CrateScope {
    per_owner: Vec<BTreeSet<String>>,
}

impl CrateScope {
    /// The scope of a hint whose type the file shows through `path`
    /// (`crate::Set`, `crate::shapes::Set`, `super::Set`), for a caller in
    /// `caller_file`.
    pub(super) fn of(
        evidence: &CrateEvidence,
        file_imports: &HashMap<String, Vec<String>>,
        caller_file: &str,
        path: &str,
    ) -> Self {
        let mut per_owner = Vec::new();
        if !evidence.known {
            return CrateScope { per_owner };
        }
        let Some(owners) = evidence.owners.get(caller_file) else {
            return CrateScope { per_owner };
        };
        for owner in owners {
            if matches!(evidence.targets.get(owner), Some(Some(_))) {
                continue;
            }
            let mut accepted = BTreeSet::from([owner.clone()]);
            if let Some(name) = path.strip_prefix("crate::").filter(|n| !n.contains("::")) {
                accepted.extend(libraries_importing(evidence, file_imports, owner, name));
            }
            per_owner.push(accepted);
        }
        CrateScope { per_owner }
    }

    /// True when a candidate defined in `candidate_file` passes every owner.
    pub(super) fn admits(&self, evidence: &CrateEvidence, candidate_file: &str) -> bool {
        let candidate_owners = evidence.owners.get(candidate_file);
        self.per_owner.iter().all(|accepted| {
            candidate_owners.is_some_and(|owners| owners.iter().any(|o| accepted.contains(o)))
        })
    }

    /// True when some owner restricts the candidates.
    pub(super) fn restricts(&self) -> bool {
        !self.per_owner.is_empty()
    }
}

/// The entry files of the libraries of the repository from which the root file
/// `root` imports `name` (`use my_lib::Set;` or `pub use my_lib::Set;` at the
/// root of a test target). An import of any other path gives nothing.
fn libraries_importing(
    evidence: &CrateEvidence,
    file_imports: &HashMap<String, Vec<String>>,
    root: &str,
    name: &str,
) -> Vec<String> {
    let crates: Vec<&str> = file_imports
        .get(root)
        .into_iter()
        .flatten()
        .filter(|path| path.rsplit("::").next() == Some(name) && path.contains("::"))
        .filter_map(|path| path.split("::").next())
        .collect();
    evidence
        .targets
        .iter()
        .filter(|(_, lib)| lib.as_deref().is_some_and(|l| crates.contains(&l)))
        .map(|(entry, _)| entry.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn evidence(targets: &[(&str, Option<&str>)], owners: &[(&str, &[&str])]) -> CrateEvidence {
        CrateEvidence {
            known: true,
            crate_names: targets
                .iter()
                .filter_map(|(_, l)| l.map(str::to_string))
                .collect(),
            targets: targets
                .iter()
                .map(|(e, l)| (e.to_string(), l.map(str::to_string)))
                .collect(),
            owners: owners
                .iter()
                .map(|(f, es)| (f.to_string(), es.iter().map(|e| e.to_string()).collect()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn no_imports() -> HashMap<String, Vec<String>> {
        HashMap::new()
    }

    const TARGETS: [(&str, Option<&str>); 5] = [
        ("src/lib.rs", Some("fx")),
        ("src/bin/x.rs", None),
        ("tests/it.rs", None),
        ("benches/b.rs", None),
        ("examples/e.rs", None),
    ];

    fn owners() -> Vec<(&'static str, &'static [&'static str])> {
        vec![
            ("src/lib.rs", &["src/lib.rs"]),
            ("src/shared.rs", &["src/lib.rs", "src/bin/x.rs"]),
            ("src/bin/x.rs", &["src/bin/x.rs"]),
            ("tests/it.rs", &["tests/it.rs"]),
            ("tests/common/mod.rs", &["tests/it.rs"]),
            ("benches/b.rs", &["benches/b.rs"]),
            ("examples/e.rs", &["examples/e.rs"]),
        ]
    }

    #[test]
    fn a_file_owned_only_by_the_library_is_not_restricted() {
        let ev = evidence(&TARGETS, &owners());
        let scope = CrateScope::of(&ev, &no_imports(), "src/lib.rs", "crate::Set");
        assert!(!scope.restricts());
    }

    #[test]
    fn each_non_library_target_keeps_only_candidates_of_its_own_tree() {
        let ev = evidence(&TARGETS, &owners());
        for (file, own) in [
            ("tests/it.rs", "tests/common/mod.rs"),
            ("benches/b.rs", "benches/b.rs"),
            ("examples/e.rs", "examples/e.rs"),
            ("src/bin/x.rs", "src/shared.rs"),
        ] {
            let scope = CrateScope::of(&ev, &no_imports(), file, "crate::Set");
            assert!(scope.restricts(), "{file}");
            assert!(scope.admits(&ev, own), "{file} admits {own}");
            assert!(
                !scope.admits(&ev, "src/lib.rs"),
                "{file} rejects the library"
            );
        }
    }

    #[test]
    fn a_file_reached_by_the_library_and_a_bin_must_pass_for_the_bin() {
        let ev = evidence(&TARGETS, &owners());
        let scope = CrateScope::of(&ev, &no_imports(), "src/shared.rs", "crate::Set");
        assert!(scope.restricts());
        assert!(scope.admits(&ev, "src/shared.rs"));
        assert!(
            !scope.admits(&ev, "src/lib.rs"),
            "the bin's crate is not the library"
        );
    }

    #[test]
    fn unknown_facts_or_an_unowned_file_are_not_restricted() {
        let mut ev = evidence(&TARGETS, &owners());
        let unowned = CrateScope::of(&ev, &no_imports(), "kani/proof.rs", "crate::Set");
        assert!(!unowned.restricts());
        ev.known = false;
        let unknown = CrateScope::of(&ev, &no_imports(), "tests/it.rs", "crate::Set");
        assert!(!unknown.restricts());
    }

    #[test]
    fn a_root_import_from_a_library_of_the_repository_admits_that_library() {
        let ev = evidence(&TARGETS, &owners());
        let imports = HashMap::from([("tests/it.rs".to_string(), vec!["fx::Set".to_string()])]);
        let scope = CrateScope::of(&ev, &imports, "tests/common/mod.rs", "crate::Set");
        assert!(scope.admits(&ev, "src/lib.rs"));
        let deeper = CrateScope::of(&ev, &imports, "tests/common/mod.rs", "crate::common::Set");
        assert!(
            !deeper.admits(&ev, "src/lib.rs"),
            "only exactly crate::X reads the root"
        );
    }

    #[test]
    fn a_root_re_export_of_an_external_path_admits_nothing_more() {
        let ev = evidence(&TARGETS, &owners());
        let imports =
            HashMap::from([("tests/it.rs".to_string(), vec!["some_ext::Set".to_string()])]);
        let scope = CrateScope::of(&ev, &imports, "tests/it.rs", "crate::Set");
        assert!(!scope.admits(&ev, "src/lib.rs"));
        assert!(scope.admits(&ev, "tests/it.rs"));
    }
}
