// classify — discovery + manifest-diff classification (issue #288 split).
//
// Walks the current tree and partitions it against the prior manifest into
// changed/added/deleted/unchanged, with rename detection via content-hash
// pairing. This is the change-detection path `apply_changes` (parent module)
// consumes; it does not touch the graph.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::super::coverage::{CoverageCollector, FileCoverage};
use super::super::manifest::{self, FileManifest, FileState};
use super::super::walk::collect_source_files;
use super::super::{light_link, walk::WalkOptions};
use super::{ChangeSet, Discovered, Plan, Rename};
use std::collections::BTreeMap;

/// Walks `codebase` and reads each file's (mtime, size), plus the walk-level
/// coverage gaps (issue #249: directories excluded by `exclude_dirs`, or
/// skipped because they were unreadable). Postcondition: one `Discovered` per
/// source file the full index would visit, with `rel` equal to the File node
/// id the indexer assigns; the returned map holds one `FileCoverage` entry per
/// pruned directory, keyed by its walk-root-relative path.
/// What one discovery pass observed: the files to index, the per-file gaps it
/// recorded, and the directories the built-in policy pruned with their reason.
/// A named alias rather than a bare triple, which clippy rejects as a complex
/// type and which reads worse at three call sites. source: ADR-9841.
pub(super) type Discovery = (
    Vec<Discovered>,
    BTreeMap<String, FileCoverage>,
    BTreeMap<String, String>,
);

pub(super) fn discover(codebase: &Path, walk_opts: WalkOptions) -> Result<Discovery, String> {
    let outcome = collect_source_files(codebase, walk_opts)?;
    let mut collector = CoverageCollector::default();
    for rel in &outcome.excluded_dirs {
        collector.record_skipped(rel, "user_excluded".to_string());
    }
    for (rel, reason) in &outcome.pruned_dirs {
        collector.record_pruned(rel, reason);
    }
    for rel in &outcome.unreadable_dirs {
        collector.record_skipped(rel, "unreadable".to_string());
    }
    let mut out = Vec::with_capacity(outcome.files.len());
    for abs in outcome.files {
        let rel = light_link::rel_id(codebase, &abs);
        let (mtime_ns, size) = match std::fs::metadata(&abs) {
            Ok(m) => (manifest::mtime_ns(&m), m.len()),
            Err(_) => (0, 0),
        };
        out.push(Discovered {
            rel,
            abs,
            mtime_ns,
            size,
        });
    }
    let pruned = collector.pruned_dirs().clone();
    Ok((out, collector.into_files(), pruned))
}

/// Classifies `current` against `prior`. See `Plan`. Rename detection pairs a
/// deleted file with an added file that carries an identical content hash (the
/// deleted file's hash comes from the manifest — the file is gone from disk, so
/// it cannot be re-hashed; the added file's hash is read fresh).
///
/// Orchestrates three phases (Fowler "Extract Function", §4.2 — this used to
/// be one 124-line function): `diff_current_against_prior` (mtime+size, hash
/// fallback), `index_deleted_by_hash` (indexes prior-only files for the
/// rename lookup), then `match_renames_and_adds` (matches added candidates
/// against that index). `next` — the manifest state to persist — accumulates
/// across all three phases, so it's threaded through and mutated in place
/// rather than rebuilt at the end.
pub(super) fn classify(prior: &FileManifest, current: &[Discovered]) -> Plan {
    let current_ids: HashSet<&str> = current.iter().map(|d| d.rel.as_str()).collect();
    let mut next = FileManifest::new();

    let (changed, added_candidates, unchanged) =
        diff_current_against_prior(prior, current, &mut next);

    let (deleted_candidates, mut deleted_by_hash) = index_deleted_by_hash(prior, &current_ids);
    let (renamed, added, consumed_deleted) =
        match_renames_and_adds(added_candidates, &mut deleted_by_hash, &mut next);

    // Truly-deleted = deleted candidates not consumed by a rename pairing.
    let deleted: Vec<String> = deleted_candidates
        .into_iter()
        .filter(|rel| !consumed_deleted.contains(rel))
        .collect();

    Plan {
        change_set: ChangeSet {
            changed,
            added,
            deleted,
            renamed,
        },
        unchanged,
        next_manifest: next,
    }
}

/// Phase 1: classifies every CURRENT file against `prior` by cheap signal
/// (mtime+size) with a content-hash fallback when either moved — identical
/// bytes despite a moved mtime is the correctness fallback the C reference
/// lacks: a `touch` with no edit is NOT a change. Returns the (changed,
/// added_candidates, unchanged) partitions; `added_candidates` may still
/// resolve to a rename's new end in `match_renames_and_adds`. Fills `next`
/// with every current file's fresh state (a fresh hash for a moved
/// mtime/size, or the prior state carried forward unchanged).
fn diff_current_against_prior(
    prior: &FileManifest,
    current: &[Discovered],
    next: &mut FileManifest,
) -> (Vec<Discovered>, Vec<Discovered>, Vec<Discovered>) {
    let mut changed = Vec::new();
    let mut added_candidates = Vec::new();
    let mut unchanged = Vec::new();

    for d in current {
        match prior.files.get(&d.rel) {
            None => {
                // Not previously known → a new file (may still be a rename's
                // new end; resolved by match_renames_and_adds).
                added_candidates.push(d.clone());
            }
            Some(prev) if prev.mtime_ns == d.mtime_ns && prev.size == d.size => {
                // Cheap signal says unchanged: carry the prior hash forward.
                unchanged.push(d.clone());
                next.files.insert(d.rel.clone(), prev.clone());
            }
            Some(prev) => {
                let hash = manifest::hash_file(&d.abs).unwrap_or_default();
                let same_content = !hash.is_empty() && hash == prev.content_hash;
                if same_content {
                    unchanged.push(d.clone());
                } else {
                    changed.push(d.clone());
                }
                next.files.insert(
                    d.rel.clone(),
                    FileState {
                        mtime_ns: d.mtime_ns,
                        size: d.size,
                        content_hash: hash,
                    },
                );
            }
        }
    }

    (changed, added_candidates, unchanged)
}

/// Phase 2: indexes prior-only files (present before, absent from `current`)
/// by their stored content hash — the rename lookup `match_renames_and_adds`
/// probes against. A deleted file's hash comes from the manifest; the file no
/// longer exists on disk to re-hash.
fn index_deleted_by_hash(
    prior: &FileManifest,
    current_ids: &HashSet<&str>,
) -> (Vec<String>, HashMap<String, Vec<String>>) {
    // Deleted candidates: prior files absent from the current discovery.
    let deleted_candidates: Vec<String> = prior
        .files
        .keys()
        .filter(|rel| !current_ids.contains(rel.as_str()))
        .cloned()
        .collect();

    let mut deleted_by_hash: HashMap<String, Vec<String>> = HashMap::new();
    for rel in &deleted_candidates {
        if let Some(state) = prior.files.get(rel) {
            if !state.content_hash.is_empty() {
                deleted_by_hash
                    .entry(state.content_hash.clone())
                    .or_default()
                    .push(rel.clone());
            }
        }
    }

    (deleted_candidates, deleted_by_hash)
}

/// Phase 3: matches each `added_candidates` entry's freshly-read content hash
/// against `deleted_by_hash`; an unconsumed match becomes a `Rename` instead
/// of an add. Every added/renamed-new file gets its fresh state written into
/// `next` (mirrors `diff_current_against_prior`'s bookkeeping for the
/// current-file partitions). Returns the renamed pairs, the true adds, and
/// the set of prior-file rels consumed by a rename — `classify` uses that set
/// to filter its "truly deleted" list.
fn match_renames_and_adds(
    added_candidates: Vec<Discovered>,
    deleted_by_hash: &mut HashMap<String, Vec<String>>,
    next: &mut FileManifest,
) -> (Vec<Rename>, Vec<Discovered>, HashSet<String>) {
    let mut renamed = Vec::new();
    let mut added = Vec::new();
    let mut consumed_deleted: HashSet<String> = HashSet::new();

    for d in added_candidates {
        let hash = manifest::hash_file(&d.abs).unwrap_or_default();
        let matched_old = if hash.is_empty() {
            None
        } else {
            deleted_by_hash.get_mut(&hash).and_then(|olds| {
                olds.iter()
                    .position(|o| !consumed_deleted.contains(o))
                    .map(|i| olds[i].clone())
            })
        };
        // Every added/renamed-new file gets its fresh state in the manifest.
        next.files.insert(
            d.rel.clone(),
            FileState {
                mtime_ns: d.mtime_ns,
                size: d.size,
                content_hash: hash.clone(),
            },
        );
        match matched_old {
            Some(old_rel) => {
                consumed_deleted.insert(old_rel.clone());
                renamed.push(Rename {
                    old_rel,
                    new_file: d,
                });
            }
            None => added.push(d),
        }
    }

    (renamed, added, consumed_deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the fixture `classify_detects_each_class_and_rename` exercises
    /// (Fowler "Extract Function", §4.2 — the test used to be one 75-line
    /// function): an on-disk current tree — keep.py (unchanged), edit.py
    /// (modified), new.py (added), moved_to.py (rename target) — plus a prior
    /// manifest covering every classification category: keep.py unchanged;
    /// edit.py with a stale hash (edited); old_name.py carrying moved_to.py's
    /// content hash (the rename); gone.py a plain deletion. Returns the prior
    /// manifest, the current discovery, and the tempdir guard — the caller
    /// must hold the guard for the test's duration (drop removes the tree).
    fn build_classify_fixture() -> (FileManifest, Vec<Discovered>, tempfile::TempDir) {
        let dir = tempfile::Builder::new()
            .prefix("incremental_classify_")
            .tempdir()
            .expect("temp dir");
        let root = dir.path();

        std::fs::write(root.join("keep.py"), "def keep():\n    return 1\n").unwrap();
        std::fs::write(root.join("edit.py"), "def edit():\n    return 2\n").unwrap();
        std::fs::write(root.join("new.py"), "def fresh():\n    return 3\n").unwrap();
        // Rename: moved_to.py holds the exact bytes old_name.py had.
        let moved_body = "def moved():\n    return 4\n";
        std::fs::write(root.join("moved_to.py"), moved_body).unwrap();

        let (current, _gaps, _pruned) = discover(root, WalkOptions::default()).expect("discover");
        let hash_of = |rel: &str| manifest::hash_file(&root.join(rel)).unwrap();

        let keep = current.iter().find(|d| d.rel == "keep.py").unwrap();
        let mut prior = FileManifest::new();
        prior.files.insert(
            "keep.py".into(),
            FileState {
                mtime_ns: keep.mtime_ns,
                size: keep.size,
                content_hash: hash_of("keep.py"),
            },
        );
        prior.files.insert(
            "edit.py".into(),
            FileState {
                mtime_ns: 1,
                size: 999,
                content_hash: "stale".into(),
            },
        );
        prior.files.insert(
            "old_name.py".into(),
            FileState {
                mtime_ns: 1,
                size: moved_body.len() as u64,
                content_hash: hash_of("moved_to.py"),
            },
        );
        prior.files.insert(
            "gone.py".into(),
            FileState {
                mtime_ns: 1,
                size: 10,
                content_hash: "whatever".into(),
            },
        );

        (prior, current, dir)
    }

    #[test]
    fn classify_detects_each_class_and_rename() {
        let (prior, current, _dir) = build_classify_fixture();

        let plan = classify(&prior, &current);
        let cs = &plan.change_set;
        let changed: Vec<&str> = cs.changed.iter().map(|d| d.rel.as_str()).collect();
        let added: Vec<&str> = cs.added.iter().map(|d| d.rel.as_str()).collect();
        let unchanged: Vec<&str> = plan.unchanged.iter().map(|d| d.rel.as_str()).collect();

        assert_eq!(changed, vec!["edit.py"], "edited file is changed");
        assert_eq!(added, vec!["new.py"], "genuinely new file is added");
        assert_eq!(unchanged, vec!["keep.py"], "untouched file is unchanged");
        assert_eq!(cs.deleted, vec!["gone.py"], "removed file is deleted");
        assert_eq!(cs.renamed.len(), 1, "one rename detected");
        assert_eq!(cs.renamed[0].old_rel, "old_name.py");
        assert_eq!(cs.renamed[0].new_file.rel, "moved_to.py");
        // Manifest carries every current file, and no stale prior-only entries.
        assert_eq!(plan.next_manifest.files.len(), 4);
        assert!(plan.next_manifest.files.contains_key("moved_to.py"));
        assert!(!plan.next_manifest.files.contains_key("old_name.py"));
    }
}
