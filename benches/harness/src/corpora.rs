// corpora.rs — load corpus configs + ground truth labels.
//
// Layout per spec:
//   benches/corpora/<name>/corpus.toml       — declares path/language/name
//   benches/corpora/<name>/ground_truth.json — hand labels (see §2.4)
//
// Schema errors are hard errors: a malformed label file fails the load
// rather than silently contributing zeros to the aggregate.  That's a
// zetetic-standard non-negotiable.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Parsed corpus.toml manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct CorpusManifest {
    pub name: String,
    pub language: String,
    /// Path relative to the corpus directory OR absolute.
    pub path: String,
    /// Optional friendly description. Read from disk but not surfaced by the
    /// runner; kept on the struct so `toml::from_str` accepts the field.
    #[serde(default)]
    #[allow(dead_code)]
    pub description: String,
}

/// A single labeled ground-truth entry.  Matches the on-disk schema in
/// §"Ground truth format" of the task brief.
#[derive(Debug, Clone, Deserialize)]
pub struct GroundTruthLabel {
    pub query_id: String,
    pub input: Value,
    pub expected: Value,
}

/// Full ground_truth.json document.
#[derive(Debug, Clone, Deserialize)]
pub struct GroundTruthDoc {
    pub corpus: String,
    pub language: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub version: String,
    pub labels: Vec<GroundTruthLabel>,
}

/// Resolved corpus ready to hand to the runner.
#[derive(Debug, Clone)]
pub struct CorpusConfig {
    pub name: String,
    pub language: String,
    /// Absolute path to the source tree to index.
    pub source_path: PathBuf,
    /// Absolute path to `benches/corpora/<name>/` itself — the anchor for
    /// any label field that names a fixture file relative to the corpus
    /// (e.g. q13's `prd_path`/`affected_symbols_path`), so ground truth
    /// never has to embed a developer-machine-specific absolute path
    /// (issue #210: that class of hardcoded path is stale on every other
    /// checkout, CI included, the same day it's written).
    pub corpus_dir: PathBuf,
    pub labels: Vec<GroundTruthLabel>,
    /// True iff labels is empty (a stub corpus).
    pub is_stub: bool,
}

/// Load one corpus by name.  Returns Err if the directory doesn't exist,
/// corpus.toml is missing/malformed, or ground_truth.json schema is broken.
/// An empty labels array is allowed (stub corpus) and produces is_stub=true.
pub fn load_one(corpora_root: &Path, name: &str) -> Result<CorpusConfig, String> {
    let dir = corpora_root.join(name);
    let manifest_path = dir.join("corpus.toml");
    let truth_path = dir.join("ground_truth.json");

    let manifest = read_manifest(&manifest_path)?;
    let truth = read_truth(&truth_path)?;

    if manifest.name != truth.corpus {
        return Err(format!(
            "corpus mismatch: manifest={} truth={}",
            manifest.name, truth.corpus
        ));
    }
    if manifest.language != truth.language {
        return Err(format!(
            "language mismatch in {}: manifest={} truth={}",
            name, manifest.language, truth.language
        ));
    }

    let source_path = resolve_source_path(&dir, &manifest.path)?;
    let corpus_dir = dir
        .canonicalize()
        .map_err(|e| format!("canonicalize {:?}: {e}", dir))?;
    let is_stub = truth.labels.is_empty();
    Ok(CorpusConfig {
        name: manifest.name,
        language: manifest.language,
        source_path,
        corpus_dir,
        labels: truth.labels,
        is_stub,
    })
}

/// Discover and load every corpus under corpora_root that has a non-empty
/// labels array.  Stubs (is_stub=true) are skipped but logged to stderr so
/// the operator knows they exist.
pub fn discover_all(corpora_root: &Path) -> Result<Vec<CorpusConfig>, String> {
    let mut out = Vec::new();
    let entries = fs::read_dir(corpora_root)
        .map_err(|e| format!("read corpora root {:?}: {e}", corpora_root))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("iterate corpora: {e}"))?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let dir = entry.path();
        if !dir.join("corpus.toml").exists() {
            continue;
        }
        match load_one(corpora_root, &name) {
            Ok(c) if c.is_stub => {
                eprintln!("[bench] skipping stub corpus: {} (no labels yet)", name);
            }
            Ok(c) => out.push(c),
            Err(e) => return Err(format!("corpus {name}: {e}")),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Ground-truth staleness guard (issue #132)
//
// A ground-truth label that names a deleted symbol does not fail — the scorer
// records 0 for the missing expectation and that zero silently depresses the
// reported accuracy. Each parser-migration phase that deletes a source file
// therefore rots the corpus without any signal. This guard makes the rot
// mechanical and loud: it enumerates every source-file path a label references
// and reports the ones that no longer exist under the corpus source tree.
// ---------------------------------------------------------------------------

/// The `<rel_path>` component of a `<rel_path>::<name>` qualified name (or the
/// whole string when there is no `::`, e.g. a bare `f.path` literal).
fn path_of_qualified_name(qn: &str) -> &str {
    qn.split("::").next().unwrap_or(qn)
}

/// Extract source-file paths embedded in a Cypher query string: the
/// `f.path = '<rel_path>'` and `s.qualified_name = '<rel_path>::<name>'`
/// literals the file/field labels key on. For each marker occurrence the
/// literal is the text of the first single-quoted token that follows it. Only
/// `*.rs` literals are collected so that RETURN targets and property names are
/// never mistaken for paths.
fn collect_query_paths(query: &str, out: &mut BTreeSet<String>) {
    for (marker, is_qn) in [("f.path", false), ("s.qualified_name", true)] {
        // `split(marker)` yields the text before the first occurrence, then
        // the text after each occurrence; `skip(1)` drops the pre-marker text
        // so we only look at what follows a marker. `split('\'').nth(1)` is the
        // token between the first pair of single quotes in that tail.
        for after_marker in query.split(marker).skip(1) {
            let Some(literal) = after_marker.split('\'').nth(1) else {
                continue;
            };
            let path = if is_qn {
                path_of_qualified_name(literal)
            } else {
                literal
            };
            if path.ends_with(".rs") {
                out.insert(path.to_string());
            }
        }
    }
}

/// Recursively collect every source-file path a label value references: the
/// `qualified_name`/`qn` fields (structured) and the paths embedded in any
/// `query` string.
fn collect_referenced_paths(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if k == "qualified_name" || k == "qn" {
                    if let Some(s) = val.as_str() {
                        out.insert(path_of_qualified_name(s).to_string());
                    }
                }
                if k == "query" {
                    if let Some(s) = val.as_str() {
                        collect_query_paths(s, out);
                    }
                }
                collect_referenced_paths(val, out);
            }
        }
        Value::Array(arr) => arr.iter().for_each(|x| collect_referenced_paths(x, out)),
        _ => {}
    }
}

/// Ground-truth references whose source-file path no longer exists under
/// `source_root`. A non-empty result means the corpus is scoring against
/// deleted symbols (issue #132); the caller turns each entry into a loud,
/// enumerated failure instead of a silent zero.
pub fn stale_ground_truth(source_root: &Path, labels: &[GroundTruthLabel]) -> Vec<String> {
    let mut refs = BTreeSet::new();
    for label in labels {
        collect_referenced_paths(&label.input, &mut refs);
        collect_referenced_paths(&label.expected, &mut refs);
    }
    refs.into_iter()
        .filter(|rel| !source_root.join(rel).exists())
        .collect()
}

fn read_manifest(path: &Path) -> Result<CorpusManifest, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("read {:?}: {e}", path))?;
    toml::from_str::<CorpusManifest>(&raw).map_err(|e| format!("parse {:?}: {e}", path))
}

fn read_truth(path: &Path) -> Result<GroundTruthDoc, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("read {:?}: {e}", path))?;
    serde_json::from_str::<GroundTruthDoc>(&raw).map_err(|e| format!("parse {:?}: {e}", path))
}

fn resolve_source_path(corpus_dir: &Path, raw: &str) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(raw);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        corpus_dir.join(candidate)
    };
    let canonical = absolute
        .canonicalize()
        .map_err(|e| format!("canonicalize {:?}: {e}", absolute))?;
    if !canonical.exists() {
        return Err(format!("source path does not exist: {:?}", canonical));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn label(input: Value, expected: Value) -> GroundTruthLabel {
        GroundTruthLabel {
            query_id: "q".into(),
            input,
            expected,
        }
    }

    #[test]
    fn stale_guard_flags_a_deleted_qualified_name_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("parser/mod.rs").parent().unwrap()).unwrap();
        std::fs::write(root.join("parser/mod.rs"), "").unwrap();

        let labels = vec![
            // present: parser/mod.rs exists
            label(
                json!({}),
                json!({ "qualified_name": "parser/mod.rs::parse_file" }),
            ),
            // stale: parser/rust/mod.rs was deleted in the migration
            label(
                json!({}),
                json!({ "qualified_name": "parser/rust/mod.rs::parse_rust_file" }),
            ),
        ];
        let stale = stale_ground_truth(root, &labels);
        assert_eq!(stale, vec!["parser/rust/mod.rs".to_string()]);
    }

    #[test]
    fn stale_guard_reads_partition_qn_and_query_literals() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join("live.rs"), "").unwrap();

        let labels = vec![
            // q12 partition rows use `qn`
            label(
                json!({}),
                json!({ "partition": [ { "qn": "gone.rs::Foo", "cluster": 1 } ] }),
            ),
            // q9 embeds the file path in a Cypher `f.path = '...'` literal
            label(
                json!({ "query": "MATCH (f:File) WHERE f.path = 'also_gone.rs' RETURN n.path" }),
                json!({ "imports": [] }),
            ),
            // q11 embeds it in `s.qualified_name = '<path>::<name>'`
            label(
                json!({ "query": "MATCH (s:Struct) WHERE s.qualified_name = 'live.rs::Bar' RETURN f.name" }),
                json!({ "fields": [] }),
            ),
        ];
        let stale = stale_ground_truth(root, &labels);
        // Both deleted paths flagged; the live one is not.
        assert_eq!(
            stale,
            vec!["also_gone.rs".to_string(), "gone.rs".to_string()]
        );
    }

    #[test]
    fn stale_guard_query_extraction_is_position_and_type_accurate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join("live.rs"), "").unwrap();

        let labels = vec![
            // A `.rs` literal BEFORE the marker (`decoy.rs`) must NOT be read —
            // only tokens that FOLLOW a marker count. Two `f.path` markers, one
            // deleted (`gone_a.rs`) and one live (`live.rs`).
            label(
                json!({ "query": "WHERE x.name = 'decoy.rs' AND f.path = 'gone_a.rs' AND f.path = 'live.rs' RETURN n" }),
                json!({ "imports": [] }),
            ),
            // `s.qualified_name` literal: the path is the part before `::`.
            label(
                json!({ "query": "WHERE s.qualified_name = 'gone_b.rs::Foo' RETURN f" }),
                json!({ "fields": [] }),
            ),
        ];
        let stale = stale_ground_truth(root, &labels);
        // decoy.rs excluded (pre-marker); live.rs excluded (exists); the two
        // deleted paths flagged, `::Foo` stripped from the qualified name.
        assert_eq!(
            stale,
            vec!["gone_a.rs".to_string(), "gone_b.rs".to_string()]
        );
    }

    #[test]
    fn stale_guard_is_empty_when_every_path_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join("a.rs"), "").unwrap();
        let labels = vec![label(
            json!({ "qualified_name": "a.rs::A" }),
            json!({ "qualified_name": "a.rs::A" }),
        )];
        assert!(stale_ground_truth(root, &labels).is_empty());
    }
}
