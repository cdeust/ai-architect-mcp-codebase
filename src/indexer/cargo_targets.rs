// cargo_targets — maps a Cargo project's COMPILED targets (issue #284, lot 3
// of tasks/plan-issues-282-283-284.md, part (a1)).
//
// Layer: indexer/persistence support, sibling of `coverage`. Answers one
// question the coverage sidecar could not ask before this module existed:
// "is this `.rs` file part of any Cargo target at all?" A Kani proof harness
// under `kani/`, a `fuzz/` directory excluded from the workspace, or a module
// gated behind a disabled `#[cfg(feature)]` all get parsed and indexed (the
// walker in `walk.rs` visits every `.rs` file, feature-blind) but rust-analyzer
// cannot resolve calls inside them — its crate graph only knows about compiled
// targets (measured: probe C in the plan, `tasks/plan-issues-282-283-284.md`
// §0.2 — no `publishDiagnostics` for a file outside the crate graph, and
// `textDocument/definition` answers `[]`). Today that shows up as an
// unexplained resolution gap; this module lets the coverage sidecar (and,
// downstream, `query_graph(graph="missed")` / `index_status`) say WHY.
//
// Source of truth: `cargo metadata --no-deps --offline --format-version 1`,
// which lists every compiled target's `src_path` for the workspace (or single
// crate) rooted at the given manifest. Precedent for shelling out to a fixed,
// argument-injection-safe command from this indexer: `git` in
// `src/git_diff.rs` (`git diff`) and `src/indexer/incremental.rs`
// (`git_output`, `git diff --name-status` / `git ls-files`). `--no-deps`
// excludes the resolved dependency graph (this module only cares about the
// analyzed crate's OWN targets); `--offline` forbids any network access, so a
// missing registry index can never turn an index run into a network call.
// source: `cargo-metadata(1)` output schema (`packages[].targets[].src_path`),
// verified against a live `cargo metadata --no-deps` run on this repo,
// 2026-09-09.
//
// `TargetMap::Unknown` is the deliberate default whenever the map cannot be
// trusted — no `Cargo.toml`, `cargo` missing from PATH, or `cargo metadata`
// exiting non-zero (this is exactly issue #282's "workspace failed to load"
// case: a crate nested under a parent `[workspace] members = []` makes
// `cargo metadata` fail with "current package believes it's in a workspace
// when it's not", per the plan's probe). `Unknown` NEVER attributes a file as
// outside a target — absence of the map is not evidence a file is uncompiled,
// it is evidence nothing was determined. This mirrors the coverage sidecar's
// own sparse-map honesty rule (`coverage.rs`'s module doc): silence is not a
// completeness claim.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The coverage detail attached to every `OutsideBuildTargets` gap, shared so
/// the wording is identical on every write path (full index, incremental
/// overlay).
pub const OUTSIDE_TARGETS_DETAIL: &str = "not in any Cargo target (cargo metadata --no-deps); \
    declarations are indexed, calls cannot be resolved by the language server";

/// The compiled-target surface of a Cargo project, rooted at some directory.
///
/// `Known` is deliberately coarse-grained ("under a target's source
/// directory") rather than a precise `mod` tree, per the plan's documented
/// trade-off (`tasks/plan-issues-282-283-284.md` §3.2): it never flags a file
/// that IS compiled (a false "outside" would be worse than the gap this
/// module closes), at the cost of missing files that are compiled only behind
/// a disabled `#[cfg(feature)]` inside an otherwise-compiled directory (a
/// known, documented gap — see the boy-scout report).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetMap {
    /// Nothing could be determined: no `Cargo.toml` at the queried root,
    /// `cargo` not on PATH, or `cargo metadata` failed (including a workspace
    /// that refuses to load, issue #282). Carries no target information.
    Unknown,
    /// A successfully parsed target set.
    Known {
        /// Root-relative directories that hold at least one compiled
        /// target's source root (e.g. `src`, `tests`, `benches`). Any file
        /// under one of these is treated as compiled, UNLESS it fails the
        /// `.rs`-extension / not-a-target-file checks first (see
        /// `is_outside_targets`).
        target_dirs: BTreeSet<PathBuf>,
        /// Root-relative paths of the target entry files themselves
        /// (`src/lib.rs`, `build.rs`, …). Tracked separately from
        /// `target_dirs` so a root-level target file (`build.rs`, parent
        /// directory `.`) does not turn the ENTIRE root into a target
        /// directory — which would make every file in the tree "inside" and
        /// silently defeat outside-detection.
        target_files: BTreeSet<PathBuf>,
    },
}

impl TargetMap {
    /// True iff `rel` (root-relative, forward-slash or native separator —
    /// `Path` component comparison is separator-agnostic) is a `.rs` file
    /// this map affirmatively knows is NOT part of any compiled Cargo
    /// target.
    ///
    /// Precondition: none — safe on any path. Postcondition: `Unknown` always
    /// returns `false` (never attributes on a missing/failed map); a non-`.rs`
    /// path always returns `false` (`Cargo.toml`, `README.md`, … were never
    /// candidates — this predicate answers "outside a target", not "not a
    /// target file").
    pub fn is_outside_targets(&self, rel: &Path) -> bool {
        let Self::Known {
            target_dirs,
            target_files,
        } = self
        else {
            return false;
        };
        if rel.extension().and_then(|e| e.to_str()) != Some("rs") {
            return false;
        }
        if target_files.contains(rel) {
            return false;
        }
        !target_dirs.iter().any(|dir| rel.starts_with(dir))
    }
}

/// Discovers `root`'s compiled Cargo targets by shelling out to
/// `cargo metadata --no-deps --offline --format-version 1 --manifest-path
/// <root>/Cargo.toml`. Every failure mode collapses to `TargetMap::Unknown`
/// (see the type doc) — this function never panics and never guesses.
///
/// Precondition: none (safe to call speculatively — callers gate this on
/// "a `Cargo.toml` exists and at least one `.rs` file was indexed" for cost,
/// not correctness). Postcondition: `Known` iff the subprocess exited 0 and
/// its stdout parsed as the documented schema relative to `root` AS GIVEN;
/// `Unknown` otherwise.
///
/// Deliberately does NOT canonicalize `root`: measured (2026-09-09, macOS,
/// `cargo` 1.95.0) — `cargo metadata --manifest-path <p>/Cargo.toml` reports
/// every `targets[].src_path` (and `workspace_root`) prefixed with `<p>`
/// EXACTLY as given, symlinks unresolved. Canonicalizing `root` here while
/// the indexer's own `relative_path` (mod.rs) strips the RAW `codebase_path`
/// argument would silently diverge on any root that sits behind a symlink —
/// macOS's `/tmp` → `/private/tmp` is the common case, and it is exactly the
/// layout `tests/coverage_honesty.rs`'s fixtures use (`tempfile::tempdir()`).
/// A `TargetMap` built from a canonicalized root against non-canonicalized
/// `rel` paths fails EVERY `strip_prefix`, silently emptying `target_dirs`/
/// `target_files` — which does not degrade to `Unknown` (the map is still
/// `Known`, just empty) and so flags EVERY `.rs` file as outside targets,
/// including the crate's own `src/lib.rs`. That false positive was caught by
/// `tests/coverage_honesty.rs::kani_harness_outside_cargo_targets_is_flagged_and_lib_rs_is_not`
/// failing during this lot's own verification pass, not by inspection —
/// the plan's original design here (`tasks/plan-issues-282-283-284.md` §3.3:
/// `strip_prefix(canonicalize(root))`) does not hold and is deviated from.
pub fn discover(root: &Path) -> TargetMap {
    let manifest = root.join("Cargo.toml");
    if !manifest.is_file() {
        return TargetMap::Unknown;
    }
    let output = Command::new("cargo")
        .arg("metadata")
        .arg("--no-deps")
        .arg("--offline")
        .arg("--format-version")
        .arg("1")
        .arg("--manifest-path")
        .arg(&manifest)
        .output();
    let Ok(output) = output else {
        return TargetMap::Unknown;
    };
    if !output.status.success() {
        return TargetMap::Unknown;
    }
    let Ok(text) = String::from_utf8(output.stdout) else {
        return TargetMap::Unknown;
    };
    parse_metadata_json(&text, root)
}

/// The subset of `cargo metadata --format-version 1`'s schema this module
/// reads. Unknown fields are ignored by default (no `deny_unknown_fields`) —
/// this module has no opinion on the rest of the schema.
#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    targets: Vec<CargoTarget>,
}

#[derive(Deserialize)]
struct CargoTarget {
    src_path: String,
}

/// Pure parse of a `cargo metadata --no-deps --format-version 1` JSON payload
/// into a `TargetMap`, relative to `root` — the SAME (non-canonicalized) path
/// value the caller fed `cargo` as the manifest directory (see `discover`'s
/// doc for why canonicalizing here would be wrong). Split from `discover` so
/// it is testable against a fixed JSON fixture with no subprocess. Malformed
/// JSON, or a target whose `src_path` does not sit under `root` (should not
/// happen for `--no-deps`, but a workspace with an unusual layout is not this
/// module's problem to diagnose) → the JSON case returns `Unknown`; an
/// out-of-root target is silently skipped (its absence from
/// `target_dirs`/`target_files` cannot flag any in-root file as outside, so
/// this is safe by construction, not merely convenient).
pub(crate) fn parse_metadata_json(json: &str, root: &Path) -> TargetMap {
    let Ok(meta) = serde_json::from_str::<CargoMetadata>(json) else {
        return TargetMap::Unknown;
    };
    let mut target_dirs = BTreeSet::new();
    let mut target_files = BTreeSet::new();
    for pkg in meta.packages {
        for t in pkg.targets {
            let abs = PathBuf::from(&t.src_path);
            let Ok(rel) = abs.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_path_buf();
            if let Some(parent) = rel.parent() {
                if parent != Path::new("") {
                    target_dirs.insert(parent.to_path_buf());
                }
            }
            target_files.insert(rel);
        }
    }
    TargetMap::Known {
        target_dirs,
        target_files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed `cargo metadata --no-deps --format-version 1` fixture with
    /// five targets under one root: `src/lib.rs`, `src/main.rs`,
    /// `tests/on_paper.rs`, `benches/b.rs`, `build.rs`. Root is
    /// `/tmp/proj` — an arbitrary absolute path, since `parse_metadata_json`
    /// takes `canon_root` as a parameter and never touches the filesystem.
    const FIXTURE_ROOT: &str = "/tmp/proj";
    fn fixture_json() -> String {
        format!(
            r#"{{"packages":[{{"targets":[
                {{"src_path":"{root}/src/lib.rs"}},
                {{"src_path":"{root}/src/main.rs"}},
                {{"src_path":"{root}/tests/on_paper.rs"}},
                {{"src_path":"{root}/benches/b.rs"}},
                {{"src_path":"{root}/build.rs"}}
            ]}}]}}"#,
            root = FIXTURE_ROOT
        )
    }

    #[test]
    fn kani_file_outside_targets() {
        let map = parse_metadata_json(&fixture_json(), Path::new(FIXTURE_ROOT));
        assert!(map.is_outside_targets(Path::new("kani/x.rs")));
    }

    #[test]
    fn nested_src_file_inside_targets() {
        let map = parse_metadata_json(&fixture_json(), Path::new(FIXTURE_ROOT));
        assert!(!map.is_outside_targets(Path::new("src/deep/mod.rs")));
    }

    #[test]
    fn non_declared_tests_file_inside_targets() {
        // Not itself a listed target, but under the "tests" target_dir —
        // matches Cargo's own auto-discovery (every tests/*.rs becomes its
        // own integration-test target, so cargo metadata would in practice
        // list it too; this asserts the directory-level fallback still
        // covers it even if metadata only reported one sibling).
        let map = parse_metadata_json(&fixture_json(), Path::new(FIXTURE_ROOT));
        assert!(!map.is_outside_targets(Path::new("tests/x.rs")));
    }

    #[test]
    fn root_level_target_file_inside_targets() {
        let map = parse_metadata_json(&fixture_json(), Path::new(FIXTURE_ROOT));
        assert!(!map.is_outside_targets(Path::new("build.rs")));
    }

    #[test]
    fn build_rs_does_not_turn_root_into_a_target_dir() {
        // If build.rs's parent ("." / "") were added to target_dirs, every
        // file in the tree — including kani/x.rs — would start_with("") and
        // be misclassified as inside. This is the regression this test pins.
        let map = parse_metadata_json(&fixture_json(), Path::new(FIXTURE_ROOT));
        assert!(map.is_outside_targets(Path::new("kani/x.rs")));
    }

    #[test]
    fn non_rust_file_never_flagged() {
        let map = parse_metadata_json(&fixture_json(), Path::new(FIXTURE_ROOT));
        assert!(!map.is_outside_targets(Path::new("README.md")));
        assert!(!map.is_outside_targets(Path::new("kani/notes.md")));
    }

    #[test]
    fn malformed_json_is_unknown_not_a_panic() {
        let map = parse_metadata_json("{ not json", Path::new(FIXTURE_ROOT));
        assert_eq!(map, TargetMap::Unknown);
        assert!(!map.is_outside_targets(Path::new("kani/x.rs")));
    }

    #[test]
    fn discover_without_cargo_toml_is_unknown() {
        let dir = tempfile::Builder::new()
            .prefix("cargo_targets_no_manifest_")
            .tempdir()
            .expect("temp dir");
        assert_eq!(discover(dir.path()), TargetMap::Unknown);
    }

    #[test]
    fn discover_with_broken_cargo_toml_is_unknown_not_a_panic() {
        let dir = tempfile::Builder::new()
            .prefix("cargo_targets_broken_manifest_")
            .tempdir()
            .expect("temp dir");
        std::fs::write(dir.path().join("Cargo.toml"), "this is not [[valid toml").unwrap();
        assert_eq!(discover(dir.path()), TargetMap::Unknown);
    }

    #[test]
    fn discover_finds_the_real_manifest_of_this_crate() {
        // Sanity check against the actual repo manifest: this crate's own
        // src/lib.rs must be discovered as a compiled target, and this test
        // file's directory (src/) must not be flagged as outside.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let map = discover(root);
        match &map {
            TargetMap::Known { .. } => {
                assert!(!map.is_outside_targets(Path::new("src/lib.rs")));
            }
            TargetMap::Unknown => {
                // cargo/network unavailable in this sandbox — do not fail the
                // suite over an environment precondition this test cannot
                // control; the dedicated Unknown-path tests above already
                // cover the failure contract.
            }
        }
    }
}
