use crate::parser::Language;
use std::collections::HashSet;
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
// ExcludeSet — issue #249 user-configurable directory exclusion
// ---------------------------------------------------------------------------

/// The parsed `exclude_dirs` contract (issue #249). An entry WITHOUT a path
/// separator is a bare directory NAME, matched against any directory anywhere
/// in the tree — exactly like the built-in skip list (`should_skip`). An
/// entry WITH a separator is a path relative to the walk root, matched
/// against exactly one subtree. No glob support (explicitly deferred by the
/// issue). Validation (rejecting absolute paths / `..` components) happens at
/// the MCP argument boundary (`handler_util::parse_exclude_dirs`); this type
/// only normalizes and matches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExcludeSet {
    /// Bare directory names, matched against any path component's name.
    names: HashSet<String>,
    /// Relative paths (forward-slash, no leading/trailing slash), matched
    /// against the walk-root-relative path of the candidate directory.
    paths: HashSet<String>,
}

impl ExcludeSet {
    /// Builds an `ExcludeSet` from the raw `exclude_dirs` argument strings.
    /// An entry containing `/` or `\` is treated as a relative path and
    /// canonicalized to the walk's own rel-path formatting (`dir_rel`:
    /// forward slashes, no leading `./`, no doubled or trailing separators —
    /// the `\`->`/` convention already used by `light_link::rel_id`), so
    /// every syntactically-equivalent spelling of the same subtree
    /// (`config/secrets`, `./config/secrets`, `config//secrets/`) compares
    /// equal. Without that canonicalization a `./`-prefixed entry passed
    /// boundary validation but never matched, silently excluding nothing —
    /// PR #250 review, BLOCK finding. An entry with no separator is a bare
    /// name. Blank entries are ignored. `..` never reaches this type: it is
    /// rejected at the MCP boundary (`handler_util::parse_exclude_dirs`).
    pub fn new(entries: &[String]) -> Self {
        let mut names = HashSet::new();
        let mut paths = HashSet::new();
        for raw in entries {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !trimmed.contains('/') && !trimmed.contains('\\') {
                names.insert(trimmed.to_string());
                continue;
            }
            let normalized = trimmed.replace('\\', "/");
            let parts: Vec<&str> = Path::new(&normalized)
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(p) => p.to_str(),
                    _ => None,
                })
                .collect();
            if !parts.is_empty() {
                // A separator-carrying entry stays a PATH entry even when it
                // canonicalizes to one component ("./secrets", "secrets/"):
                // per the contract it pins exactly the root-level subtree,
                // unlike a bare name which matches anywhere.
                paths.insert(parts.join("/"));
            }
        }
        ExcludeSet { names, paths }
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.paths.is_empty()
    }

    /// True when the bare directory `name` or its walk-root-relative path
    /// `rel` (forward-slash) matches an excluded entry.
    fn matches(&self, name: &str, rel: &str) -> bool {
        self.names.contains(name) || self.paths.contains(rel)
    }
}

// ---------------------------------------------------------------------------
// Directory walking
// ---------------------------------------------------------------------------

/// Options controlling a directory walk.
///
/// Bundled into one value so the recursive walker stays within the 4-parameter
/// limit (coding-standards §4.4) as new traversal knobs are added. Not `Copy`
/// (issue #249's `ExcludeSet` owns growable collections) — callers pass it by
/// value once per walk; the recursive walker itself borrows it (`&WalkOptions`).
#[derive(Clone, Default)]
pub(super) struct WalkOptions {
    /// When `Some(L)`, only collect files of language `L`; `None` collects all.
    pub language_filter: Option<Language>,
    /// Tri-tier dependency-ingestion scope. `None` prunes build/dependency
    /// directories (node_modules, .venv, vendor, target, …); `PublicApi` and
    /// `Full` both descend into them (only `.git` is still skipped) — they
    /// differ at the persistence filter (`indexer::persist`), not the walk.
    pub dependency_scope: DependencyScope,
    /// User-specified directories to prune (issue #249), independent of and
    /// applied BEFORE `dependency_scope`: exclusion wins over every tier,
    /// including `Full`.
    pub exclude_dirs: ExcludeSet,
}

/// The result of a directory walk: the collected source files, plus every
/// directory pruned along the way (issue #249) — never silently dropped, so
/// the caller can fold them into the coverage-honesty report.
pub(super) struct WalkOutcome {
    pub files: Vec<PathBuf>,
    /// Walk-root-relative (forward-slash) path of each directory pruned by an
    /// explicit `exclude_dirs` match.
    pub excluded_dirs: Vec<String>,
    /// Walk-root-relative (forward-slash) path of each directory skipped
    /// because `read_dir` returned `PermissionDenied` — the walk continues
    /// past it instead of aborting (issue #249).
    pub unreadable_dirs: Vec<String>,
}

/// Recursively collects source files, skipping hidden dirs, target/, node_modules/.
/// When `opts.language_filter` is Some, only collects files for that language.
/// When None, collects all files with recognized extensions.
/// When `opts.dependency_scope` descends into dependencies (`PublicApi` or
/// `Full`), build/dependency dirs are also descended into (only `.git` is
/// skipped) — UNLESS `opts.exclude_dirs` prunes them explicitly, which wins
/// over every tier.
///
/// Symlinks are intentionally NOT followed — source: security hardening (C4).
/// This prevents a symlink inside the codebase from causing `read_dir` to
/// silently traverse outside the tree (e.g. to `/etc/passwd` or `~/.ssh`).
///
/// A `PermissionDenied` directory (other than the walk root itself) is
/// recorded and skipped rather than aborting the whole walk (issue #249); the
/// walk root failing to read is still fatal — there is nothing to index.
pub(super) fn collect_source_files(root: &Path, opts: WalkOptions) -> Result<WalkOutcome, String> {
    let mut files = Vec::new();
    let mut excluded_dirs = Vec::new();
    let mut unreadable_dirs = Vec::new();
    let ctx = WalkContext { root, opts: &opts };
    let mut collectors = WalkCollectors {
        files: &mut files,
        excluded_dirs: &mut excluded_dirs,
        unreadable_dirs: &mut unreadable_dirs,
    };
    walk_dir_recursive(root, &ctx, &mut collectors, 0)?;
    if files.len() > super::MAX_FILES {
        return Err(format!(
            "too_many_files: codebase contains {} files, MAX_FILES is {}",
            files.len(),
            super::MAX_FILES
        ));
    }
    files.sort();
    Ok(WalkOutcome {
        files,
        excluded_dirs,
        unreadable_dirs,
    })
}

/// Immutable per-walk context: the walk root (needed to compute rel paths for
/// exclusion matching and reporting) and the walk options.
struct WalkContext<'a> {
    root: &'a Path,
    opts: &'a WalkOptions,
}

/// Mutable accumulators threaded through the recursive walk. Bundled so
/// `walk_dir_recursive` stays within the 4-parameter limit (coding-standards
/// §4.4) despite tracking three distinct outputs (files, excluded dirs,
/// unreadable dirs).
struct WalkCollectors<'a> {
    files: &'a mut Vec<PathBuf>,
    excluded_dirs: &'a mut Vec<String>,
    unreadable_dirs: &'a mut Vec<String>,
}

fn walk_dir_recursive(
    dir: &Path,
    ctx: &WalkContext,
    collectors: &mut WalkCollectors,
    depth: usize,
) -> Result<(), String> {
    if depth > super::MAX_DEPTH {
        return Err(format!(
            "walk_too_deep: exceeded MAX_DEPTH ({}) at {}",
            super::MAX_DEPTH,
            dir.display()
        ));
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // issue #249: a locked-down directory must not abort the whole walk.
        // The walk root itself (depth 0) failing to read stays fatal — there
        // is nothing to index at all, which is a different failure than "one
        // subtree is unreadable".
        Err(e) if depth > 0 && e.kind() == std::io::ErrorKind::PermissionDenied => {
            collectors.unreadable_dirs.push(dir_rel(ctx.root, dir));
            return Ok(());
        }
        Err(e) => return Err(format!("read_dir {}: {e}", dir.display())),
    };
    for entry in entries {
        // Entry-level iteration errors stay fatal DELIBERATELY (narrower than
        // the read_dir degrade above): a directory that refuses to open is a
        // permissions *configuration* (issue #249's target); an error midway
        // through a listing that already opened is unexpected filesystem
        // state (racing mutation, I/O fault) — degrading silently there
        // would hide real corruption. PR #250 review, scope decision.
        let entry = entry.map_err(|e| format!("dir entry: {e}"))?;
        visit_entry(entry, ctx, collectors, depth)?;
    }
    Ok(())
}

/// Classifies and dispatches one directory entry: skip (built-in list or
/// symlink), or hand off to the directory/file visitor. Split out of
/// `walk_dir_recursive` (coding-standards §4.2) so each step stays a short,
/// independently-reasoned unit.
fn visit_entry(
    entry: std::fs::DirEntry,
    ctx: &WalkContext,
    collectors: &mut WalkCollectors,
    depth: usize,
) -> Result<(), String> {
    let path = entry.path();
    let name = entry.file_name();
    let name_str = name.to_string_lossy();
    if should_skip(&name_str, ctx.opts.dependency_scope) {
        return Ok(());
    }
    // Use symlink_metadata (lstat) instead of metadata (stat) so symlinks are
    // detected and skipped rather than silently followed.
    // source: C4 fix — POSIX lstat(2), does not follow the final symlink.
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    if meta.file_type().is_symlink() {
        return Ok(()); // intentionally skip symlinks
    }
    if meta.is_dir() {
        let candidate = DirCandidate {
            path,
            name: name_str.into_owned(),
        };
        visit_dir_entry(candidate, ctx, collectors, depth)
    } else if meta.is_file() {
        visit_file_entry(path, &meta, ctx, collectors);
        Ok(())
    } else {
        Ok(())
    }
}

/// A directory entry pending the exclude-check + recurse step. Bundles
/// `path` and its bare `name` so `visit_dir_entry` stays within the
/// 4-parameter limit (coding-standards §4.4).
struct DirCandidate {
    path: PathBuf,
    name: String,
}

/// Applies `exclude_dirs` (issue #249, wins over every `dependency_scope`
/// tier) then recurses, bounding the collected-file count as it returns.
fn visit_dir_entry(
    candidate: DirCandidate,
    ctx: &WalkContext,
    collectors: &mut WalkCollectors,
    depth: usize,
) -> Result<(), String> {
    let DirCandidate { path, name } = candidate;
    if !ctx.opts.exclude_dirs.is_empty() {
        let rel = dir_rel(ctx.root, &path);
        if ctx.opts.exclude_dirs.matches(&name, &rel) {
            collectors.excluded_dirs.push(rel);
            return Ok(());
        }
    }
    walk_dir_recursive(&path, ctx, collectors, depth + 1)?;
    if collectors.files.len() > super::MAX_FILES {
        return Err(format!(
            "too_many_files: exceeded MAX_FILES ({}) during walk",
            super::MAX_FILES
        ));
    }
    Ok(())
}

/// File collection policy:
///   * `language_filter = Some(L)`: collect ONLY files of language L (a
///     scoped re-index of a single language).
///   * `language_filter = None`: ALL-FILE indexing — collect every file
///     regardless of extension. Code files in a supported language get a
///     full AST; every other file still becomes a File node
///     (path/name/extension/size), and .js-family files are light-linked
///     (import/require → Imports_File_File) in a post-pass. Oversized files
///     are skipped here; build/dependency dirs are pruned by `should_skip`
///     before this function is ever reached.
///     source: "the pipeline should index any kind of files" — so every
///     file a session touches is navigable in the graph.
fn visit_file_entry(
    path: PathBuf,
    meta: &std::fs::Metadata,
    ctx: &WalkContext,
    collectors: &mut WalkCollectors,
) {
    if meta.len() > super::MAX_FILE_BYTES {
        eprintln!(
            "indexer: skipping oversized file ({} bytes > MAX_FILE_BYTES {}): {}",
            meta.len(),
            super::MAX_FILE_BYTES,
            path.display()
        );
        return;
    }
    match ctx.opts.language_filter {
        Some(filter) => {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if Language::from_extension(ext) == Some(filter) {
                    collectors.files.push(path);
                }
            }
        }
        None => collectors.files.push(path),
    }
}

/// The walk-root-relative, forward-slash path of `path` — shared by the
/// `exclude_dirs` relative-path match and the coverage-report rel key.
/// Mirrors `light_link::rel_id`'s `\`->`/` normalization.
fn dir_rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
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
    // Other VCS dirs are filtered by ``starts_with('.')``; ``.git`` itself is
    // handled explicitly above so it is excluded in full-dependency mode too.
    name.starts_with('.') || DEPENDENCY_DIR_NAMES.contains(&name)
}

/// Build-output / fetched-dependency / cache directory names pruned by the
/// default walk (`DependencyScope::None`); `PublicApi`/`Full` descend into
/// them instead. Dot-prefixed entries are redundant with `should_skip`'s
/// ``starts_with('.')`` filter and kept for documentation completeness.
const DEPENDENCY_DIR_NAMES: &[&str] = &[
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
    // General build output
    "dist",
    "bin",
    "obj",
    // Test / coverage
    "coverage",
    ".nyc_output",
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
pub(super) fn is_dependency_path(root: &Path, file_path: &Path) -> bool {
    let rel = file_path.strip_prefix(root).unwrap_or(file_path);
    rel.parent()
        .into_iter()
        .flat_map(|p| p.components())
        .any(|c| should_skip(&c.as_os_str().to_string_lossy(), DependencyScope::None))
}

#[cfg(test)]
mod tests;
