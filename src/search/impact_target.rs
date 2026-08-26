// search::impact_target — resolves the `qualified_name` argument of
// `get_impact` to the key the graph is actually stored under, plus the File
// that key belongs to.
//
// `get_impact` accepts a wider target set than its peer tools: since issue
// #205 it answers file-level fan-in (`References_File_File`, `Imports_File_
// File`) for a File target as well as reverse dependencies for a symbol.
// `resolve_qualified_name` only probes the eight symbol labels, so it cannot
// serve as that tool's gate on its own — see `resolve_impact_target`.
//
// Why the File is recovered from the qualified name's path part rather than by
// walking the symbol's `Defines_File_<Label>` edge. The structural edge looks
// like the more principled route; measured, it is strictly worse:
//
//   * There is no `Defines_File_Method` table at all. The query is not a miss
//     but a hard Binder exception ("Table Defines_File_Method does not exist"),
//     which drops the whole query's results. Methods reach their File only in
//     two hops, through one of three different middle labels
//     (`HasMethod_{Struct,Enum,Trait}_Method` then `Defines_File_<middle>`).
//   * Measured on this repository's own `src/` (213 files, 316 Methods,
//     2026-08-25): the two-hop route reaches 247 Methods via Struct, 34 via
//     Trait and 17 via Enum — 298 of 316, leaving 18 (5.7%) with no File at
//     all. The path-part route below resolves 316 of 316.
//   * It also cannot serve the File-target branch, where there is no symbol to
//     traverse from, nor the caller's-raw-input fallback candidate.
//
// So the Defines route would trade one query for up to three, lose 5.7% of
// Method targets' co-change sections, and still need this lookup beside it.
// Rejected; the single-candidate query below stays.

use super::qualified_name::file_path_of;
use super::{resolve_qualified_name, SymbolNotFound};
use crate::graph_store::GraphStore;
use lbug::Value;

/// A resolved `get_impact` target.
pub struct ImpactTarget {
    /// The key the graph stores this target under: a symbol's stored
    /// `qualified_name`, or a `File.id`. Every graph lookup for the request
    /// must use THIS, not the caller's raw input.
    pub key: String,
    /// The `File.id` the target belongs to, when one is recoverable. Used for
    /// the co-change section, which is keyed by file.
    pub file: Option<String>,
}

/// Resolves `input` to the stored graph key for a `get_impact` request.
///
/// Why (review findings 2 and 5). Gating `get_impact` on
/// `resolve_qualified_name` alone made `get_impact("src/main.rs")` — the
/// File-target fan-in that issue #205 exists to provide — return
/// `symbol_not_found`, because that resolver probes only Function, Method,
/// Struct, Enum, Trait, Module, Constant and TypeAlias, and `File` carries no
/// `qualified_name` column at all (a `WHERE n.qualified_name = ..` probe over
/// `File` is a binder error, not a miss). So File targets are resolved here by
/// `id`, after the symbol layers have had their turn.
///
/// The same call also recovers the target's File, because the two are one
/// question and answering them separately is what let them disagree: the
/// impact query used the resolved key while the co-change section split the
/// caller's RAW input on `::`. That worked only for the unstripped form. The
/// parser strips the leading path component when building `qualified_name`, so
/// the stored form `main.rs::foo` — which this tool's own `next_steps` tells
/// callers to chain on — yielded the file `main.rs`, which is no `File.id`,
/// and co-change silently returned `[]`.
pub fn resolve_impact_target(
    store: &GraphStore,
    input: &str,
) -> Result<ImpactTarget, SymbolNotFound> {
    match resolve_qualified_name(store, input) {
        Ok(qn) => {
            // A symbol whose own path part misses (an unusual qualified name
            // shape) can still resolve through the caller's input.
            let file = resolve_file(store, &[file_path_of(&qn), file_path_of(input)]);
            Ok(ImpactTarget { key: qn, file })
        }
        Err(not_found) => match resolve_file(store, &[input]) {
            Some(id) => Ok(ImpactTarget {
                key: id.clone(),
                file: Some(id),
            }),
            None => Err(not_found),
        },
    }
}

/// The first of `candidates` that names a `File`, in order.
///
/// Sole entry point for File resolution in this module. A request can offer
/// more than one candidate because a symbol's own path part and the caller's
/// raw input need not agree; when they DO agree — the common case, a caller
/// chaining on the stored form this tool's own `next_steps` hands back — the
/// repeat used to cost a second identical query, because each candidate was
/// probed through its own call. Owning the sequence here means the ordering,
/// the empty-candidate skip and the de-duplication are stated once instead of
/// being re-derived per call site.
fn resolve_file(store: &GraphStore, candidates: &[&str]) -> Option<String> {
    let mut tried: Vec<&str> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if candidate.is_empty() || tried.contains(candidate) {
            continue;
        }
        tried.push(candidate);
        if let Some(id) = query_file_id(store, candidate) {
            return Some(id);
        }
    }
    None
}

/// Resolves one `path` to a `File.id`, tolerating the one leading path
/// component the parser strips when it builds qualified names (`main.rs` →
/// `src/main.rs`).
///
/// An EXACT `File.id` match always wins, unconditionally — see
/// [`exact_file_id`] for the wrong answers the previous single-query form
/// returned. The suffix form is the fallback, and only that.
///
/// `ENDS WITH '/' || path` is anchored on a full path segment, so `main.rs`
/// cannot match `domain.rs`. It can still match two files in different
/// directories — but those two files would also share one `qualified_name`
/// prefix, so that ambiguity is a property of the qualified-name scheme rather
/// than of this lookup. `ORDER BY f.id` makes the choice deterministic instead
/// of leaving it to the engine's scan order.
///
/// Both queries BIND `path` rather than interpolating it: it is a
/// caller-supplied qualified-name fragment, and each query text is constant so
/// the prepared statement caches across every lookup.
///
/// Resolving instead through the symbol's `Defines_File_<Label>` edge was
/// evaluated and rejected — see this module's header.
///
/// `pub(super)` rather than private: `hybrid::enrich_file_hit` resolves a
/// doc/prose BM25 hit's key to its `File` through this same lookup
/// (fleet-watch#112). Two File resolvers that tolerate different key shapes is
/// how one of them silently stops finding files the other still finds.
pub(super) fn query_file_id(store: &GraphStore, path: &str) -> Option<String> {
    exact_file_id(store, path).or_else(|| suffix_file_id(store, path))
}

/// The `File` whose `id` IS `path`. Asked first, and separately, so that an
/// exact match can never lose to a suffix collision.
///
/// The two lookups used to share one query —
/// `WHERE f.id = $exact OR f.id ENDS WITH $suffix ORDER BY f.id LIMIT 1` —
/// which made the winner whichever id sorted first, not whichever matched
/// better. Both `docs/guide.md` (the real target) and `api/docs/guide.md` (an
/// unrelated file that merely ends with the same segment) satisfy that WHERE,
/// and `api/...` sorts earlier, so the caller silently received metadata for
/// the wrong file. A wrong answer, not an error — nothing anywhere reported it.
///
/// Ordering by a computed priority (`ORDER BY (f.id = $exact) DESC`) would also
/// work, but rests on how the engine sorts booleans; asking the exact question
/// first rests on nothing. It is also the cheaper shape in the common case,
/// where the exact lookup is a primary-key hit and the second query never runs.
fn exact_file_id(store: &GraphStore, path: &str) -> Option<String> {
    let cypher = "MATCH (f:File) WHERE f.id = $exact RETURN f.id LIMIT 1";
    let params = vec![("exact", Value::String(path.to_string()))];
    let qr = store.query_prepared_params(cypher, params).ok()?;
    qr.rows.first()?.first().cloned()
}

/// The `File` whose `id` ends with `/path` — the one leading path component the
/// parser strips when it builds qualified names (`main.rs` → `src/main.rs`).
///
/// Only consulted when no exact match exists. `ORDER BY f.id` makes the choice
/// among several suffix collisions deterministic rather than leaving it to the
/// engine's scan order; it no longer decides anything against an exact match.
fn suffix_file_id(store: &GraphStore, path: &str) -> Option<String> {
    let cypher = "MATCH (f:File) WHERE f.id ENDS WITH $suffix \
                  RETURN f.id ORDER BY f.id LIMIT 1";
    let params = vec![("suffix", Value::String(format!("/{path}")))];
    let qr = store.query_prepared_params(cypher, params).ok()?;
    qr.rows.first()?.first().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_store::{cypher_str, NODE_FILE, NODE_FUNCTION};

    /// A graph shaped like a real index: the root is the repo, so `File.id`
    /// keeps the `src/` component that the parser strips when it builds
    /// `qualified_name`. That gap is the whole subject of these tests.
    fn fixture() -> (tempfile::TempDir, GraphStore) {
        let dir = tempfile::Builder::new()
            .prefix("impact_target")
            .tempdir()
            .expect("tempdir");
        let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open");
        store.create_schema().expect("schema");
        for id in ["src/helpers.rs", "src/main.rs", "docs/guide.md"] {
            store
                .insert_node(
                    NODE_FILE,
                    &[
                        ("id", cypher_str(id).as_str()),
                        ("path", cypher_str(id).as_str()),
                        ("name", "'x'"),
                        ("extension", "'rs'"),
                        ("size_bytes", "1"),
                        ("parse_errors", "0"),
                    ],
                )
                .expect("insert file");
        }
        store
            .insert_node(
                NODE_FUNCTION,
                &[
                    ("id", "'helpers.rs::sanitize'"),
                    ("name", "'sanitize'"),
                    ("qualified_name", "'helpers.rs::sanitize'"),
                    ("start_line", "1"),
                    ("end_line", "3"),
                    ("visibility", "'pub'"),
                    ("is_async", "false"),
                ],
            )
            .expect("insert fn");
        (dir, store)
    }

    #[test]
    fn a_file_target_resolves_instead_of_reporting_symbol_not_found() {
        // Review finding 2 (regression). Gating get_impact on the symbol-only
        // resolver made `get_impact("src/main.rs")` — the File-target fan-in
        // issue #205 exists to provide — return `symbol_not_found`.
        let (_dir, store) = fixture();
        let target = resolve_impact_target(&store, "src/main.rs")
            .unwrap_or_else(|e| panic!("a File target must resolve, got not-found: {:?}", e.input));
        assert_eq!(target.key, "src/main.rs");
        assert_eq!(target.file.as_deref(), Some("src/main.rs"));

        // A non-code file is equally a valid target (issue #205 references).
        let doc = resolve_impact_target(&store, "docs/guide.md").expect("doc File target");
        assert_eq!(doc.key, "docs/guide.md");
    }

    #[test]
    fn the_stored_qualified_name_form_still_finds_its_file() {
        // Review finding 5 (regression). The impact query used the RESOLVED
        // key while the co-change section split the caller's RAW input on
        // `::`. For the stored form — the one this tool's own `next_steps`
        // tells callers to chain on — that yielded `helpers.rs`, which is no
        // `File.id`, so co-change silently returned [].
        let (_dir, store) = fixture();
        for input in ["helpers.rs::sanitize", "src/helpers.rs::sanitize"] {
            let target = resolve_impact_target(&store, input).expect("symbol must resolve");
            assert_eq!(target.key, "helpers.rs::sanitize", "input: {input}");
            assert_eq!(
                target.file.as_deref(),
                Some("src/helpers.rs"),
                "both input forms must recover the same File.id; input: {input}"
            );
        }
    }

    /// Review round 3, finding 1. The exact match and the suffix fallback used
    /// to share one `WHERE f.id = $exact OR f.id ENDS WITH $suffix ORDER BY
    /// f.id LIMIT 1`, which let alphabetical order decide between them. Both
    /// `docs/guide.md` (the real target, an exact id) and `api/docs/guide.md`
    /// (unrelated, merely ending in the same segment) satisfy that WHERE, and
    /// `api/…` sorts first — so the caller silently got the wrong file's
    /// metadata. A wrong answer, not an error.
    ///
    /// This test fails on the pre-fix code, returning `api/docs/guide.md`.
    #[test]
    fn an_exact_match_beats_a_lexicographically_earlier_suffix_collision() {
        let (_dir, store) = fixture();
        for id in ["api/docs/guide.md", "zz/docs/guide.md"] {
            store
                .insert_node(
                    NODE_FILE,
                    &[
                        ("id", cypher_str(id).as_str()),
                        ("path", cypher_str(id).as_str()),
                        ("name", "'guide.md'"),
                        ("extension", "'md'"),
                        ("size_bytes", "1"),
                        ("parse_errors", "0"),
                    ],
                )
                .expect("insert file");
        }
        // `docs/guide.md` is itself a File.id (from `fixture`), and sorts
        // AFTER `api/docs/guide.md`.
        assert_eq!(
            resolve_file(&store, &["docs/guide.md"]).as_deref(),
            Some("docs/guide.md"),
            "an exact File.id must win over any suffix collision, whatever the order"
        );

        // With no exact match, the suffix fallback still answers, and still
        // does so deterministically (lowest id among the collisions).
        assert_eq!(
            resolve_file(&store, &["guide.md"]).as_deref(),
            Some("api/docs/guide.md"),
            "the fallback is unchanged when nothing matches exactly"
        );
    }

    #[test]
    fn an_unknown_target_still_reports_symbol_not_found() {
        let (_dir, store) = fixture();
        let err = resolve_impact_target(&store, "nowhere.rs::absent")
            .err()
            .expect("an unknown target must not resolve");
        assert_eq!(err.input, "nowhere.rs::absent");
    }

    #[test]
    fn the_suffix_match_is_anchored_on_a_path_segment() {
        // `helpers.rs` must not be recovered from a file merely ENDING in
        // those bytes, e.g. `src/my_helpers.rs`.
        let (_dir, store) = fixture();
        store
            .insert_node(
                NODE_FILE,
                &[
                    ("id", "'src/my_helpers.rs'"),
                    ("path", "'src/my_helpers.rs'"),
                    ("name", "'my_helpers.rs'"),
                    ("extension", "'rs'"),
                    ("size_bytes", "1"),
                    ("parse_errors", "0"),
                ],
            )
            .expect("insert file");
        assert_eq!(
            resolve_file(&store, &["helpers.rs"]).as_deref(),
            Some("src/helpers.rs")
        );
        assert_eq!(resolve_file(&store, &["elpers.rs"]), None);
    }
}
