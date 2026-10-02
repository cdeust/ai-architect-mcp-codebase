// indexer::target_context: what the Cargo package says each Rust file is
// (issue #354).
//
// A file is `test`, `bench` or `example` code when it is reached only from such
// targets (`tests/*.rs`, `benches/*.rs`, `examples/*.rs`, and the modules they
// declare) or only through a `mod` declaration gated on `cfg(test)`
// (`#[cfg(test)] mod tests;`). It is `proof` when it is reached only through a
// `mod` declaration gated on `cfg(kani)` (`#[cfg(kani)] #[path = "../kani/x.rs"]
// mod x;`, issue #423): Kani-only code, compiled out of every build but the
// verifier's, as `cfg(test)` code is compiled out of every build but the test
// harness's. The helpers of such a file carry no `#[kani::proof]`, so this is
// the only place their kind is decided. It is `production` when some lib, bin
// or build target reaches it through declarations none of which requires
// `cfg(test)` or `cfg(kani)`.
//
// Method: walk each target's module tree from its entry file (the tree
// `feature_gated` walks), carrying the class of the path. A declaration that
// requires `cfg(test)` turns the class of everything below it into `test`, one
// that requires `cfg(kani)` into `proof`; a declaration's own feature gate is
// ignored, because a module compiled out by a Cargo feature is still production
// code (a build with that feature compiles it), unlike test and Kani-only code.
// A file reached by several paths is `production` if any path is; else the class
// of its non-production paths when they agree; else nothing is decided (a file
// reached both through `cfg(test)` and through `cfg(kani)` is `{test, proof}`:
// undecided, no entry).
//
// Combinations (pinned by tests): `cfg(all(kani, test))` is `test`, because the
// `cfg(test)` requirement is read first and a build that compiles it is the test
// harness's; `cfg(test)` below a `cfg(kani)` module stays `test` (the nearest
// declaration that requires an option decides); `cfg(kani)` below a test-class
// file (a `tests/*.rs` target or a `cfg(test)` module) is `proof`, on purpose:
// the module compiles only under Kani, whatever the class of its parent.
//
// Evidence is positive only. A file no declaration reaches (an orphan, a file
// behind an inline `mod a { mod b; }`, a `kani/` directory outside every
// target) gets no entry: its callers read as `unknown`, which the totals count
// as production. Nothing is guessed from a path name, so a file called
// `tests.rs` declared as a plain `mod tests;` from `lib.rs` is production.
// source: The Rust Reference, "Modules"; Cargo Book, "Package layout"
// (`tests/`, `benches/`, `examples/` targets and their auto-discovery).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::cargo_targets::{TargetKind, TargetMap};
use super::feature_gated::ModuleTree;
use crate::parser::cfg_expr::CfgPredicate;

pub(crate) const PRODUCTION: &str = "production";
const TEST: &str = "test";
const BENCH: &str = "bench";
const EXAMPLE: &str = "example";
const PROOF: &str = "proof";

/// The class of one path from a target root to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Production,
    Test,
    Bench,
    Example,
    Proof,
}

impl Class {
    fn of(kind: TargetKind) -> Self {
        match kind {
            TargetKind::Production => Class::Production,
            TargetKind::Test => Class::Test,
            TargetKind::Bench => Class::Bench,
            TargetKind::Example => Class::Example,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Class::Production => PRODUCTION,
            Class::Test => TEST,
            Class::Bench => BENCH,
            Class::Example => EXAMPLE,
            Class::Proof => PROOF,
        }
    }
}

/// The context of every indexed file some target reaches, by root-relative path.
/// Empty unless the Cargo map is known.
pub(crate) fn analyse(
    root: &Path,
    map: &TargetMap,
    indexed: &BTreeSet<PathBuf>,
) -> BTreeMap<PathBuf, &'static str> {
    let TargetMap::Known { crate_roots, .. } = map else {
        return BTreeMap::new();
    };
    let tree = ModuleTree::new(root, indexed, crate_roots);
    let mut reached: BTreeMap<PathBuf, BTreeSet<Class>> = BTreeMap::new();
    for crate_root in crate_roots {
        if !indexed.contains(&crate_root.entry) {
            continue;
        }
        let mut pending = vec![(crate_root.entry.clone(), Class::of(crate_root.kind))];
        while let Some((file, class)) = pending.pop() {
            if !reached.entry(file.clone()).or_default().insert(class) {
                continue;
            }
            for (decl, target) in tree.children(&file) {
                let below = if requires_option(&decl.cfg, "test") {
                    Class::Test
                } else if requires_option(&decl.cfg, "kani") {
                    Class::Proof
                } else {
                    class
                };
                pending.push((target, below));
            }
        }
    }
    reached
        .into_iter()
        .filter_map(|(file, classes)| decide(&classes).map(|class| (file, class.name())))
        .collect()
}

/// The entry files of every target whose module tree reaches each indexed file,
/// by root-relative path (issue #357): inside a file, `crate` names each of these
/// targets. Every declaration is followed, gated or not, so the `#[cfg(test)]`
/// modules of a library are owned by the library, whose `crate` they see. A
/// file no declaration reaches has no entry. Empty unless the Cargo map is known.
pub(crate) fn owners(
    root: &Path,
    map: &TargetMap,
    indexed: &BTreeSet<PathBuf>,
) -> BTreeMap<PathBuf, BTreeSet<PathBuf>> {
    let TargetMap::Known { crate_roots, .. } = map else {
        return BTreeMap::new();
    };
    let tree = ModuleTree::new(root, indexed, crate_roots);
    let mut owned: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    for crate_root in crate_roots {
        if !indexed.contains(&crate_root.entry) {
            continue;
        }
        let mut pending = vec![crate_root.entry.clone()];
        while let Some(file) = pending.pop() {
            if !owned
                .entry(file.clone())
                .or_default()
                .insert(crate_root.entry.clone())
            {
                continue;
            }
            pending.extend(tree.children(&file).into_iter().map(|(_, target)| target));
        }
    }
    owned
}

/// True when the declaration exists only under the bare option `key` (`test`,
/// `kani`). A declaration whose `cfg` did not parse (`None`) is not gated: it
/// keeps the class above it.
fn requires_option(cfg: &Option<Vec<CfgPredicate>>, key: &str) -> bool {
    cfg.as_ref()
        .is_some_and(|all| CfgPredicate::All(all.clone()).requires_option(key))
}

/// Any production path wins; otherwise the classes must agree.
fn decide(classes: &BTreeSet<Class>) -> Option<Class> {
    if classes.contains(&Class::Production) {
        return Some(Class::Production);
    }
    let mut iter = classes.iter();
    match (iter.next(), iter.next()) {
        (Some(only), None) => Some(*only),
        _ => None,
    }
}

#[cfg(test)]
#[path = "target_context_tests.rs"]
mod tests;
