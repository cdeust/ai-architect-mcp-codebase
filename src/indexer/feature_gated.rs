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

/// What the default build knows about the features one source file is compiled
/// with (issue #353, part B): the input of `cfg_active`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileFeatures {
    /// Reached only through `mod` declarations the default features compile out.
    CompiledOut,
    /// Every crate root that reaches the file enables exactly these features.
    Enabled(BTreeSet<String>),
    /// Crate roots that reach the file enable different sets (two packages of a
    /// workspace share it): nothing is decided for its items.
    Disagree,
}

/// The two answers one walk of the module trees gives.
pub(crate) struct FeatureAnalysis {
    /// Every feature-gated file, with the coverage detail naming the gate.
    pub gated: BTreeMap<PathBuf, String>,
    /// The features of every file some crate root reaches, and the gated ones.
    pub features: BTreeMap<PathBuf, FileFeatures>,
}

/// Walks every crate's module tree once and reports both which files the
/// default features compile out and which features each compiled file sees.
pub(crate) fn analyse(
    root: &Path,
    map: &TargetMap,
    indexed: &BTreeSet<PathBuf>,
) -> FeatureAnalysis {
    let TargetMap::Known { crate_roots, .. } = map else {
        return FeatureAnalysis {
            gated: BTreeMap::new(),
            features: BTreeMap::new(),
        };
    };
    let tree = ModuleTree::new(root, indexed, crate_roots);
    let mut live: BTreeMap<PathBuf, BTreeSet<BTreeSet<String>>> = BTreeMap::new();
    let mut seeds = Vec::new();
    for crate_root in crate_roots {
        tree.walk_live(crate_root, &mut live, &mut seeds);
    }
    let gated = tree.propagate_gated(seeds, &live);
    let mut features: BTreeMap<PathBuf, FileFeatures> = live
        .into_iter()
        .map(|(file, sets)| {
            let mut sets = sets.into_iter();
            let known = match (sets.next(), sets.next()) {
                (Some(only), None) => FileFeatures::Enabled(only),
                _ => FileFeatures::Disagree,
            };
            (file, known)
        })
        .collect();
    for file in gated.keys() {
        features.insert(file.clone(), FileFeatures::CompiledOut);
    }
    FeatureAnalysis { gated, features }
}

/// What the walk needs that is fixed for the whole codebase. Shared with
/// `target_context`, which walks the same tree for another question.
pub(super) struct ModuleTree<'a> {
    root: &'a Path,
    indexed: &'a BTreeSet<PathBuf>,
    crate_entries: BTreeSet<PathBuf>,
}

impl<'a> ModuleTree<'a> {
    pub(super) fn new(
        root: &'a Path,
        indexed: &'a BTreeSet<PathBuf>,
        crate_roots: &[CrateRoot],
    ) -> Self {
        ModuleTree {
            root,
            indexed,
            crate_entries: crate_roots.iter().map(|c| c.entry.clone()).collect(),
        }
    }
}

impl ModuleTree<'_> {
    /// Marks every file `crate_root` compiles as live, with the feature set that
    /// reaches it, and collects each declaration its default features compile
    /// out as a `(file, detail)` seed. A file already reached with the same
    /// features is not walked again; one reached with other features is.
    fn walk_live(
        &self,
        crate_root: &CrateRoot,
        live: &mut BTreeMap<PathBuf, BTreeSet<BTreeSet<String>>>,
        seeds: &mut Vec<(PathBuf, String)>,
    ) {
        let features = &crate_root.default_features;
        if !self.indexed.contains(&crate_root.entry) || !reach(live, &crate_root.entry, features) {
            return;
        }
        let mut pending = vec![crate_root.entry.clone()];
        while let Some(file) = pending.pop() {
            for (decl, target) in self.children(&file) {
                if gate(&decl, features) == Truth::False {
                    seeds.push((target, gate_detail(&decl, &file)));
                } else if reach(live, &target, features) {
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
        live: &BTreeMap<PathBuf, BTreeSet<BTreeSet<String>>>,
    ) -> BTreeMap<PathBuf, String> {
        let mut gated = BTreeMap::new();
        let mut pending = seeds;
        while let Some((file, detail)) = pending.pop() {
            if live.contains_key(&file) || gated.contains_key(&file) {
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
    pub(super) fn children(&self, file: &Path) -> Vec<(ModDecl, PathBuf)> {
        let owns_directory = self.crate_entries.contains(file) || file.ends_with("mod.rs");
        declared_children(self.root, file, owns_directory, &|p| {
            self.indexed.contains(p)
        })
    }
}

/// The `mod name;` declarations of `file` that resolve to a file `exists`
/// accepts, each alternative of a `cfg_attr(.., path = ..)` apart.
fn declared_children(
    root: &Path,
    file: &Path,
    owns_directory: bool,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<(ModDecl, PathBuf)> {
    let Ok(source) = std::fs::read_to_string(root.join(file)) else {
        return Vec::new();
    };
    if !source.contains("mod") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for decl in rust_mod_decls::mod_decls(&source) {
        for alternative in alternatives(&decl) {
            if let Some(target) = module_file(file, owns_directory, &alternative, exists) {
                out.push((alternative, target));
            }
        }
    }
    out
}

/// Every file on disk that the module tree of some crate root reaches, through
/// any declaration, gated or not, `#[path]` included. A file reached this way is
/// compiled by that target under some configuration, wherever it sits: `#[cfg(kani)]
/// #[path = "../kani/proofs.rs"] mod proofs;` in `src/lib.rs` makes
/// `kani/proofs.rs` part of the library, though no target lives in `kani/`.
pub(super) fn reached_on_disk(root: &Path, crate_roots: &[CrateRoot]) -> BTreeSet<PathBuf> {
    let entries: BTreeSet<&Path> = crate_roots.iter().map(|c| c.entry.as_path()).collect();
    let exists = |p: &Path| root.join(p).is_file();
    let mut reached = BTreeSet::new();
    let mut pending: Vec<PathBuf> = entries
        .iter()
        .filter(|e| exists(e))
        .map(|e| e.to_path_buf())
        .collect();
    while let Some(file) = pending.pop() {
        if !reached.insert(file.clone()) {
            continue;
        }
        let owns_directory = entries.contains(file.as_path()) || file.ends_with("mod.rs");
        for (_, target) in declared_children(root, &file, owns_directory, &exists) {
            pending.push(target);
        }
    }
    reached
}

/// Records that `file` is reached with `features`; true when that pair is new.
fn reach(
    live: &mut BTreeMap<PathBuf, BTreeSet<BTreeSet<String>>>,
    file: &Path,
    features: &BTreeSet<String>,
) -> bool {
    live.entry(file.to_path_buf())
        .or_default()
        .insert(features.clone())
}

/// The files one declaration can name, each with the gate under which it does
/// (issue #366): one declaration per `cfg_attr(pred, path = ..)`, reaching the
/// file `path` names under the declaration's own `cfg` AND `pred`, then the
/// declaration itself, reaching its `#[path]` or default-name file under its
/// own `cfg` AND the negation of every such `pred` (the file is used only when
/// no `cfg_attr` path applies). A gate one part of which did not parse is
/// undecidable (`None`). A declaration without `cfg_attr` paths is returned as
/// it is.
fn alternatives(decl: &ModDecl) -> Vec<ModDecl> {
    let gated = |extra: Option<CfgPredicate>| -> Option<Vec<CfgPredicate>> {
        let mut all = decl.cfg.clone()?;
        all.push(extra?);
        Some(all)
    };
    let mut out: Vec<ModDecl> = decl
        .alt_paths
        .iter()
        .map(|alt| ModDecl {
            name: decl.name.clone(),
            path_attr: Some(alt.path.clone()),
            cfg: gated(alt.pred.clone()),
            cfg_text: decl.cfg_text.clone(),
            alt_paths: Vec::new(),
        })
        .collect();
    let fallback_cfg = || -> Option<Vec<CfgPredicate>> {
        let mut all = decl.cfg.clone()?;
        for alt in &decl.alt_paths {
            all.push(CfgPredicate::Not(Box::new(alt.pred.clone()?)));
        }
        Some(all)
    };
    out.push(ModDecl {
        cfg: fallback_cfg(),
        alt_paths: Vec::new(),
        ..decl.clone()
    });
    out
}

/// The cfg gate of a declaration: `all` of its `cfg` attributes, `Unknown`
/// when one did not parse.
fn gate(decl: &ModDecl, enabled: &BTreeSet<String>) -> Truth {
    match &decl.cfg {
        Some(all) => CfgPredicate::All(all.clone()).eval(enabled),
        None => Truth::Unknown,
    }
}

/// Resolves `decl`, declared in `file`, to a file `exists` accepts: `#[path]`
/// is relative to `file`'s directory, `..` included (`#[path =
/// "../kani/proofs.rs"]` in `src/lib.rs` names `kani/proofs.rs`); otherwise
/// `<dir>/name.rs`, then `<dir>/name/mod.rs`, where `<dir>` is `file`'s
/// directory for a crate root or a `mod.rs`, and `file`'s directory joined with
/// its stem otherwise. A path that climbs above the analyzed root names nothing.
/// source: The Rust Reference, "Modules" — module source filenames and the
/// `path` attribute.
fn module_file(
    file: &Path,
    owns_directory: bool,
    decl: &ModDecl,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let parent = file.parent().unwrap_or(Path::new(""));
    if let Some(path) = &decl.path_attr {
        return lexical(&parent.join(path)).filter(|p| exists(p));
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
    .find(|p| exists(p))
}

/// `path` with its `.` and `..` components folded away, `None` when it climbs
/// above its first component or is absolute. The indexed set holds such
/// root-relative paths, so `src/../kani/x.rs` must read `kani/x.rs` to be found.
fn lexical(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(part) => out.push(part),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
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
