// bounded — the incremental refresh with a ceiling on the share of the tree
// it re-parses or purges (issue #443). `analyze_codebase` rebuilds past that
// share, where the refresh measured slower than a rebuild (see
// `analyze_handlers::index_phase::MAX_TOUCHED_PCT`).

use super::{refresh, FileManifest, IncrementalResult, IndexOptions};
use std::path::Path;

/// What a bounded refresh did (issue #443).
pub enum Refresh {
    Applied(Box<IncrementalResult>),
    /// More than the bound's share of the tree changed: the pass stopped after
    /// classifying, before it touched a file's rows, and the caller rebuilds.
    Declined {
        touched: u64,
        files: u64,
    },
}

/// `index_incremental` that declines when the files to re-parse or purge
/// exceed `max_touched_pct` percent of the current tree; the manifest, the
/// coverage sidecar and every file's rows are then left as they were. The
/// schema is not: `ensure_cfg_active_columns` and `ensure_rel_tables` run
/// before the change set is classified, so a graph written before a column or
/// a relationship table existed gains it, empty, even when the pass declines.
pub fn index_incremental_bounded(
    codebase: &Path,
    graph_dir: &Path,
    manifest_path: &Path,
    options: &IndexOptions,
    prior: &FileManifest,
    max_touched_pct: u64,
) -> Result<Refresh, String> {
    refresh(
        codebase,
        graph_dir,
        manifest_path,
        options,
        prior,
        Some(max_touched_pct),
    )
}
