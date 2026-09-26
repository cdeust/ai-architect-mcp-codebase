// indexer::file_scope: the gate and the logical module path a Rust file gets
// from the `mod` declarations that lead to it (issue #366, part B).
//
// `#[cfg(unix)] #[path = "unix.rs"] mod imp;` and `#[cfg(windows)] #[path =
// "windows.rs"] mod imp;` put two files in the place of ONE module, `imp`: an
// item of one is the twin of the item of the same name in the other, though
// their ids share nothing but the name. The walk gives every file the module
// path its declarations spell (the crate root's path, then each `mod` name) and
// the conjunction of their `cfg` attributes, so the resolver can tell such twins
// apart from two unrelated items that happen to share a name.
//
// A file reached in more than one way keeps no module path and no gate: its
// items are not read as twins, and a call to them stays an ordinary ambiguity
// with no edge. The
// file's verdict under the default build is `inactive` when every path to it is
// compiled out (`FileFeatures::CompiledOut`), `active` when one path holds under
// the package's default features, `unknown` otherwise (a bare `unix`, a gate
// that did not parse). The gate is never folded into an item's own gate: an
// `unknown` file gate says nothing about the twins inside the file.
// source: The Rust Reference, "Modules" and "Conditional compilation".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::cargo_targets::TargetMap;
use super::feature_gated::{FileFeatures, ModuleTree};
use crate::graph_store::{FileCfg, CFG_ACTIVE, CFG_INACTIVE, CFG_UNKNOWN};
use crate::parser::cfg_expr::{CfgPredicate, Truth};

/// Gate text for a conjunction one member of which did not parse.
const UNPARSED_GATE: &str = "?";

/// One way a crate root reaches a file.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Reach {
    module_path: String,
    gate: String,
    truth: u8,
}

/// The facts of every indexed file under a gate or compiled out, by
/// root-relative forward-slash id. Empty when the Cargo map is not known.
pub(crate) fn analyse(
    root: &Path,
    map: &TargetMap,
    indexed: &BTreeSet<PathBuf>,
    features: &BTreeMap<PathBuf, FileFeatures>,
) -> BTreeMap<String, FileCfg> {
    let TargetMap::Known { crate_roots, .. } = map else {
        return BTreeMap::new();
    };
    let tree = ModuleTree::new(root, indexed, crate_roots);
    let mut reaches: BTreeMap<PathBuf, BTreeSet<Reach>> = BTreeMap::new();
    for crate_root in crate_roots {
        if !indexed.contains(&crate_root.entry) {
            continue;
        }
        let enabled = &crate_root.default_features;
        let start = (
            crate_root.entry.clone(),
            id_of(&crate_root.entry),
            Some(Vec::new()),
        );
        let mut pending = vec![start];
        let mut seen: BTreeSet<(PathBuf, String)> = BTreeSet::new();
        while let Some((file, module_path, gates)) = pending.pop() {
            if !seen.insert((file.clone(), module_path.clone())) {
                continue;
            }
            reaches.entry(file.clone()).or_default().insert(reach(
                &module_path,
                gates.as_deref(),
                enabled,
            ));
            for (decl, target) in tree.children(&file) {
                let child_gates = match (&gates, &decl.cfg) {
                    (Some(outer), Some(own)) => Some(outer.iter().chain(own).cloned().collect()),
                    _ => None,
                };
                pending.push((target, format!("{module_path}::{}", decl.name), child_gates));
            }
        }
    }
    let mut out = BTreeMap::new();
    for (file, set) in reaches {
        let compiled_out = matches!(features.get(&file), Some(FileFeatures::CompiledOut));
        let facts = scope(&set, compiled_out);
        if !facts.gate.is_empty() || facts.active == CFG_INACTIVE {
            out.insert(id_of(&file), facts);
        }
    }
    out
}

fn reach(module_path: &str, gates: Option<&[CfgPredicate]>, enabled: &BTreeSet<String>) -> Reach {
    let (gate, truth) = match gates {
        None => (UNPARSED_GATE.to_string(), Truth::Unknown),
        Some([]) => (String::new(), Truth::True),
        Some(all) => {
            let predicate = CfgPredicate::All(all.to_vec());
            let truth = predicate.eval(enabled);
            (predicate.canonical().compact(), truth)
        }
    };
    let truth = match truth {
        Truth::True => 2,
        Truth::Unknown => 1,
        Truth::False => 0,
    };
    Reach {
        module_path: module_path.to_string(),
        gate,
        truth,
    }
}

/// The facts of a file from every way it is reached.
fn scope(reaches: &BTreeSet<Reach>, compiled_out: bool) -> FileCfg {
    let (module_path, gate) = match reaches.iter().collect::<Vec<_>>().as_slice() {
        [only] => (only.module_path.clone(), only.gate.clone()),
        _ => (String::new(), String::new()),
    };
    let best = reaches.iter().map(|r| r.truth).max().unwrap_or(1);
    let active = if compiled_out || best == 0 {
        CFG_INACTIVE
    } else if best == 2 {
        CFG_ACTIVE
    } else {
        CFG_UNKNOWN
    };
    FileCfg {
        module_path,
        gate,
        active: active.to_string(),
    }
}

fn id_of(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reached(module_path: &str, gate: &str, truth: u8) -> Reach {
        Reach {
            module_path: module_path.to_string(),
            gate: gate.to_string(),
            truth,
        }
    }

    #[test]
    fn one_way_in_keeps_its_module_path_and_gate() {
        let set = BTreeSet::from([reached("src/lib.rs::imp", "unix", 1)]);
        let facts = scope(&set, false);
        assert_eq!(facts.module_path, "src/lib.rs::imp");
        assert_eq!(facts.gate, "unix");
        assert_eq!(facts.active, CFG_UNKNOWN);
    }

    #[test]
    fn two_ways_in_keep_neither_and_one_true_path_compiles_the_file() {
        let set = BTreeSet::from([
            reached("src/lib.rs::a", "unix", 1),
            reached("src/lib.rs::b", "", 2),
        ]);
        let facts = scope(&set, false);
        assert_eq!((facts.module_path.as_str(), facts.gate.as_str()), ("", ""));
        assert_eq!(facts.active, CFG_ACTIVE);
    }

    #[test]
    fn a_compiled_out_file_is_inactive_whatever_its_gate_says() {
        let set = BTreeSet::from([reached("src/lib.rs::imp", "feature=fast", 0)]);
        assert_eq!(scope(&set, true).active, CFG_INACTIVE);
    }

    #[test]
    fn the_gate_is_the_compact_conjunction_of_the_declarations() {
        let enabled = BTreeSet::new();
        let unix = CfgPredicate::Option {
            key: "unix".into(),
            value: None,
        };
        let fast = CfgPredicate::Feature("fast".into());
        let r = reach("m", Some(&[unix, fast]), &enabled);
        assert_eq!(
            r.truth, 0,
            "feature=fast is off, so the conjunction is false"
        );
        assert!(
            r.gate.contains("unix") && r.gate.contains("feature=fast"),
            "{}",
            r.gate
        );
        assert_eq!(reach("m", None, &enabled).gate, UNPARSED_GATE);
        assert_eq!(reach("m", Some(&[]), &enabled).gate, "");
    }
}
