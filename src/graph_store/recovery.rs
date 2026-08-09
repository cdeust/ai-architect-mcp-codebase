// graph_store::recovery — stale lbug sidecar quarantine (issue #201).
//
// Extracted from graph_store.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared store
// vocabulary exactly as when this lived in one module.

use super::*;

// Stale-sidecar recovery — source: issue #201.
//
// lbug writes temporary sidecars next to the database: `<db>.wal`,
// `<db>.wal.checkpoint`, `<db>.shadow`, `<db>.tmp`. Each carries the UUID of
// the database that produced it, and `FileDBIDUtils::verifyDatabaseID`
// (lbug-src/src/storage/file_db_id_utils.cpp) throws when it does not match
// the database being opened:
//
//   "Database ID for temporary file '<path>' does not match the current
//    database. This file may have been left behind from a previous database
//    with the same name. If it is safe to do so, please delete this file and
//    restart the database."
//
// Observed on a June-vintage graph dir in the 2026-08-06 A/B ingestion bench:
// every subsequent open failed hard and the only remedy was deleting the whole
// directory by hand.
//
// The throw sites are wal_replayer.cpp and shadow_file.cpp — the SIDECARS, never
// the main database file. So recovery does not need to discard the index: move
// the mismatched sidecars aside and reopen. A sidecar whose UUID belongs to a
// different database describes transactions that are not this database's, so
// replaying it would be wrong; discarding it is what upstream advises.
//
// Deliberately narrow. The recovery runs ONLY for this error signature: a full
// disk or a permission fault must never be "fixed" by removing files, so every
// other open failure keeps its existing annotate_write_failure path.
// ---------------------------------------------------------------------------

/// Temporary sidecar suffixes lbug appends to the database path.
/// Source: lbug-src/src/include/common/constants.h StorageConstants
/// (WAL_FILE_SUFFIX / CHECKPOINT_WAL_FILE_SUFFIX / SHADOWING_SUFFIX /
/// TEMP_FILE_SUFFIX).
pub(super) const LBUG_SIDECAR_SUFFIXES: &[&str] = &["wal", "wal.checkpoint", "shadow", "tmp"];

/// True iff `err` is lbug's stale-sidecar database-ID mismatch.
///
/// Matches on two co-occurring fragments of the upstream message rather than
/// the whole string, so incidental rewording upstream does not silently turn
/// recovery off, while an unrelated error cannot accidentally match.
pub(crate) fn is_stale_sidecar_db_id_error(err: &str) -> bool {
    err.contains("Database ID") && err.contains("does not match")
}

impl GraphStore {
    /// Moves mismatched lbug sidecars aside so the database can be reopened.
    ///
    /// Returns Ok(()) when the caller should retry the open. Returns Err when
    /// the error was not the stale-sidecar signature (the caller must surface
    /// the original failure unchanged) or when nothing could be quarantined.
    ///
    /// Quarantine, not delete: the sidecars are renamed to
    /// `<name>.stale-<unix_millis>` beside the database. They are small, the
    /// operation is reversible, and a wrong call is then recoverable — which a
    /// delete would not be. Every action is logged with the reason.
    pub(super) fn recover_from_stale_sidecars(path: &Path, err: &str) -> Result<(), String> {
        if !is_stale_sidecar_db_id_error(err) {
            // Not our condition — hand the original error back untouched so
            // ENOSPC/permission diagnostics keep their existing contract.
            return Err(crate::write_diagnostics::annotate_write_failure(
                path,
                &format!("lbug database open failed: {err}"),
            ));
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut moved = 0usize;
        for suffix in LBUG_SIDECAR_SUFFIXES {
            let mut sidecar = path.as_os_str().to_owned();
            sidecar.push(format!(".{suffix}"));
            let sidecar = std::path::PathBuf::from(sidecar);
            // symlink_metadata: never follow a symlink planted at the sidecar
            // path, and only ever touch a regular file.
            match std::fs::symlink_metadata(&sidecar) {
                Ok(meta) if meta.is_file() => {
                    let mut dest = sidecar.as_os_str().to_owned();
                    dest.push(format!(".stale-{stamp}"));
                    let dest = std::path::PathBuf::from(dest);
                    match std::fs::rename(&sidecar, &dest) {
                        Ok(()) => {
                            moved += 1;
                            eprintln!(
                                "[ap] graph recovery: {} carried a foreign database ID \
                                 (left by a previous database of the same name); moved to {} \
                                 and reopening — the index itself is untouched (issue #201)",
                                sidecar.display(),
                                dest.display()
                            );
                        }
                        Err(e) => {
                            return Err(format!(
                                "graph recovery: could not quarantine stale sidecar {}: {e}",
                                sidecar.display()
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        if moved == 0 {
            // The signature said stale sidecar but none is present next to the
            // database — do not silently retry an open that will fail the same
            // way; say what was looked for.
            return Err(format!(
                "graph recovery: lbug reported a stale-sidecar database-ID mismatch for {} \
                 but no sidecar ({}) was found to quarantine; original error: {err}",
                path.display(),
                LBUG_SIDECAR_SUFFIXES.join(", ")
            ));
        }
        Ok(())
    }
}
