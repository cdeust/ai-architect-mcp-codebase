use crate::parser::Language;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// DependencyScope — tri-tier dependency-ingestion contract
// ---------------------------------------------------------------------------

/// Tri-tier control over dependency-directory ingestion for
/// `index_codebase` / `analyze_codebase`. Replaces the old binary
/// `include_dependencies: bool` flag, which could not express the
/// "public API surface only" tier.
/// source: ADR-4253701 (analyze_codebase — contrat tri-tier DependencyScope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DependencyScope {
    /// Prune build/dependency directories entirely — the pre-existing
    /// `include_dependencies=false` behavior.
    #[default]
    None,
    /// Descend into dependency directories, but persist only publicly
    /// visible symbols (`visibility == public`, per-language convention)
    /// from files under them. Project files are unaffected: they are always
    /// indexed in full regardless of this tier.
    PublicApi,
    /// Descend into dependency directories and persist everything — the
    /// pre-existing `include_dependencies=true` behavior.
    Full,
}

impl DependencyScope {
    /// Parses the tri-tier string values accepted by the MCP contract.
    /// Returns `None` (the `Option`, not the variant) on an unrecognized value.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "public_api" => Some(Self::PublicApi),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// True for every tier that descends into dependency directories at all
    /// (`PublicApi` and `Full`); only `None` prunes them at the walk level.
    pub fn descends_into_dependencies(self) -> bool {
        !matches!(self, Self::None)
    }
}

// ---------------------------------------------------------------------------
// Directory walking
// ---------------------------------------------------------------------------

/// Options controlling a directory walk.
///
/// Bundled into one value so the recursive walker stays within the 4-parameter
/// limit (coding-standards §4.4) as new traversal knobs are added.
#[derive(Clone, Copy, Default)]
pub(super) struct WalkOptions {
    /// When `Some(L)`, only collect files of language `L`; `None` collects all.
    pub language_filter: Option<Language>,
    /// Tri-tier dependency-ingestion scope. `None` prunes build/dependency
    /// directories (node_modules, .venv, vendor, target, …); `PublicApi` and
    /// `Full` both descend into them (only `.git` is still skipped) — they
    /// differ at the persistence filter (`indexer::persist`), not the walk.
    pub dependency_scope: DependencyScope,
}

/// Recursively collects source files, skipping hidden dirs, target/, node_modules/.
/// When `opts.language_filter` is Some, only collects files for that language.
/// When None, collects all files with recognized extensions.
/// When `opts.dependency_scope` descends into dependencies (`PublicApi` or
/// `Full`), build/dependency dirs are also descended into (only `.git` is
/// skipped).
///
/// Symlinks are intentionally NOT followed — source: security hardening (C4).
/// This prevents a symlink inside the codebase from causing `read_dir` to
/// silently traverse outside the tree (e.g. to `/etc/passwd` or `~/.ssh`).
pub(super) fn collect_source_files(root: &Path, opts: WalkOptions) -> Result<Vec<PathBuf>, String> {
    let mut result = Vec::new();
    walk_dir_recursive(root, &mut result, opts, 0)?;
    if result.len() > super::MAX_FILES {
        return Err(format!(
            "too_many_files: codebase contains {} files, MAX_FILES is {}",
            result.len(),
            super::MAX_FILES
        ));
    }
    result.sort();
    Ok(result)
}

fn walk_dir_recursive(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    opts: WalkOptions,
    depth: usize,
) -> Result<(), String> {
    if depth > super::MAX_DEPTH {
        return Err(format!(
            "walk_too_deep: exceeded MAX_DEPTH ({}) at {}",
            super::MAX_DEPTH,
            dir.display()
        ));
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry: {e}"))?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if should_skip(&name_str, opts.dependency_scope) {
            continue;
        }
        // Use symlink_metadata (lstat) instead of metadata (stat) so symlinks
        // are detected and skipped rather than silently followed.
        // source: C4 fix — POSIX lstat(2), does not follow the final symlink.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue; // intentionally skip symlinks
        }
        if meta.is_dir() {
            walk_dir_recursive(&path, out, opts, depth + 1)?;
            if out.len() > super::MAX_FILES {
                return Err(format!(
                    "too_many_files: exceeded MAX_FILES ({}) during walk",
                    super::MAX_FILES
                ));
            }
        } else if meta.is_file() {
            if meta.len() > super::MAX_FILE_BYTES {
                eprintln!(
                    "indexer: skipping oversized file ({} bytes > MAX_FILE_BYTES {}): {}",
                    meta.len(),
                    super::MAX_FILE_BYTES,
                    path.display()
                );
                continue;
            }
            // File collection policy:
            //   * language_filter = Some(L): collect ONLY files of language L
            //     (a scoped re-index of a single language).
            //   * language_filter = None: ALL-FILE indexing — collect every
            //     file regardless of extension. Code files in a supported
            //     language get a full AST; every other file still becomes a
            //     File node (path/name/extension/size), and .js-family files
            //     are light-linked (import/require → Imports_File_File) in a
            //     post-pass. Oversized files are already skipped above and
            //     build/dependency dirs are pruned by should_skip.
            //     source: "the pipeline should index any kind of files" — so
            //     every file a session touches is navigable in the graph.
            match opts.language_filter {
                Some(filter) => {
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        if Language::from_extension(ext) == Some(filter) {
                            out.push(path);
                        }
                    }
                }
                None => out.push(path),
            }
        }
    }
    Ok(())
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
fn should_skip(name: &str, dependency_scope: DependencyScope) -> bool {
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
    if name == crate::artifact::ARTIFACT_DIR {
        return true;
    }
    // PublicApi/Full both descend into vendored/build/cache dirs so the graph
    // covers node_modules, .venv, vendor, target, etc. They differ at the
    // persistence filter (indexer::persist), not here.
    if dependency_scope.descends_into_dependencies() {
        return false;
    }
    name.starts_with('.')
        // Rust
        || name == "target"
        // JS / TS / Node
        || name == "node_modules"
        // Python
        || name == "__pycache__"
        || name == ".venv"
        || name == "venv"
        || name == ".pytest_cache"
        || name == ".mypy_cache"
        || name == ".tox"
        || name == ".eggs"
        // JVM / Android (Gradle / Maven / Eclipse / IntelliJ)
        || name == "build"
        || name == "out"
        || name == ".gradle"
        || name == ".idea"
        // Apple (Xcode / SPM / CocoaPods / Carthage)
        || name == "Pods"
        || name == "DerivedData"
        || name == ".build"
        || name == "Carthage"
        || name == ".swiftpm"
        // Go
        || name == "vendor"
        // Elixir / Mix and Erlang / rebar3 — `deps` is the standard fetched-
        // dependency directory for both build tools, and is also used in the
        // wild as an ad hoc vendored-packages dir. source: measured
        // 2026-08-06 — indexing the Cortex repo without this entry walked
        // into its gitignored deps/ (1.1 GB vendored Python site-packages,
        // including numpy C headers), flooding the log with duplicate-id
        // warnings and timing out the Cortex->AP MCP client.
        || name == "deps"
        // General build output
        || name == "dist"
        || name == "bin"
        || name == "obj"
        // Test / coverage
        || name == "coverage"
        || name == ".nyc_output"
    // Other VCS dirs are filtered by ``starts_with('.')``; ``.git`` itself is
    // handled explicitly above so it is excluded in full-dependency mode too.
}

/// True when `file_path` lives under a directory that `should_skip` would
/// prune in `DependencyScope::None` mode — i.e. it is a vendored/build
/// dependency file, not a project file. Pure function of the path; reuses
/// `should_skip` as the single source of truth for the dependency-directory
/// name list instead of duplicating it.
///
/// Used by the indexer to scope the `PublicApi` visibility filter to
/// dependency-tree symbols only: project files stay fully indexed regardless
/// of `dependency_scope`.
pub(super) fn is_dependency_path(root: &Path, file_path: &Path) -> bool {
    let rel = file_path.strip_prefix(root).unwrap_or(file_path);
    rel.parent()
        .into_iter()
        .flat_map(|p| p.components())
        .any(|c| should_skip(&c.as_os_str().to_string_lossy(), DependencyScope::None))
}
