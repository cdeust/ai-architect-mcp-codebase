// search::vector — Sparse TF-IDF vector search for semantic similarity.
//
// Neural embeddings (fastembed) were evaluated but add ~200MB ONNX Runtime
// dep. TF-IDF over tokenized symbol names provides semantic matching
// (e.g., "process incoming requests" finds "handle_tool_call" via shared
// vocabulary patterns) with zero additional dependencies.
//
// Tokenization: split on `_`, `::`, camelCase -> lowercase terms.
// Weighting: TF-IDF (term frequency * inverse document frequency).
// Similarity: cosine similarity between query vector and document vectors.
//
// Storage model — sparse, hand-rolled binary. source: Fermi audit April 2026.
// The previous implementation persisted a dense N_docs x |V| f32 matrix as
// CSV, which scaled as O(N * |V|) in both disk and RAM. On a 50K-symbol
// codebase with a 20K vocabulary that is ~4 GB. Observed corpora are
// Zipfian: each document's non-zero term count is bounded by its token
// count, typically under 30. Storing only non-zero (term_id, weight) pairs
// keeps the index at O(nnz) ~= 12 MB instead of 4 GB.
//
// Binary layout (little-endian throughout):
//   header:
//     u32 magic = 0x56454354 ("VECT")
//     u32 version = 1
//     u32 vocab_size
//     u32 doc_count
//   vocabulary: for each term,
//     u32 term_len
//     [u8; term_len] utf-8 bytes
//   documents: for each doc,
//     u32 qn_len, [u8] qualified_name utf-8
//     u32 name_len, [u8] name
//     u32 label_len, [u8] label
//     u32 fp_len, [u8] file_path
//     u32 nnz  (non-zero term count)
//     repeat nnz times: u32 term_id, f32 weight
//
// Length-prefixed so the reader never depends on delimiters and never
// copies the whole file into memory beyond the final struct.

use std::collections::HashMap;
use std::path::Path;

use super::bm25::tokenize_symbol;
use super::qualified_name::file_path_of;
use super::vector_format::{load_index, save_index};
use crate::graph_store::GraphStore;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single document in the vector index. Sparse representation: only
/// non-zero (term_id, weight) pairs are stored, sorted by term_id.
pub(super) struct VectorDoc {
    pub(super) qualified_name: String,
    pub(super) name: String,
    pub(super) label: String,
    pub(super) file_path: String,
    /// Sorted by term_id ascending. u32 term_id fits vocabularies up to 4B.
    pub(super) terms: Vec<(u32, f32)>,
}

/// In-memory vector index. Built once, queried many times.
pub struct VectorIndex {
    pub(super) docs: Vec<VectorDoc>,
    /// vocabulary[i] is the term with id = i. Sorted for deterministic id
    /// assignment; binary search recovers term_id from the term string.
    pub(super) vocabulary: Vec<String>,
}

pub struct VectorResult {
    pub qualified_name: String,
    #[allow(dead_code)]
    pub name: String,
    #[allow(dead_code)]
    pub label: String,
    #[allow(dead_code)]
    pub file_path: String,
    #[allow(dead_code)]
    pub score: f32,
}

// ---------------------------------------------------------------------------
// Index building
// ---------------------------------------------------------------------------

const SEARCHABLE_LABELS: &[&str] = &[
    "Function",
    "Method",
    "Struct",
    "Enum",
    "Trait",
    "Module",
    "Constant",
    "TypeAlias",
];

/// Binary file magic and version for the sparse TF-IDF on-disk format.
/// source: Fermi audit April 2026 — replaces the old text CSV format.
pub(super) const VECTOR_MAGIC: u32 = 0x56454354; // "VECT"
pub(super) const VECTOR_VERSION: u32 = 1;

/// Builds a TF-IDF vector index from all symbol nodes in the graph.
/// Persists to `index_dir/vector_index.bin` for later loading.
pub fn build_index(store: &GraphStore, index_dir: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(index_dir).map_err(|e| format!("create vector index dir: {e}"))?;

    let raw_docs = collect_raw_docs(store);
    if raw_docs.is_empty() {
        return Ok(0);
    }

    let (vocabulary, idf, term_id_of) = build_vocabulary_idf(&raw_docs);
    let docs: Vec<VectorDoc> = raw_docs
        .into_iter()
        .map(|(qn, name, label, fp, tokens)| {
            let terms = compute_sparse_tf_idf(&tokens, &term_id_of, &idf);
            VectorDoc {
                qualified_name: qn,
                name,
                label,
                file_path: fp,
                terms,
            }
        })
        .collect();

    let doc_count = docs.len();
    let index = VectorIndex { docs, vocabulary };
    save_index(&index, index_dir)?;
    Ok(doc_count)
}

fn collect_raw_docs(store: &GraphStore) -> Vec<(String, String, String, String, Vec<String>)> {
    let mut raw_docs = Vec::new();
    for &label in SEARCHABLE_LABELS {
        let cypher = format!("MATCH (n:{label}) RETURN n.qualified_name, n.name, n.id");
        let qr = match store.execute_query(&cypher) {
            Ok(qr) => qr,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() < 3 {
                continue;
            }
            let qn = &row[0];
            let name_val = &row[1];
            let file_path = file_path_of(qn).to_string();
            let combined = format!("{} {} {}", name_val, label, qn);
            let tokens = tokenize_to_terms(&combined);
            raw_docs.push((
                qn.clone(),
                name_val.clone(),
                label.to_string(),
                file_path,
                tokens,
            ));
        }
    }
    raw_docs
}

/// Loads a persisted vector index and queries it with cosine similarity.
pub fn query_index(
    index_dir: &Path,
    query_str: &str,
    limit: usize,
) -> Result<Vec<VectorResult>, String> {
    let index_path = index_dir.join("vector_index.bin");
    if !index_path.exists() {
        return Ok(Vec::new());
    }
    let index = load_index(index_dir)?;
    if index.docs.is_empty() {
        return Ok(Vec::new());
    }

    let query_tokens = tokenize_to_terms(query_str);
    let idf = reconstruct_idf(&index);
    let term_id_of = vocab_to_map(&index.vocabulary);
    let query_terms = compute_sparse_tf_idf(&query_tokens, &term_id_of, &idf);
    let query_norm = sparse_norm(&query_terms);
    if query_norm < 1e-10 {
        return Ok(Vec::new());
    }

    let mut scored: Vec<(usize, f32)> = index
        .docs
        .iter()
        .enumerate()
        .map(|(i, doc)| (i, sparse_cosine(&query_terms, query_norm, &doc.terms)))
        .filter(|(_, s)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    Ok(scored
        .into_iter()
        .map(|(i, score)| {
            let doc = &index.docs[i];
            VectorResult {
                qualified_name: doc.qualified_name.clone(),
                name: doc.name.clone(),
                label: doc.label.clone(),
                file_path: doc.file_path.clone(),
                score,
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// TF-IDF computation — sparse
// ---------------------------------------------------------------------------

fn tokenize_to_terms(s: &str) -> Vec<String> {
    tokenize_symbol(s)
        .split_whitespace()
        .map(|t| t.to_string())
        .collect()
}

fn build_vocabulary_idf(
    raw_docs: &[(String, String, String, String, Vec<String>)],
) -> (Vec<String>, Vec<f32>, HashMap<String, u32>) {
    // Document frequency per term.
    let mut df: HashMap<String, usize> = HashMap::new();
    for (_, _, _, _, tokens) in raw_docs {
        let unique: std::collections::HashSet<&String> = tokens.iter().collect();
        for t in unique {
            *df.entry(t.clone()).or_insert(0) += 1;
        }
    }

    let n = raw_docs.len() as f32;
    let mut vocabulary: Vec<String> = df.keys().cloned().collect();
    vocabulary.sort();

    let idf: Vec<f32> = vocabulary
        .iter()
        .map(|term| {
            let d = df.get(term).copied().unwrap_or(1) as f32;
            (n / d).ln() + 1.0
        })
        .collect();

    let term_id_of = vocab_to_map(&vocabulary);
    (vocabulary, idf, term_id_of)
}

fn vocab_to_map(vocabulary: &[String]) -> HashMap<String, u32> {
    vocabulary
        .iter()
        .enumerate()
        .map(|(i, t)| (t.clone(), i as u32))
        .collect()
}

/// Returns the sparse TF-IDF representation: (term_id, weight) pairs sorted
/// by term_id. Only non-zero weights are stored.
fn compute_sparse_tf_idf(
    tokens: &[String],
    term_id_of: &HashMap<String, u32>,
    idf: &[f32],
) -> Vec<(u32, f32)> {
    let mut tf: HashMap<&str, f32> = HashMap::new();
    for t in tokens {
        *tf.entry(t.as_str()).or_insert(0.0) += 1.0;
    }
    let max_tf = tf.values().copied().fold(0.0f32, f32::max).max(1.0);

    let mut out: Vec<(u32, f32)> = Vec::with_capacity(tf.len());
    for (term, raw_tf) in &tf {
        if let Some(&id) = term_id_of.get(*term) {
            let norm_tf = 0.5 + 0.5 * (*raw_tf / max_tf);
            let w = norm_tf * idf[id as usize];
            if w != 0.0 {
                out.push((id, w));
            }
        }
    }
    out.sort_by_key(|(id, _)| *id);
    out
}

/// A sparse term vector: `(term_id, weight)` pairs kept sorted by term id, so
/// two of them can be merged in one linear pass. Named because the bare tuple
/// shape recurs across this module's signatures and stored documents.
type SparseTerms = [(u32, f32)];

fn sparse_norm(v: &SparseTerms) -> f32 {
    v.iter().map(|(_, w)| w * w).sum::<f32>().sqrt()
}

/// Cosine similarity of a sparse query against a sparse doc. Merges the
/// two sorted (term_id, weight) lists in a single linear pass.
fn sparse_cosine(query: &SparseTerms, query_norm: f32, doc: &SparseTerms) -> f32 {
    if query.is_empty() || doc.is_empty() {
        return 0.0;
    }
    let mut i = 0usize;
    let mut j = 0usize;
    let mut dot = 0.0f32;
    while i < query.len() && j < doc.len() {
        let (qi, qw) = query[i];
        let (dj, dw) = doc[j];
        if qi == dj {
            dot += qw * dw;
            i += 1;
            j += 1;
        } else if qi < dj {
            i += 1;
        } else {
            j += 1;
        }
    }
    let doc_norm = sparse_norm(doc);
    if query_norm < 1e-10 || doc_norm < 1e-10 {
        return 0.0;
    }
    dot / (query_norm * doc_norm)
}

fn reconstruct_idf(index: &VectorIndex) -> Vec<f32> {
    let n = index.docs.len() as f32;
    let mut df = vec![0usize; index.vocabulary.len()];
    for doc in &index.docs {
        for (id, _) in &doc.terms {
            df[*id as usize] += 1;
        }
    }
    df.iter()
        .map(|&d| {
            let d = d.max(1) as f32;
            (n / d).ln() + 1.0
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sparse_cosine_identical() {
        let v = vec![(0u32, 1.0f32), (3, 2.0), (7, 3.0)];
        let n = sparse_norm(&v);
        let sim = sparse_cosine(&v, n, &v);
        assert!((sim - 1.0).abs() < 1e-5, "identical: {sim}");
    }

    #[test]
    fn test_sparse_cosine_orthogonal() {
        let a = vec![(0u32, 1.0f32)];
        let b = vec![(1u32, 1.0f32)];
        let sim = sparse_cosine(&a, sparse_norm(&a), &b);
        assert!(sim.abs() < 1e-5, "orthogonal: {sim}");
    }

    #[test]
    fn test_tokenize_to_terms() {
        let terms = tokenize_to_terms("handle_tool_call");
        assert_eq!(terms, vec!["handle", "tool", "call"]);
    }

    #[test]
    fn test_sparse_round_trip() {
        let dir = std::env::temp_dir().join("vector_round_trip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let idx = VectorIndex {
            vocabulary: vec!["alpha".into(), "beta".into(), "gamma".into()],
            docs: vec![VectorDoc {
                qualified_name: "a::b".into(),
                name: "b".into(),
                label: "Function".into(),
                file_path: "a".into(),
                terms: vec![(0, 0.5), (2, 1.25)],
            }],
        };
        save_index(&idx, &dir).unwrap();
        let loaded = load_index(&dir).unwrap();
        assert_eq!(loaded.vocabulary, idx.vocabulary);
        assert_eq!(loaded.docs.len(), 1);
        assert_eq!(loaded.docs[0].qualified_name, "a::b");
        assert_eq!(loaded.docs[0].terms, vec![(0, 0.5), (2, 1.25)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
