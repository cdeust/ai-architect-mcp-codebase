// indexer::persist::nodes — node insertion from parsed results.
//
// Extracted from indexer/persist.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move: the node-property mapping concern (accumulate
// symbol nodes, PublicApi visibility gate, ExtractedNode→schema-column mapping)
// relocated verbatim. No behavior change.

use super::SymbolBatch;
use crate::graph_store::cypher_str;
use crate::parser;

// ---------------------------------------------------------------------------
// Node insertion from parsed results
// ---------------------------------------------------------------------------

pub(super) fn accumulate_parsed_nodes(
    batch: &mut SymbolBatch,
    nodes: &[parser::ExtractedNode],
    label_by_qn: &mut std::collections::HashMap<String, String>,
    seen_node_ids: &mut std::collections::HashSet<String>,
    language: &str,
    restrict_to_public_api: bool,
) {
    // Accumulate into the cross-file batch (flushed in large bulk calls).
    // source: Fermi audit — per-row CREATE was ~100x slower than batched;
    // the April 2026 scalability audit further found per-FILE batching still
    // dominated indexing time, so accumulation now spans files.
    //
    // Defensive dedup: parsers should produce unique ids per node, but a bug
    // there would abort the whole bulk flush (LadybugDB rejects duplicate
    // primary keys atomically), taking down every file in the batch, not one.
    // The id set is global to the run, so cross-file collisions are caught too.
    //
    // Enum qualified-names dropped by the PublicApi filter within THIS file's
    // node list. A Variant's own `visibility` is always "" — parsers never
    // declare it independently (source: src/parser/rust/extract/g2.rs:30) —
    // so a Variant is kept iff its parent Enum was kept. Scoped per-file
    // because `nodes` is one file's ExtractedNode list and parsers always
    // emit an Enum before its Variants within it.
    let mut dropped_enums: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for node in nodes {
        if restrict_to_public_api && !keep_under_public_api(node, language, &mut dropped_enums) {
            continue;
        }
        if !seen_node_ids.insert(node.qualified_name.clone()) {
            eprintln!(
                "indexer: dropped duplicate-id {} node '{}'",
                node.label, node.qualified_name
            );
            continue;
        }
        label_by_qn.insert(node.qualified_name.clone(), node.label.clone());
        let props = build_node_properties(node, language);
        batch.push_node(&node.label, props);
    }
}

/// PublicApi-tier gate: true iff `node` belongs on the dependency's public
/// API surface. Only applied to files under dependency directories — see
/// `restrict_to_public_api` at the call site.
/// source: ADR-4253701 §Decision 1 ("public_api": only visibility==public
/// symbols persisted from dependency files).
fn keep_under_public_api<'a>(
    node: &'a parser::ExtractedNode,
    language: &str,
    dropped_enums: &mut std::collections::HashSet<&'a str>,
) -> bool {
    if node.label == "Variant" {
        let Some((enum_qn, _)) = node.qualified_name.rsplit_once("::") else {
            return true;
        };
        return !dropped_enums.contains(enum_qn);
    }
    if !is_visibility_declaring_label(&node.label) {
        // Import, CallSite, Module, File, Directory: no declared-visibility
        // contract to filter on. Kept as-is — they are structural/navigation
        // nodes, not part of the "public API surface" the tier scopes.
        return true;
    }
    if is_public_symbol(language, &node.visibility) {
        true
    } else {
        if node.label == "Enum" {
            dropped_enums.insert(node.qualified_name.as_str());
        }
        false
    }
}

/// True for node labels whose `visibility` field is genuinely populated by
/// every parser via an explicit visibility/export check. `Variant` is
/// excluded — see `keep_under_public_api`.
/// source: src/parser/rust/extract/g2.rs,g3.rs (Function/Method/Struct/Enum/
/// Trait/Field/Constant/TypeAlias all call extract_visibility()) and
/// src/parser/typescript/extract/g1.rs:47,79 (export-keyword check).
fn is_visibility_declaring_label(label: &str) -> bool {
    matches!(
        label,
        "Function" | "Method" | "Struct" | "Enum" | "Trait" | "Field" | "Constant" | "TypeAlias"
    )
}

/// True when `visibility` denotes a publicly visible symbol for `language`.
///
/// Python's parser convention has the OPPOSITE polarity of every other
/// supported language: `python_visibility` (src/parser/python/mod.rs:105-116,
/// tested at lines 205-211) emits "" for a PUBLIC name and "private" for an
/// underscore-prefixed one. Rust/TypeScript/JVM/Go/Swift emit "" when no
/// visibility keyword is present (module-private by default) and a keyword
/// token ("pub"/"export"/"public"/"open") when the symbol is public.
/// Deliberately NOT reusing clustering::process::PUBLIC_VISIBILITY_VALUES:
/// that list's "public" entry for Python never matches python_visibility's
/// actual output ("" or "private"), which would silently exclude every
/// Python symbol from this filter — a Bug-5-class inconsistency this
/// function avoids rather than propagates.
/// source: src/parser/{rust,typescript}/mod.rs visibility tests
/// (rust/mod.rs:190 "pub", typescript/mod.rs:217 "pub"); python/mod.rs:205-211.
fn is_public_symbol(language: &str, visibility: &str) -> bool {
    if language == "python" {
        visibility != "private"
    } else {
        matches!(visibility, "pub" | "export" | "public" | "open")
    }
}

/// Builds the full property list for a node, mapping ExtractedNode fields
/// to the schema columns defined in graph_store.rs node_table_ddl().
///
/// source: Spike B' BUG #5 fix — `language` is appended for every
/// symbol-bearing label (anything that isn't File / Directory) so consumers
/// can filter by language without re-parsing.
fn build_node_properties(node: &parser::ExtractedNode, language: &str) -> Vec<(String, String)> {
    let mut props = vec![("id".to_string(), cypher_str(&node.qualified_name))];
    if has_name_col(&node.label) {
        props.push(("name".to_string(), cypher_str(&node.name)));
    }
    if has_qualified_name_col(&node.label) {
        props.push((
            "qualified_name".to_string(),
            cypher_str(&node.qualified_name),
        ));
    }
    if has_line_cols(&node.label) {
        props.push(("start_line".to_string(), node.start_line.to_string()));
        props.push(("end_line".to_string(), node.end_line.to_string()));
    }
    if has_visibility_col(&node.label) {
        props.push(("visibility".to_string(), cypher_str(&node.visibility)));
    }
    append_label_properties(&mut props, node);
    if has_language_col(&node.label) {
        props.push(("language".to_string(), cypher_str(language)));
    }
    props
}

/// True for every symbol-bearing node label (everything carrying source-code
/// semantics). File and Directory are excluded — they cross language boundaries.
fn has_language_col(label: &str) -> bool {
    matches!(
        label,
        "Function"
            | "Method"
            | "Struct"
            | "Enum"
            | "Variant"
            | "Trait"
            | "Field"
            | "Constant"
            | "TypeAlias"
            | "Import"
            | "CallSite"
    )
}

/// Maps parser extra properties to schema columns by label.
fn append_label_properties(props: &mut Vec<(String, String)>, node: &parser::ExtractedNode) {
    let find = |key: &str| -> String {
        node.properties
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    match node.label.as_str() {
        "Function" => {
            props.push(("is_async".to_string(), find("is_async")));
            // source: issue #92 — Uses-edge inputs; "" when the parser set none.
            props.push(("return_type".to_string(), cypher_str(&find("return_type"))));
            props.push((
                "constructed_types".to_string(),
                cypher_str(&find("constructed_types")),
            ));
        }
        "Method" => {
            props.push(("is_async".to_string(), find("is_async")));
            props.push((
                "receiver_type".to_string(),
                cypher_str(&find("receiver_type")),
            ));
            // source: implements fix — trait_name set by the parser on methods
            // inside `impl Trait for Type` blocks; resolve_implements reads it.
            props.push(("trait_name".to_string(), cypher_str(&find("trait_name"))));
            // source: issue #92 — Uses-edge inputs; "" when the parser set none.
            props.push(("return_type".to_string(), cypher_str(&find("return_type"))));
            props.push((
                "constructed_types".to_string(),
                cypher_str(&find("constructed_types")),
            ));
        }
        "Field" => {
            props.push((
                "type_annotation".to_string(),
                cypher_str(&find("type_annotation")),
            ));
        }
        "Constant" => {
            props.push((
                "type_annotation".to_string(),
                cypher_str(&find("type_annotation")),
            ));
        }
        "TypeAlias" => {
            props.push(("target_type".to_string(), cypher_str(&find("target_type"))));
        }
        // source: Spike B' BUG #9 — bases CSV emitted by parser/python.rs
        // for class/struct/trait/enum nodes; consumed by resolver.resolve_extends.
        // implements fix — `implements` CSV (derived/declared trait names) is
        // the parallel column consumed by resolver.resolve_implements.
        "Struct" | "Enum" | "Trait" => {
            props.push(("bases".to_string(), cypher_str(&find("bases"))));
            props.push(("implements".to_string(), cypher_str(&find("implements"))));
        }
        "Import" => {
            props.push(("path".to_string(), cypher_str(&find("path"))));
            props.push(("alias".to_string(), cypher_str(&find("alias"))));
            props.push(("is_glob".to_string(), find("is_glob")));
            // §10.1 span for the import statement; §10.4 is_resolved starts false
            // and is flipped by the resolver's resolve pass.
            props.push(("start_line".to_string(), node.start_line.to_string()));
            props.push(("end_line".to_string(), node.end_line.to_string()));
            props.push(("is_resolved".to_string(), "false".to_string()));
        }
        "CallSite" => {
            props.push(("callee_name".to_string(), cypher_str(&find("callee_name"))));
            props.push(("line".to_string(), node.start_line.to_string()));
            props.push(("col".to_string(), "0".to_string()));
            // §10.4 is_resolved starts false; the resolver flips it to true when
            // it emits the resolved Calls edge for this site.
            props.push(("is_resolved".to_string(), "false".to_string()));
        }
        _ => {}
    }
}

// Schema awareness — source: graph_store.rs node_table_ddl().
// Each function returns true iff the label's CREATE NODE TABLE includes that column.

fn has_name_col(label: &str) -> bool {
    // All node tables have `name` EXCEPT Import (path/alias only).
    // CallSite stores callee_name via properties, not via 'name' column.
    !matches!(label, "Import" | "CallSite")
}

fn has_qualified_name_col(label: &str) -> bool {
    matches!(
        label,
        "Module"
            | "Function"
            | "Method"
            | "Struct"
            | "Enum"
            | "Variant"
            | "Trait"
            | "Constant"
            | "TypeAlias"
    )
}

fn has_line_cols(label: &str) -> bool {
    // source: stages/stage-3.md §10.1 — every symbol carries its span. Variant,
    // Field, Constant and TypeAlias now have span columns too (Import keeps its
    // span via append_label_properties, alongside path/alias). CallSite records
    // position via its own line/col columns, not start_line/end_line.
    matches!(
        label,
        "Function"
            | "Method"
            | "Struct"
            | "Enum"
            | "Trait"
            | "Variant"
            | "Field"
            | "Constant"
            | "TypeAlias"
    )
}

fn has_visibility_col(label: &str) -> bool {
    matches!(
        label,
        "Function" | "Method" | "Struct" | "Enum" | "Trait" | "Field"
    )
}
