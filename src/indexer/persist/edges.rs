// indexer::persist::edges — edge insertion + multi-table resolution.
//
// Extracted from indexer/persist.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move: the edge-accumulation concern (accumulate
// parsed edges, structural provenance, in-memory multi-table name resolution)
// relocated verbatim. No behavior change.

use super::SymbolBatch;
use crate::graph_store::cypher_str;
use crate::parser;
use std::collections::{HashMap, HashSet};

pub(super) fn accumulate_parsed_edges(
    batch: &mut SymbolBatch,
    refs: &[parser::ExtractedRef],
    label_by_qn: &HashMap<String, HashSet<String>>,
) {
    // source: Fermi audit — per-edge probe_node_label was firing up to 9
    // MATCH queries; the in-memory label_by_qn map eliminates them entirely.
    // Edges accumulate into the cross-file batch and flush in large bulk calls.
    for edge_ref in refs {
        let table = resolve_edge_table(
            &edge_ref.kind,
            &edge_ref.from_qualified_name,
            &edge_ref.to_qualified_name,
            label_by_qn,
        );
        let table_name = match table {
            Some(t) => t,
            None => continue,
        };
        let props = structural_provenance_props(&table_name);
        batch.push_edge(
            &table_name,
            edge_ref.from_qualified_name.clone(),
            edge_ref.to_qualified_name.clone(),
            props,
        );
    }
}

/// Structural edges (Defines_*, HasMethod_*, HasField_*, HasVariant_*) are
/// ground-truth AST facts; they carry (confidence=1.0, "direct-ast") so
/// downstream consumers see uniform provenance across structural and
/// resolution edges. Resolution edges get their provenance from the resolver.
/// source: Spike B' BUG #4 — see graph_store::is_structural_provenance_rel.
fn structural_provenance_props(table_name: &str) -> Vec<(String, String)> {
    if table_name.starts_with("Defines_")
        || table_name.starts_with("HasMethod_")
        || table_name.starts_with("HasField_")
        || table_name.starts_with("HasVariant_")
    {
        vec![
            ("confidence".to_string(), "1.0".to_string()),
            ("resolution_method".to_string(), cypher_str("direct-ast")),
        ]
    } else {
        Vec::new()
    }
}

/// Resolves the multi-table edge name using the in-memory label map.
/// This eliminates the per-edge Cypher probes that used to dominate
/// indexing cost on large codebases.
fn resolve_edge_table(
    kind: &str,
    from_qn: &str,
    to_qn: &str,
    label_by_qn: &HashMap<String, HashSet<String>>,
) -> Option<String> {
    match kind {
        "Defines" => resolve_defines_table(from_qn, to_qn, label_by_qn),
        "HasMethod" => resolve_has_method_table(from_qn, label_by_qn),
        "HasField" => resolve_has_field_table(from_qn, label_by_qn),
        "HasVariant" => Some("HasVariant_Enum_Variant".to_string()),
        // 3b: Extends refs are deferred to the resolver pass — skip here.
        "Extends" => None,
        _ => None,
    }
}

fn resolve_defines_table(
    from_qn: &str,
    to_qn: &str,
    label_by_qn: &HashMap<String, HashSet<String>>,
) -> Option<String> {
    // source: Spike B' BUG #12 fix — added Function/Method to from-candidates
    // and CallSite to to-candidates. Previously the parser-emitted
    // `Defines: Function → CallSite` edges were silently dropped here because
    // the whitelist excluded both endpoints. CallSite nodes were orphans.
    let from_label = lookup_label_among(
        from_qn,
        label_by_qn,
        &["File", "Module", "Function", "Method"],
    )?;
    let to_candidates = &[
        "Function",
        "Struct",
        "Enum",
        "Trait",
        "Constant",
        "TypeAlias",
        "Module",
        "Import",
        "CallSite",
    ];
    let to_label = lookup_label_among(to_qn, label_by_qn, to_candidates)?;
    let table = format!("Defines_{from_label}_{to_label}");
    if is_valid_rel_table(&table) {
        Some(table)
    } else {
        None
    }
}

fn resolve_has_method_table(
    from_qn: &str,
    label_by_qn: &HashMap<String, HashSet<String>>,
) -> Option<String> {
    let from_label = lookup_label_among(from_qn, label_by_qn, &["Struct", "Enum", "Trait"])?;
    let table = format!("HasMethod_{from_label}_Method");
    if is_valid_rel_table(&table) {
        Some(table)
    } else {
        None
    }
}

fn resolve_has_field_table(
    from_qn: &str,
    label_by_qn: &HashMap<String, HashSet<String>>,
) -> Option<String> {
    let from_label = lookup_label_among(from_qn, label_by_qn, &["Struct", "Enum"])?;
    let table = format!("HasField_{from_label}_Field");
    if is_valid_rel_table(&table) {
        Some(table)
    } else {
        None
    }
}

/// Looks up the known label for an id among the candidates allowed for the
/// edge kind being resolved. No DB access.
///
/// `label_by_qn` records EVERY label ever observed for a qualified_name, not
/// just one: Rust's namespace rules let two distinct, real declarations
/// legally share one qualified_name string — e.g. `mod foo {}` (type
/// namespace) and `fn foo() {}` (value namespace) at the same scope, both
/// producing "src/lib.rs::foo" (verified 2026-09-03 by parsing that exact
/// snippet through `parser::parse_file(RUST_SPEC, ...)`: one Module node,
/// one Function node, and two Defines refs with byte-identical from/to
/// qualified_names — the parser's ExtractedRef carries only the qn string,
/// so nothing at this layer can tell which of the two structural facts a
/// given ref refers to).
///
/// A single stored label would non-deterministically resolve BOTH refs to
/// whichever label was written last: routed into the wrong (but schema-
/// valid) rel table when it was, e.g. "Function", or silently dropped when
/// it was "Module" (no `Defines_*_Module` table exists) — a wrong-but-valid
/// answer is worse than a dropped one, because nothing downstream can tell
/// it apart from a correct resolution.
///
/// Exactly one recorded label intersecting `candidates` resolves normally.
/// Two or more is genuine, unresolvable ambiguity — mirrors
/// `ambiguity_policy`'s "drop rather than guess" policy (see that module's
/// doc comment and `resolver::calls::resolve_single_call`'s handling of
/// `PolicyResolution::Ambiguous`): both structural refs are left unrouted
/// (`None`) rather than one being arbitrarily chosen. This is deterministic
/// and insertion-order-independent — the recorded-labels set carries no
/// order, so which of the two source nodes was parsed first cannot affect
/// the outcome.
fn lookup_label_among(
    id: &str,
    label_by_qn: &HashMap<String, HashSet<String>>,
    candidates: &[&str],
) -> Option<String> {
    let labels = label_by_qn.get(id)?;
    let mut matches = candidates.iter().filter(|c| labels.contains(**c));
    let first = matches.next()?;
    if matches.next().is_some() {
        eprintln!(
            "indexer: ambiguous label for '{id}' among candidates {candidates:?} \
             (recorded labels: {labels:?}) — edge left unrouted"
        );
        return None;
    }
    Some((*first).to_string())
}

/// Checks if a rel table name exists in the known schema. Single source
/// of truth is `graph_store::REL_TABLES`; this thin shim avoids drift.
fn is_valid_rel_table(name: &str) -> bool {
    crate::graph_store::is_known_rel_table(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::persist::nodes::accumulate_parsed_nodes;

    fn node(label: &str, qualified_name: &str) -> parser::ExtractedNode {
        parser::ExtractedNode {
            label: label.to_string(),
            name: qualified_name.rsplit("::").next().unwrap_or("").to_string(),
            qualified_name: qualified_name.to_string(),
            start_line: 1,
            end_line: 1,
            visibility: "pub".to_string(),
            properties: Vec::new(),
        }
    }

    /// Builds `label_by_qn` by running `nodes` through the real accumulation
    /// path (not a hand-built map), in the exact order given — the same path
    /// production indexing uses.
    fn label_by_qn_from(nodes: &[parser::ExtractedNode]) -> HashMap<String, HashSet<String>> {
        let mut batch = SymbolBatch::default();
        let mut label_by_qn = HashMap::new();
        // (label, qualified_name)-keyed dedup (#269) — every node below has a
        // distinct (label, qualified_name) pair, so this never fires; still
        // required by accumulate_parsed_nodes's signature.
        let mut seen_ids: std::collections::HashSet<(String, String)> = HashSet::new();
        accumulate_parsed_nodes(
            &mut batch,
            nodes,
            &mut label_by_qn,
            &mut seen_ids,
            "rust",
            false,
        );
        label_by_qn
    }

    /// Regression test for the label-blind `label_by_qn` bug (the #269
    /// companion this PR closes): a `Struct` and a `Function` sharing one
    /// qualified_name is legal Rust — `struct Foo { .. }` occupies the type
    /// namespace, `fn Foo() { .. }` occupies the value namespace, so both can
    /// coexist at the same scope (verified 2026-09-03 via
    /// `parser::parse_file` on `struct Foo { x: i32 } impl Foo { fn
    /// method(&self) {} } fn Foo() {}`: one Struct node, one Function node,
    /// both qualified_name "src/lib.rs::Foo", plus a real HasMethod ref from
    /// that same qn to the Struct's method).
    ///
    /// `resolve_has_method_table`'s candidate set is `["Struct", "Enum",
    /// "Trait"]` — "Function" is not a candidate, so recording BOTH labels
    /// and intersecting with candidates must resolve to "Struct" regardless
    /// of which node the accumulation loop reaches first. Pre-fix (single
    /// `String` value, last-write-wins), whichever node was parsed LAST
    /// decided the outcome: if Function overwrote Struct, the real
    /// `HasMethod` edge for `Foo::method` was silently dropped even though
    /// "Struct" was the correct, unambiguous answer.
    #[test]
    fn struct_fn_namespace_collision_resolves_has_method_regardless_of_order() {
        let struct_first = [
            node("Struct", "src/lib.rs::Foo"),
            node("Function", "src/lib.rs::Foo"),
        ];
        let fn_first = [
            node("Function", "src/lib.rs::Foo"),
            node("Struct", "src/lib.rs::Foo"),
        ];

        let map_a = label_by_qn_from(&struct_first);
        let map_b = label_by_qn_from(&fn_first);

        let table_a = resolve_has_method_table("src/lib.rs::Foo", &map_a);
        let table_b = resolve_has_method_table("src/lib.rs::Foo", &map_b);

        assert_eq!(
            table_a,
            Some("HasMethod_Struct_Method".to_string()),
            "Struct-first insertion order must resolve to the Struct's HasMethod table"
        );
        assert_eq!(
            table_b,
            Some("HasMethod_Struct_Method".to_string()),
            "Function-first insertion order must resolve identically — the \
             unrelated Function sharing this qualified_name must not shadow \
             the Struct label"
        );
    }

    /// Companion test: a `Module` and a `Function` sharing one
    /// qualified_name is ALSO legal Rust (`mod foo {}` is type-namespace,
    /// `fn foo() {}` is value-namespace — verified 2026-09-03 via
    /// `parser::parse_file` on `mod foo { pub fn inner() {} } fn foo() {}`:
    /// one Module node and one Function node, both qualified_name
    /// "src/lib.rs::foo", plus two structurally-identical `Defines` refs
    /// (byte-identical from_qn/to_qn) — one for each declaration). Unlike
    /// the Struct/Function case above, BOTH "Module" and "Function" are
    /// valid `to_candidates` for `resolve_defines_table`, so there is no
    /// single correct answer: the `ExtractedRef` carries only the qn string,
    /// not which declaration it names. Per `ambiguity_policy`'s "drop rather
    /// than guess" philosophy, this must resolve to `None` — never an
    /// arbitrary pick — regardless of insertion order.
    #[test]
    fn module_fn_namespace_collision_is_genuinely_ambiguous_regardless_of_order() {
        let module_first = [
            node("Module", "src/lib.rs::foo"),
            node("Function", "src/lib.rs::foo"),
        ];
        let fn_first = [
            node("Function", "src/lib.rs::foo"),
            node("Module", "src/lib.rs::foo"),
        ];

        let mut map_a = label_by_qn_from(&module_first);
        let mut map_b = label_by_qn_from(&fn_first);
        // Defines' from-side needs a valid container label; use File, as the
        // real crate-root scope would have.
        map_a
            .entry("src/lib.rs".to_string())
            .or_default()
            .insert("File".to_string());
        map_b
            .entry("src/lib.rs".to_string())
            .or_default()
            .insert("File".to_string());

        let table_a = resolve_defines_table("src/lib.rs", "src/lib.rs::foo", &map_a);
        let table_b = resolve_defines_table("src/lib.rs", "src/lib.rs::foo", &map_b);

        assert_eq!(
            table_a, None,
            "genuine ambiguity must drop, not guess (Module-first order)"
        );
        assert_eq!(
            table_b, None,
            "genuine ambiguity must drop, not guess (Function-first order)"
        );
    }
}
