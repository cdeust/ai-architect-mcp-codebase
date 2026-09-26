// cargo_attribution — one pass's Cargo-derived coverage attribution, shared by
// the full-index and incremental write paths (issues #284, #291, #316).
//
// Both `outside_build_targets` and `feature_gated` derive from the SAME
// `TargetMap`. When that map is unknown (`cargo metadata` failed, or `cargo`
// is not on PATH) neither bucket can be populated, and an empty bucket then
// looks exactly like a clean one. `CargoAttributionStatus` records which case
// produced the buckets, once per report rather than once per bucket, so a
// reader can tell "nothing outside the build" from "nothing was determined".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::cargo_targets::{self, TargetMap};
use super::feature_gated::{self, FileFeatures};
use super::target_context;

/// Whether the Cargo-derived coverage buckets carry information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CargoAttributionStatus {
    /// `cargo metadata` succeeded: the buckets are the answer, empty included.
    Known,
    /// A root `Cargo.toml` and `.rs` files exist, but the target map could not
    /// be built: the buckets are empty because nothing was determined.
    Unknown { detail: String },
    /// No root `Cargo.toml`, or no `.rs` file: there is nothing to attribute.
    NotApplicable { detail: String },
}

impl CargoAttributionStatus {
    /// The wire name of the status.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Known => "known",
            Self::Unknown { .. } => "unknown",
            Self::NotApplicable { .. } => "not_applicable",
        }
    }

    /// The cause, for the two states that have one.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Known => None,
            Self::Unknown { detail } | Self::NotApplicable { detail } => Some(detail),
        }
    }
}

/// What one attribution pass found: the status, and the root-relative
/// (forward-slash) files for each bucket, feature-gated ones with their reason.
#[derive(Debug)]
pub struct CargoAttributions {
    pub status: CargoAttributionStatus,
    pub outside_targets: Vec<String>,
    pub feature_gated: Vec<(String, String)>,
    /// The library crate names of the workspace (`use <name>::X`), empty
    /// unless the status is `Known`. Issues #348 and #349.
    pub crate_names: BTreeSet<String>,
    /// The features each compiled Rust file sees under the default build
    /// (root-relative, forward-slash keys), empty unless the status is `Known`.
    /// Issue #353, part B.
    pub(crate) file_features: BTreeMap<String, FileFeatures>,
    /// What the Cargo package says each reached Rust file is (`production`,
    /// `test`, `bench`, `example`; root-relative, forward-slash keys), empty
    /// unless the status is `Known`. Issue #354.
    pub(crate) target_contexts: BTreeMap<String, &'static str>,
    /// Every target entry file, with its crate name for a library-like target,
    /// and the entries whose module tree reaches each file (root-relative,
    /// forward-slash), empty unless the status is `Known`. Issue #357.
    pub(crate) targets: BTreeMap<String, Option<String>>,
    pub(crate) target_owners: BTreeMap<String, BTreeSet<String>>,
    /// The gate, module path and default-build verdict each Rust file gets from
    /// the `mod` declarations that lead to it, for the files under a gate or
    /// compiled out, empty unless the status is `Known`. Issue #366, part B.
    pub(crate) file_cfg: BTreeMap<String, crate::graph_store::FileCfg>,
}

/// What the Cargo map tells the rest of an index pass, apart from the coverage
/// buckets: the workspace's crate names (issues #348 and #349) and the features
/// each file is compiled with (issue #353). Empty when the map is unknown.
#[derive(Debug, Default)]
pub(crate) struct CargoFacts {
    /// True when the Cargo map was known on this pass.
    pub known: bool,
    pub crate_names: BTreeSet<String>,
    pub file_features: BTreeMap<String, FileFeatures>,
    pub target_contexts: BTreeMap<String, &'static str>,
    pub targets: BTreeMap<String, Option<String>>,
    pub target_owners: BTreeMap<String, BTreeSet<String>>,
    pub file_cfg: BTreeMap<String, crate::graph_store::FileCfg>,
}

impl CargoFacts {
    /// What the resolver reads to accept a receiver type named through a `use`
    /// (issues #348, #349 and #358).
    pub(crate) fn crate_evidence(&self) -> crate::graph_store::import_roots::CrateEvidence {
        crate::graph_store::import_roots::CrateEvidence {
            known: self.known,
            crate_names: self.crate_names.clone(),
            targets: self.targets.clone(),
            owners: self.target_owners.clone(),
        }
    }
}

/// Attributes `rust_files` (root-relative `.rs` paths indexed this pass)
/// against `codebase`'s Cargo targets. Spawns `cargo metadata` only when a
/// root `Cargo.toml` exists and `rust_files` is non-empty.
///
/// Postcondition: the buckets are non-empty only under `Known`.
pub fn attribute(codebase: &Path, rust_files: &BTreeSet<PathBuf>) -> CargoAttributions {
    let not_applicable = |detail: &str| {
        empty(CargoAttributionStatus::NotApplicable {
            detail: detail.to_string(),
        })
    };
    if !codebase.join("Cargo.toml").is_file() {
        return not_applicable("no Cargo.toml at the analyzed root");
    }
    if rust_files.is_empty() {
        return not_applicable("no .rs file indexed");
    }
    match cargo_targets::discover(codebase) {
        TargetMap::Unknown { detail } => empty(CargoAttributionStatus::Unknown { detail }),
        known => attributed(codebase, rust_files, &known),
    }
}

/// An attribution with `status` and nothing in any bucket.
fn empty(status: CargoAttributionStatus) -> CargoAttributions {
    CargoAttributions {
        status,
        outside_targets: Vec::new(),
        feature_gated: Vec::new(),
        crate_names: BTreeSet::new(),
        file_features: BTreeMap::new(),
        target_contexts: BTreeMap::new(),
        targets: BTreeMap::new(),
        target_owners: BTreeMap::new(),
        file_cfg: BTreeMap::new(),
    }
}

/// The attribution of `rust_files` under a known Cargo `map`.
fn attributed(
    codebase: &Path,
    rust_files: &BTreeSet<PathBuf>,
    map: &TargetMap,
) -> CargoAttributions {
    let (crate_names, targets) = match map {
        TargetMap::Known {
            crate_names,
            crate_roots,
            ..
        } => (
            crate_names.clone(),
            crate_roots
                .iter()
                .map(|r| (forward_slash(&r.entry), r.lib_name.clone()))
                .collect(),
        ),
        TargetMap::Unknown { .. } => (BTreeSet::new(), BTreeMap::new()),
    };
    let outside_targets = rust_files
        .iter()
        .map(PathBuf::as_path)
        .filter(|rel| map.is_outside_targets(rel))
        .map(forward_slash)
        .collect();
    let analysis = feature_gated::analyse(codebase, map, rust_files);
    let file_cfg = super::file_scope::analyse(codebase, map, rust_files, &analysis.features);
    let feature_gated = analysis
        .gated
        .into_iter()
        .map(|(rel, detail)| (forward_slash(&rel), detail))
        .collect();
    let file_features = analysis
        .features
        .into_iter()
        .map(|(rel, known)| (forward_slash(&rel), known))
        .collect();
    let target_contexts = target_context::analyse(codebase, map, rust_files)
        .into_iter()
        .map(|(rel, context)| (forward_slash(&rel), context))
        .collect();
    let target_owners = target_context::owners(codebase, map, rust_files)
        .into_iter()
        .map(|(rel, entries)| {
            let entries = entries.iter().map(|e| forward_slash(e)).collect();
            (forward_slash(&rel), entries)
        })
        .collect();
    CargoAttributions {
        status: CargoAttributionStatus::Known,
        outside_targets,
        feature_gated,
        crate_names,
        file_features,
        target_contexts,
        targets,
        target_owners,
        file_cfg,
    }
}

fn forward_slash(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
#[path = "cargo_attribution_tests.rs"]
mod tests;
