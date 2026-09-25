// feature_gated — indexed Rust files the default build compiles out through a
// `#[cfg(feature = "...")]` on their `mod` declaration (issue #291).
//
// Layer: indexer support, sibling of `cargo_targets`. Such a file sits inside
// a compiled target's directory, so `TargetMap::is_outside_targets` (#284)
// calls it compiled; rust-analyzer, which loads the crate with default
// features, never links it (measured 2026-09-23, rust-analyzer 1.95.0:
// `unlinked-file` on the module file, `inactive-code` on its `mod` item), so
// no call in it can be resolved through the language server.
//
// Method: walk each crate's module tree from its target entry file, following
// `mod name;` declarations to files. A file is LIVE when some path from a
// crate root reaches it through declarations whose cfg is not `False` under
// that package's default features; it is FEATURE-GATED when it is reached
// only through a declaration whose cfg IS `False` (`cfg_expr` decides `False`
// from feature leaves alone, so `cfg(test)`, `cfg(unix)` never gate).
// A file no declaration reaches is not this module's business (#292's
// `unlinked_file` covers the orphan case). Only files in the indexed set are
// ever classified, so a mis-resolved path produces nothing.

use super::cargo_targets::{CrateRoot, TargetMap};
use super::rust_mod_decls::{self, ModDecl};
use crate::parser::cfg_expr::{CfgPredicate, Truth};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Every feature-gated file in `indexed` (root-relative), with the coverage
/// detail naming the gate. Reads each reachable `.rs` file under `root` once.
pub(crate) fn find_feature_gated(
    root: &Path,
    map: &TargetMap,
    indexed: &BTreeSet<PathBuf>,
) -> BTreeMap<PathBuf, String> {
    let TargetMap::Known { crate_roots, .. } = map else {
        return BTreeMap::new();
    };
    let tree = ModuleTree {
        root,
        indexed,
        crate_entries: crate_roots.iter().map(|c| c.entry.clone()).collect(),
    };
    let mut live = BTreeSet::new();
    let mut seeds = Vec::new();
    for crate_root in crate_roots {
        tree.walk_live(crate_root, &mut live, &mut seeds);
    }
    tree.propagate_gated(seeds, &live)
}

/// What the walk needs that is fixed for the whole codebase.
struct ModuleTree<'a> {
    root: &'a Path,
    indexed: &'a BTreeSet<PathBuf>,
    crate_entries: BTreeSet<PathBuf>,
}

impl ModuleTree<'_> {
    /// Marks every file `crate_root` compiles as live, and collects each
    /// declaration its default features compile out as a `(file, detail)` seed.
    fn walk_live(
        &self,
        crate_root: &CrateRoot,
        live: &mut BTreeSet<PathBuf>,
        seeds: &mut Vec<(PathBuf, String)>,
    ) {
        if !self.indexed.contains(&crate_root.entry) || !live.insert(crate_root.entry.clone()) {
            return;
        }
        let mut pending = vec![crate_root.entry.clone()];
        while let Some(file) = pending.pop() {
            for (decl, target) in self.children(&file) {
                if gate(&decl, &crate_root.default_features) == Truth::False {
                    seeds.push((target, gate_detail(&decl, &file)));
                } else if live.insert(target.clone()) {
                    pending.push(target);
                }
            }
        }
    }

    /// Everything a gated seed declares is compiled out with it, unless some
    /// other path made it live.
    fn propagate_gated(
        &self,
        seeds: Vec<(PathBuf, String)>,
        live: &BTreeSet<PathBuf>,
    ) -> BTreeMap<PathBuf, String> {
        let mut gated = BTreeMap::new();
        let mut pending = seeds;
        while let Some((file, detail)) = pending.pop() {
            if live.contains(&file) || gated.contains_key(&file) {
                continue;
            }
            for (_, target) in self.children(&file) {
                let via = file.to_string_lossy().replace('\\', "/");
                pending.push((target, format!("declared in {via}, itself {detail}")));
            }
            gated.insert(file, detail);
        }
        gated
    }

    /// The `mod name;` declarations of `file` that resolve to an indexed file.
    fn children(&self, file: &Path) -> Vec<(ModDecl, PathBuf)> {
        let Ok(source) = std::fs::read_to_string(self.root.join(file)) else {
            return Vec::new();
        };
        if !source.contains("mod") {
            return Vec::new();
        }
        let owns_directory = self.crate_entries.contains(file) || file.ends_with("mod.rs");
        rust_mod_decls::mod_decls(&source)
            .into_iter()
            .filter_map(|decl| {
                let target = module_file(file, owns_directory, &decl, self.indexed)?;
                Some((decl, target))
            })
            .collect()
    }
}

/// The cfg gate of a declaration: `all` of its `cfg` attributes, `Unknown`
/// when one did not parse.
fn gate(decl: &ModDecl, enabled: &BTreeSet<String>) -> Truth {
    match &decl.cfg {
        Some(all) => CfgPredicate::All(all.clone()).eval(enabled),
        None => Truth::Unknown,
    }
}

/// Resolves `decl`, declared in `file`, to an indexed file: `#[path]` is
/// relative to `file`'s directory; otherwise `<dir>/name.rs`, then
/// `<dir>/name/mod.rs`, where `<dir>` is `file`'s directory for a crate root
/// or a `mod.rs`, and `file`'s directory joined with its stem otherwise.
/// source: The Rust Reference, "Modules" — module source filenames.
fn module_file(
    file: &Path,
    owns_directory: bool,
    decl: &ModDecl,
    indexed: &BTreeSet<PathBuf>,
) -> Option<PathBuf> {
    let parent = file.parent().unwrap_or(Path::new(""));
    if let Some(path) = &decl.path_attr {
        return Some(parent.join(path)).filter(|p| indexed.contains(p));
    }
    let dir = if owns_directory {
        parent.to_path_buf()
    } else {
        parent.join(file.file_stem()?)
    };
    [
        dir.join(format!("{}.rs", decl.name)),
        dir.join(&decl.name).join("mod.rs"),
    ]
    .into_iter()
    .find(|p| indexed.contains(p))
}

fn gate_detail(decl: &ModDecl, file: &Path) -> String {
    format!(
        "compiled out under the package's default features (cargo metadata): \
         {} on `mod {};` in {} is false; declarations are indexed, calls in it \
         cannot be resolved by the language server",
        decl.cfg_text,
        decl.name,
        file.to_string_lossy().replace('\\', "/")
    )
}

#[cfg(test)]
#[path = "feature_gated_tests.rs"]
mod tests;
