// Light cross-file linking for files the AST parsers don't cover.
//
// Under all-file indexing every file becomes a File node, but only files in a
// supported language get a parsed AST (and therefore real Calls/Imports edges).
// JavaScript (.js/.jsx/.mjs/.cjs) has no tree-sitter parser wired in, yet its
// import/require graph is exactly the "what depends on what" signal the
// visualization needs. This module recovers that graph cheaply: a regex-free
// scan for relative `import ... from "X"`, `require("X")` and dynamic
// `import("X")`, resolved against the set of indexed File ids and emitted as
// `Imports_File_File` edges.
//
// Markdown and shell files get the same treatment but are emitted as
// `References_File_File` — a file→file reference, not a code dependency (issue
// #205). Without this, fan-in queries are blind to the two kinds of file most
// doc/script-heavy repos actually hub around (a rules doc every agent cites, a
// tool script every workflow sources) and undershoot their true fan-in by an
// order of magnitude. Extraction is split by source language into sibling
// modules (`js`, `markdown`, `shell`); this file only orchestrates: pick the
// extractor for a file's extension, resolve each specifier, stage the edge.
//
// Runs as a POST-PASS after the main index loop, so every File node already
// exists — no forward-reference problem (the AST resolver defers cross-file
// edges for the same reason). Best-effort: an unresolved specifier is skipped,
// never an error. source: all-file indexing follow-through.

mod js;
mod markdown;
mod shell;

use crate::graph_store::{GraphStore, PropEdgeList};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// JS-family extensions that have no AST parser but a meaningful import graph.
const JS_EXTS: &[&str] = &["js", "jsx", "mjs", "cjs"];

/// Markdown extensions — docs that reference other files via `[text](path)`,
/// backtick-quoted paths, or bare relative-path mentions.
const MD_EXTS: &[&str] = &["md", "markdown", "mdx"];

/// Shell extensions — scripts that reference other files via `source`/`.`,
/// direct relative invocation, or a `$VAR/`-prefixed path.
const SH_EXTS: &[&str] = &["sh", "bash"];

/// Candidate suffixes tried when resolving a bare relative specifier to a
/// concrete indexed File id (Node-style resolution, minus package lookup).
const JS_RESOLVE_SUFFIXES: &[&str] =
    &["", ".js", ".jsx", ".mjs", ".cjs", "/index.js", "/index.mjs"];

/// One extracted reference specifier plus the extraction method that found
/// it, carried through to the edge's `resolution_method` property so a reader
/// can see exactly which heuristic produced a given edge (issue #205
/// provenance requirement).
struct Specifier {
    text: String,
    method: &'static str,
}

/// Emits `Imports_File_File`/`References_File_File` edges for relative
/// imports and doc/script references. Returns the number of edges created.
/// Never fails the index — resolution misses and per-file read errors are
/// silently skipped.
///
/// Full-index convenience: scan every file and resolve against that same set.
pub(super) fn link_loose_file_imports(
    store: &GraphStore,
    root: &Path,
    files: &[PathBuf],
) -> Result<u64, String> {
    link_file_imports_for(store, root, files, files)
}

/// Incremental-friendly core: scan only `sources` for import/reference
/// specifiers, but resolve each specifier against the id set of `all_files`
/// (the full current file list). This is the split the incremental pass needs —
/// after a partial re-parse it must re-derive the light links *out of* the
/// changed/added files while still resolving them against every File node that
/// exists in the graph, not just the handful that were re-parsed.
///
/// Preconditions: every File node for `all_files` already exists in `store`
/// (this runs as a post-pass, like the full-index caller). Postconditions on
/// `Ok(n)`: `n` `Imports_File_File`/`References_File_File` edges whose source is
/// in `sources` and whose resolved target is in `all_files` have been inserted;
/// per-file read errors and unresolved specifiers are skipped, never fatal.
pub(super) fn link_file_imports_for(
    store: &GraphStore,
    root: &Path,
    sources: &[PathBuf],
    all_files: &[PathBuf],
) -> Result<u64, String> {
    let file_ids: HashSet<String> = all_files.iter().map(|f| rel_id(root, f)).collect();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    // Staged per rel_table and bulk-inserted once at the end instead of one
    // insert_edge round-trip per specifier. source: ADR-4253701 §Decision 2
    // (levier 2, formerly light_link.rs:93, logic unchanged by the #205 split).
    let mut edges_by_table: HashMap<&'static str, PropEdgeList> = HashMap::new();

    for file_path in sources {
        let Some((rel_table, specs)) = specs_for_file(file_path) else {
            continue;
        };
        let from_id = rel_id(root, file_path);
        stage_edges(
            &from_id,
            &specs,
            &file_ids,
            rel_table,
            &mut seen,
            &mut edges_by_table,
        );
    }

    let mut edge_count: u64 = 0;
    for (rel_table, edges) in &edges_by_table {
        edge_count += store.bulk_insert_edges(rel_table, edges)?;
    }
    Ok(edge_count)
}

/// Reads `file_path` and extracts its reference/import specifiers, tagged with
/// the target relation table for its language:
///   JS family   → import/require              → Imports_File_File   (code dep)
///   Markdown    → link / backtick / bare path  → References_File_File (doc link)
///   Shell       → source / invoke / $VAR path  → References_File_File (script link)
/// Returns `None` for unsupported extensions or unreadable/oversize files —
/// never an error, this is a best-effort post-pass.
fn specs_for_file(file_path: &Path) -> Option<(&'static str, Vec<Specifier>)> {
    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if JS_EXTS.contains(&ext.as_str()) {
        let src = read_text(file_path)?;
        let specs = js::extract_relative_specifiers(&src)
            .into_iter()
            .map(|text| Specifier {
                text,
                method: "js-regex",
            })
            .collect();
        Some(("Imports_File_File", specs))
    } else if MD_EXTS.contains(&ext.as_str()) {
        let src = read_text(file_path)?;
        Some((
            "References_File_File",
            markdown::extract_markdown_specifiers(&src),
        ))
    } else if SH_EXTS.contains(&ext.as_str()) {
        let src = read_text(file_path)?;
        Some((
            "References_File_File",
            shell::extract_shell_specifiers(&src),
        ))
    } else {
        None
    }
}

/// Resolves and stages every specifier of one source file into
/// `edges_by_table`, deduping by `(from, target)` via `seen`. Resolution
/// strategy is per extraction method:
///   js-regex       → Node-style suffix resolution (existing contract)
///   sh-var-prefix  → the `$VAR/`-stripped remainder must resolve to a UNIQUE
///                    existing File id — ambiguous or absent means skip,
///                    never guess (issue #205 spec)
///   everything else (md-link/md-backtick/md-bare/sh-source/sh-invoke)
///                  → exact match only, no suffix guessing, per the "no fuzzy
///                    matching" constraint on the new reference kinds.
/// Never fails: an unresolved specifier is skipped, not an error.
fn stage_edges(
    from_id: &str,
    specs: &[Specifier],
    file_ids: &HashSet<String>,
    rel_table: &'static str,
    seen: &mut HashSet<(String, String)>,
    edges_by_table: &mut HashMap<&'static str, PropEdgeList>,
) {
    for spec in specs {
        let target = match spec.method {
            "js-regex" => js::resolve_specifier(from_id, &spec.text, file_ids),
            "sh-var-prefix" => shell::resolve_var_prefixed(&spec.text, file_ids),
            _ => resolve_exact(from_id, &spec.text, file_ids),
        };
        let Some(target) = target else { continue };
        if target == from_id {
            continue;
        }
        if !seen.insert((from_id.to_string(), target.clone())) {
            continue;
        }
        // confidence 1.0: every edge here is emitted ONLY after its specifier
        // resolved to an exact, already-indexed File id (see `resolve_exact`
        // / the JS suffix table) — there is no fuzziness left to discount
        // for, unlike the AST resolver's heuristic name/scope lookups (which
        // stay < 1.0). source: coding-standards.md §8 — an exact-match edge
        // earns 1.0, not an invented discount.
        let props = vec![
            ("confidence".to_string(), "1.0".to_string()),
            (
                "resolution_method".to_string(),
                format!("'{}'", spec.method),
            ),
        ];
        edges_by_table
            .entry(rel_table)
            .or_default()
            .push((from_id.to_string(), target, props));
    }
}

/// Reads a file as UTF-8 text within the parse cap; None on binary / oversize.
fn read_text(path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    if s.len() as u64 > super::MAX_PARSE_BYTES {
        return None;
    }
    Some(s)
}

/// Repo-relative, forward-slash File id for a path under `root`.
pub(super) fn rel_id(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Resolves `spec` against the set of indexed File ids with NO suffix
/// guessing: it must equal an existing File id exactly, either relative to
/// the referrer's directory or as a repo-root path. Used for the markdown and
/// shell reference kinds (issue #205), which carry no language-resolution
/// contract to justify JS-style suffix fallback — an inexact match here would
/// be a fabricated edge, forbidden by coding-standards.md §8.
fn resolve_exact(from_id: &str, spec: &str, file_ids: &HashSet<String>) -> Option<String> {
    let from_dir = Path::new(from_id).parent().unwrap_or_else(|| Path::new(""));
    for candidate_path in [normalize(&from_dir.join(spec)), normalize(Path::new(spec))] {
        let candidate = candidate_path.to_string_lossy().replace('\\', "/");
        if !candidate.is_empty() && file_ids.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Lexically normalizes a path: resolves `.` and `..` components without
/// touching the filesystem (the targets live in the indexed-id set, not on
/// disk relative to cwd).
fn normalize(p: &Path) -> PathBuf {
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s.to_os_string()),
            _ => {}
        }
    }
    let mut pb = PathBuf::new();
    for s in out {
        pb.push(s);
    }
    pb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_exact_rejects_nonexistent_paths_no_fuzzy_matching() {
        let ids: HashSet<String> = ["rules/coding-standards.md", "tools/memory-tool.sh"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            resolve_exact("README.md", "rules/coding-standards.md", &ids),
            Some("rules/coding-standards.md".to_string())
        );
        // No suffix guessing: an inexact/near-miss path must not resolve.
        assert_eq!(
            resolve_exact("README.md", "rules/coding-standard", &ids),
            None
        );
        assert_eq!(
            resolve_exact("README.md", "rules/coding-standards", &ids),
            None
        );
    }
}
