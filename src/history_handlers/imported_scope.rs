//! Drops the `scope` of a manifest an artifact unpacked beside its graph
//! (reviews of #478). The scope says "this graph was built from source with
//! these settings"; an imported graph was built by whoever exported it, so a
//! later `analyze_codebase` must not reuse it. The per-file states stay:
//! `index_codebase` still fills incrementally from them.

use crate::indexer;
use std::path::Path;

/// Runs after `artifact::import_artifact`, whatever it returned: a failed
/// unpack can leave the bundled manifest behind too. If the scope cannot be
/// dropped, the manifest is removed instead; if that fails as well, both
/// errors are returned, and an import error is never hidden by them.
pub(super) fn forget(graph_dir: &Path, imported: Result<impl Sized, String>) -> Result<(), String> {
    let Err(scope_err) = drop_scope(graph_dir) else {
        return imported.map(|_| ());
    };
    eprintln!("[ap] could not drop the imported scope ({scope_err}); removing the manifest");
    let removal = graph_dir
        .parent()
        .map(|out| std::fs::remove_file(indexer::manifest::manifest_path(out)))
        .unwrap_or(Ok(()));
    let mut err = scope_err;
    if let Err(rm) = removal {
        if rm.kind() != std::io::ErrorKind::NotFound {
            err = format!("{err}; and the manifest could not be removed: {rm}");
        }
    }
    match imported {
        Err(import_err) => Err(format!("{import_err} (and {err})")),
        Ok(_) => Err(err),
    }
}

fn drop_scope(graph_dir: &Path) -> Result<(), String> {
    let Some(output_dir) = graph_dir.parent() else {
        return Ok(());
    };
    let path = indexer::manifest::manifest_path(output_dir);
    match indexer::manifest::load(&path) {
        Some(mut m) if m.scope.is_some() => {
            m.scope = None;
            indexer::manifest::save(&path, &m)
        }
        _ => Ok(()),
    }
}
