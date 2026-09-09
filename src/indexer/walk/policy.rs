// indexer::walk::policy — which directories the walk refuses to enter, and why.
//
// Split out of `walk.rs` to keep it under the coding-standards section 4.1
// cap. This half is pure policy: a name list, the predicate that reads it, and
// the reason string that names the rule for the coverage report. It performs
// no IO and holds no walk state.
// source: ADR-9841.

use std::path::Path;

use super::super::DependencyScope;

/// Why the built-in policy pruned a directory. One of a closed set, so a
/// reader can tell a declared policy apart from a gap. source: ADR-9841.
pub(super) fn prune_reason(name: &str, is_dir: bool) -> String {
    if name == ".git" {
        return "vcs".to_string();
    }
    if name == crate::artifact::ARTIFACT_DIR || name == crate::artifact::LEGACY_ARTIFACT_DIR {
        return "tool_artifact".to_string();
    }
    let _ = is_dir;
    "dependency_or_build_dir".to_string()
}

/// Returns true for directories that should be skipped during walk.
///
/// Covers build / dependency / cache directories across the languages
/// the indexer supports. source: empirical — without ``build`` and
/// ``Pods`` excluded, an Android repo's ``app/build/intermediates/``
/// alone produces tens of thousands of stat() calls and many hundred
/// MB of *.dex / *.aar / *.jar files that the indexer rejects per-file
/// after walking into them. Filtering at the directory level avoids
/// the descent entirely.
pub(super) fn should_skip(name: &str, dependency_scope: DependencyScope) -> bool {
    // `.git` is never source — its object store is large and binary — so it is
    // skipped even in full-dependency mode. source: checkpoint 2026-07-04.
    if name == ".git" {
        return true;
    }
    // The tool's own artifact directory (issue #55 committed graph snapshot +
    // sidecar) is generated data, never source. Skipping it in EVERY dependency
    // scope keeps a full index and an artifact-bootstrap fill identical: the
    // committed `graph.zst`/`file_manifest.json` never become File nodes, so the
    // artifact's presence in the tree can't perturb graph parity (issue #62/#55).
    // The pre-rename directory name is skipped too (issue #195) — a repo that
    // has not yet had its artifact touched (so `artifact::migrate_legacy_dir`
    // has not fired) must not have its stale snapshot walked as source.
    if name == crate::artifact::ARTIFACT_DIR || name == crate::artifact::LEGACY_ARTIFACT_DIR {
        return true;
    }
    // PublicApi/Full both descend into vendored/build/cache dirs so the graph
    // covers node_modules, .venv, vendor, target, etc. They differ at the
    // persistence filter (indexer::persist), not here.
    if dependency_scope.descends_into_dependencies() {
        return false;
    }
    // No blanket dot rule. `.github` holds the CI that decides what merges and
    // `.claude` holds the hooks and agents that run on this repo; excluding
    // them by name shape meant the graph could not answer what breaks when
    // either changes. Machine state that happens to start with a dot is named
    // in DEPENDENCY_DIR_NAMES instead. Measured on this repository: the rule
    // hid 238 files under `.claude` and 15 under `.github`.
    // source: ADR-9841.
    DEPENDENCY_DIR_NAMES.contains(&name)
}

/// Build-output / fetched-dependency / cache directory names pruned by the
/// default walk (`DependencyScope::None`); `PublicApi`/`Full` descend into
/// them instead. Dot-prefixed entries are redundant with `should_skip`'s
/// ``starts_with('.')`` filter and kept for documentation completeness.
pub(super) const DEPENDENCY_DIR_NAMES: &[&str] = &[
    // Rust
    "target",
    // JS / TS / Node
    "node_modules",
    // Python
    "__pycache__",
    ".venv",
    "venv",
    ".pytest_cache",
    ".mypy_cache",
    ".tox",
    ".eggs",
    // JVM / Android (Gradle / Maven / Eclipse / IntelliJ)
    "build",
    "out",
    ".gradle",
    ".idea",
    // Apple (Xcode / SPM / CocoaPods / Carthage)
    "Pods",
    "DerivedData",
    ".build",
    "Carthage",
    ".swiftpm",
    // Go
    "vendor",
    // Elixir / Mix and Erlang / rebar3 — `deps` is the standard fetched-
    // dependency directory for both build tools, and is also used in the
    // wild as an ad hoc vendored-packages dir. source: measured
    // 2026-08-06 — indexing the Cortex repo without this entry walked
    // into its gitignored deps/ (1.1 GB vendored Python site-packages,
    // including numpy C headers), flooding the log with duplicate-id
    // warnings and timing out the Cortex->AP MCP client.
    "deps",
    // General build output. `bin` is NOT here: it holds first-class project
    // scripts as often as it holds artifacts (this repository's own
    // `bin/ensure-binary.sh` pins the release SHA-256), and a compiled binary
    // has no recognised source extension, so the file-level filter already
    // rejects it. Pruning the directory by name only hid the scripts.
    // source: ADR-9841, measured 2026-09-09.
    "dist",
    "obj",
    // Test / coverage
    "coverage",
    ".nyc_output",
    // Other VCS stores. These used to be caught by a blanket
    // `name.starts_with('.')` rule that also swallowed `.github` and
    // `.claude`, so they are named explicitly now. source: ADR-9841.
    ".hg",
    ".svn",
    ".bzr",
    // JS / infra tool caches, same reason.
    ".cache",
    ".next",
    ".nuxt",
    ".turbo",
    ".parcel-cache",
    ".terraform",
    ".serverless",
    ".dart_tool",
    ".stack-work",
];

/// True when `file_path` lives under a directory that `should_skip` would
/// prune in `DependencyScope::None` mode — i.e. it is a vendored/build
/// dependency file, not a project file. Pure function of the path; reuses
/// `should_skip` as the single source of truth for the dependency-directory
/// name list instead of duplicating it.
///
/// Used by the indexer to scope the `PublicApi` visibility filter to
/// dependency-tree symbols only: project files stay fully indexed regardless
/// of `dependency_scope`. Note: a user-excluded directory (issue #249) never
/// reaches this function at all — it is pruned at the walk, so no File node
/// (and therefore no call to `is_dependency_path`) is ever produced for it.
pub(in crate::indexer) fn is_dependency_path(root: &Path, file_path: &Path) -> bool {
    let rel = file_path.strip_prefix(root).unwrap_or(file_path);
    rel.parent()
        .into_iter()
        .flat_map(|p| p.components())
        .any(|c| should_skip(&c.as_os_str().to_string_lossy(), DependencyScope::None))
}
