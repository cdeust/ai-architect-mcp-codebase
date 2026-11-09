//! The check `analyze_codebase` makes before reusing a graph (issue #443):
//! did anything the graph was built from change? Split out of `mod.rs` for
//! the §4.1 file cap (review of #478).

use std::path::Path;

use super::super::manifest::FileManifest;
use super::super::walk::WalkOptions;
use super::super::{IndexOptions, MAX_TOTAL_BYTES};
use super::classify::{classify, discover};
use super::Discovered;

/// True when the tree under `codebase`, walked with `options`, differs from
/// `prior`: a file changed, added, deleted or renamed. Reads the tree and
/// writes nothing — no graph, no sidecar.
///
/// Exists for `analyze_codebase` (#478 review): the resolver keeps the edges
/// an earlier run wrote and has no invalidation for an unchanged file whose
/// candidate set another file's edit changed, so analyze reuses a graph only
/// when this returns `false`.
pub fn tree_changed_since(
    codebase: &Path,
    options: &IndexOptions,
    prior: &FileManifest,
) -> Result<bool, String> {
    let walk_opts = WalkOptions {
        language_filter: options.language_filter,
        dependency_scope: options.dependency_scope,
        exclude_dirs: options.exclude_dirs.clone(),
    };
    let (current, _walk_gaps, _walk_pruned) = discover(codebase, walk_opts)?;
    // Before `classify` hashes anything (Codex security review of #478): the
    // full index refuses a tree over MAX_TOTAL_BYTES, and the change check must
    // not read more than the rebuild it stands in for would.
    check_total_bytes(&current, MAX_TOTAL_BYTES)?;
    // A file the full index could not read is recorded with an empty hash, and
    // making it readable (`chmod +r`) moves no mtime: count it as a change, or
    // it would never be retried (review of #478).
    if prior.files.values().any(|f| f.content_hash.is_empty()) {
        return Ok(true);
    }
    let c = classify(prior, &current).change_set;
    Ok(!(c.changed.is_empty()
        && c.added.is_empty()
        && c.deleted.is_empty()
        && c.renamed.is_empty()))
}

/// `Err(total_bytes_exceeded…)` when the discovered files sum past `cap` —
/// the same refusal, and wording, as the full index's walk.
pub(super) fn check_total_bytes(current: &[Discovered], cap: u64) -> Result<(), String> {
    let total = current
        .iter()
        .fold(0u64, |acc, d| acc.saturating_add(d.size));
    if total > cap {
        return Err(format!(
            "total_bytes_exceeded: {total} bytes discovered (MAX_TOTAL_BYTES {cap})"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file(size: u64) -> Discovered {
        Discovered {
            rel: format!("f{size}"),
            abs: PathBuf::from(format!("/nonexistent/f{size}")),
            mtime_ns: 0,
            size,
        }
    }

    #[test]
    fn a_tree_over_the_cap_is_refused_before_any_file_is_read() {
        // The paths do not exist: a refusal that tried to read them would
        // surface an I/O error instead of the cap.
        let tree = [file(6), file(5)];
        let err = check_total_bytes(&tree, 10).expect_err("11 bytes > 10");
        assert!(err.starts_with("total_bytes_exceeded"), "{err}");
        assert!(
            check_total_bytes(&tree, 11).is_ok(),
            "the cap itself is allowed"
        );
    }
}
