//! Drops the `scope` of a manifest an artifact unpacked beside its graph
//! (reviews of #478). The scope says "this graph was built from source with
//! these settings"; an imported graph was built by whoever exported it, so a
//! later `analyze_codebase` must not reuse it. The per-file states stay:
//! `index_codebase` still fills incrementally from them. A manifest that is
//! not a regular file that loads (a symlink, dangling or not, or one that does
//! not parse) is removed instead, since it could later read as a scoped one.

use crate::indexer;
use std::path::Path;

/// Runs after `artifact::import_artifact`, whatever it returned: a failed
/// unpack can leave the bundled manifest behind too. If the scope cannot be
/// dropped, the manifest is removed instead; if that fails as well, both
/// errors are returned, and an import error is never hidden by them.
pub(super) fn forget(graph_dir: &Path, imported: Result<(), String>) -> Result<(), String> {
    let Err(scope_err) = drop_scope(graph_dir) else {
        return imported;
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
        Ok(()) => Err(err),
    }
}

fn drop_scope(graph_dir: &Path) -> Result<(), String> {
    let Some(output_dir) = graph_dir.parent() else {
        return Ok(());
    };
    let path = indexer::manifest::manifest_path(output_dir);
    // Only a regular file that loads is kept (Claude review of #478): a
    // symlink, even a dangling one, or a file that does not parse could later
    // read as a manifest with a scope that was never dropped. A non-regular
    // entry is unlinked before anything reads it: `load` follows links, and a
    // link to `/dev/zero` would be read without end (Codex security review).
    let regular = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta.file_type().is_file(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("stat {}: {e}", path.display())),
    };
    let remove = || {
        std::fs::remove_file(&path).map_err(|e| format!("remove unusable {}: {e}", path.display()))
    };
    if !regular {
        return remove();
    }
    match indexer::manifest::load(&path) {
        Some(m) if m.scope.is_none() => Ok(()),
        Some(mut m) => {
            m.scope = None;
            indexer::manifest::save(&path, &m)
        }
        None => remove(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn layout() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph = dir.path().join("graph");
        let manifest = indexer::manifest::manifest_path(dir.path());
        (dir, graph, manifest)
    }

    #[test]
    #[cfg(unix)]
    fn a_dangling_symlink_manifest_is_removed() {
        let (dir, graph, manifest) = layout();
        std::os::unix::fs::symlink(dir.path().join("later.json"), &manifest).expect("link");
        forget(&graph, Ok(())).expect("forget");
        assert!(fs::symlink_metadata(&manifest).is_err(), "the link is gone");
    }

    #[test]
    #[cfg(unix)]
    fn a_non_regular_manifest_is_removed_without_being_read() {
        // Codex security review of #478: a symlink to `/dev/zero` would be read
        // without end. A FIFO with no writer stands in for it without risking
        // the test's memory: opening it to read blocks forever, so reaching
        // the assertion means nothing opened it.
        let (_dir, graph, manifest) = layout();
        let made = std::process::Command::new("mkfifo")
            .arg(&manifest)
            .status()
            .expect("mkfifo");
        assert!(made.success());
        forget(&graph, Ok(())).expect("forget");
        assert!(fs::symlink_metadata(&manifest).is_err(), "the FIFO is gone");
    }

    #[test]
    fn a_manifest_that_does_not_parse_is_removed() {
        let (_dir, graph, manifest) = layout();
        fs::write(&manifest, "not json").expect("write");
        forget(&graph, Ok(())).expect("forget");
        assert!(!manifest.exists());
    }

    #[test]
    fn a_regular_manifest_keeps_its_states_and_loses_its_scope() {
        let (_dir, graph, manifest) = layout();
        let mut m = indexer::manifest::FileManifest::new();
        m.scope = Some(indexer::IndexOptions::default().scope());
        m.files.insert(
            "a.rs".into(),
            indexer::manifest::FileState {
                mtime_ns: 1,
                size: 2,
                content_hash: "h".into(),
            },
        );
        indexer::manifest::save(&manifest, &m).expect("save");
        forget(&graph, Ok(())).expect("forget");
        let kept = indexer::manifest::load(&manifest).expect("kept");
        assert!(kept.scope.is_none());
        assert_eq!(kept.files.len(), 1);
    }
}
