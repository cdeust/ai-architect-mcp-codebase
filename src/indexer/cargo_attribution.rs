// cargo_attribution — one pass's Cargo-derived coverage attribution, shared by
// the full-index and incremental write paths (issues #284, #291, #316).
//
// Both `outside_build_targets` and `feature_gated` derive from the SAME
// `TargetMap`. When that map is unknown (`cargo metadata` failed, or `cargo`
// is not on PATH) neither bucket can be populated, and an empty bucket then
// looks exactly like a clean one. `CargoAttributionStatus` records which case
// produced the buckets, once per report rather than once per bucket, so a
// reader can tell "nothing outside the build" from "nothing was determined".

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::cargo_targets::{self, TargetMap};
use super::feature_gated;

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
}

/// Attributes `rust_files` (root-relative `.rs` paths indexed this pass)
/// against `codebase`'s Cargo targets. Spawns `cargo metadata` only when a
/// root `Cargo.toml` exists and `rust_files` is non-empty.
///
/// Postcondition: the buckets are non-empty only under `Known`.
pub fn attribute(codebase: &Path, rust_files: &BTreeSet<PathBuf>) -> CargoAttributions {
    let not_applicable = |detail: &str| CargoAttributions {
        status: CargoAttributionStatus::NotApplicable {
            detail: detail.to_string(),
        },
        outside_targets: Vec::new(),
        feature_gated: Vec::new(),
        crate_names: BTreeSet::new(),
    };
    if !codebase.join("Cargo.toml").is_file() {
        return not_applicable("no Cargo.toml at the analyzed root");
    }
    if rust_files.is_empty() {
        return not_applicable("no .rs file indexed");
    }
    let map = cargo_targets::discover(codebase);
    if let TargetMap::Unknown { detail } = map {
        return CargoAttributions {
            status: CargoAttributionStatus::Unknown { detail },
            outside_targets: Vec::new(),
            feature_gated: Vec::new(),
            crate_names: BTreeSet::new(),
        };
    }
    let crate_names = match &map {
        TargetMap::Known { crate_names, .. } => crate_names.clone(),
        TargetMap::Unknown { .. } => BTreeSet::new(),
    };
    let outside_targets = rust_files
        .iter()
        .map(PathBuf::as_path)
        .filter(|rel| map.is_outside_targets(rel))
        .map(forward_slash)
        .collect();
    let feature_gated = feature_gated::find_feature_gated(codebase, &map, rust_files)
        .into_iter()
        .map(|(rel, detail)| (forward_slash(&rel), detail))
        .collect();
    CargoAttributions {
        status: CargoAttributionStatus::Known,
        outside_targets,
        feature_gated,
        crate_names,
    }
}

fn forward_slash(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
#[path = "cargo_attribution_tests.rs"]
mod tests;
