// security_gates::gates — the five gate checks themselves (S1..S5).
//
// Split from `security_gates.rs`, which was over the §4.1 cap. The module's
// front door owns the report types, the orchestration that runs the gates over
// the resolved symbols, and the serialization; each gate's own rule — auth
// community membership, unsafe symbols, visibility, file scope, secrets — lives
// here. A new gate is a new function beside these plus one call in the
// orchestration, and touches neither the report shape nor the writer.

use super::{SecurityFlag, AUTH_CRITICAL_PATTERNS};
use crate::graph_store::{
    community_of as membership_community_of, cypher_str, GraphStore, SymbolMatch,
};
use serde_json::json;
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// S1 — auth-critical community touch
// ---------------------------------------------------------------------------

pub(super) fn find_auth_communities(store: &GraphStore) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    // One scan per symbol label that can be a community member. Keep it
    // simple: iterate patterns, iterate labels, collect c.id rows.
    let labels = [
        ("Function", "MemberOf_Function_Community"),
        ("Method", "MemberOf_Method_Community"),
        ("Struct", "MemberOf_Struct_Community"),
        ("Enum", "MemberOf_Enum_Community"),
        ("Trait", "MemberOf_Trait_Community"),
        ("Constant", "MemberOf_Constant_Community"),
        ("TypeAlias", "MemberOf_TypeAlias_Community"),
        ("Module", "MemberOf_Module_Community"),
    ];
    // source: stages/stage-8.md §4 S1 patterns are lowercase; we normalize
    // in-memory rather than rely on an engine-specific `toLower()` Cypher fn.
    for (label, rel) in labels {
        let cypher = format!("MATCH (s:{label})-[:{rel}]->(c:Community) RETURN s.name, c.id");
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if row.len() < 2 {
                    continue;
                }
                let name_lower = row[0].to_ascii_lowercase();
                if AUTH_CRITICAL_PATTERNS
                    .iter()
                    .any(|p| name_lower.contains(p))
                {
                    let cid = &row[1];
                    if !cid.is_empty() {
                        out.insert(cid.clone());
                    }
                }
            }
        }
    }
    out
}

pub(super) fn run_s1(
    store: &GraphStore,
    qualified_name: &str,
    auth_communities: &BTreeSet<String>,
    flags: &mut Vec<SecurityFlag>,
) {
    if auth_communities.is_empty() {
        return;
    }
    let cid = match community_of(store, qualified_name) {
        Some(c) => c,
        None => return,
    };
    if !auth_communities.contains(&cid) {
        return;
    }
    flags.push(SecurityFlag {
        gate: "auth_critical_touch".into(),
        severity: "critical".into(),
        symbol: qualified_name.into(),
        message: format!(
            "changed symbol shares community '{}' with auth-critical symbols",
            cid
        ),
        details: json!({ "community_id": cid, "auth_patterns": AUTH_CRITICAL_PATTERNS }),
    });
}

/// The community `qualified_name` belongs to, through the shared membership
/// traversal in `graph_store::membership`.
///
/// Per-label iteration rather than rel-type alternation is an lbug dialect
/// constraint, not a preference. The label order is `clustering::SYMBOL_LABELS`
/// and it is behaviour: this returns the first hit.
pub(super) fn community_of(store: &GraphStore, qualified_name: &str) -> Option<String> {
    crate::clustering::SYMBOL_LABELS
        .iter()
        .find_map(|label| {
            membership_community_of(store, label, SymbolMatch::QualifiedName(qualified_name))
        })
        .map(|c| c.id)
        .filter(|cid| !cid.is_empty())
}

// ---------------------------------------------------------------------------
// S2 — unsafe symbol (info-skip when parser lacks is_unsafe)
// ---------------------------------------------------------------------------
//
// source: stages/stage-8.md §4.2 + §7 Open Q1. The current Rust parser
// (src/parser/rust.rs) does NOT record an is_unsafe property on Function or
// Method nodes. Per the spec's graceful-degradation rule (Invariant I6), S2
// ships in INFO-SKIP mode: emit one info flag per changed symbol stating the
// detection is unavailable and skip the critical check. This preserves
// determinism and leaves a breadcrumb for the 3b-v2 parser roadmap.

pub(super) fn run_s2(_store: &GraphStore, qualified_name: &str, flags: &mut Vec<SecurityFlag>) {
    flags.push(SecurityFlag {
        gate: "unsafe_symbol".into(),
        severity: "info".into(),
        symbol: qualified_name.into(),
        message: "unsafe detection unavailable: the Rust parser does not record is_unsafe \
                  (see stages/stage-8.md §7 Q1; unblocks when stage 3a-v2 ships)"
            .into(),
        details: json!({ "skipped": true, "reason": "parser_missing_is_unsafe" }),
    });
}

// ---------------------------------------------------------------------------
// S3 — public API surface change
// ---------------------------------------------------------------------------

pub(super) fn run_s3(store: &GraphStore, qualified_name: &str, flags: &mut Vec<SecurityFlag>) {
    let meta = match symbol_visibility_and_parent(store, qualified_name) {
        Some(m) => m,
        None => return,
    };
    if meta.visibility.as_deref() != Some("pub") {
        return;
    }
    if !meta.parent_is_file {
        return;
    }
    // severity: warning on modify (default); caller supplies change_kind only
    // through the batch list. We can't distinguish remove/rename without it,
    // so this gate stays at "warning". Callers who want critical escalation
    // feed remove/rename through a richer change_kind in a follow-up.
    flags.push(SecurityFlag {
        gate: "public_api_change".into(),
        severity: "warning".into(),
        symbol: qualified_name.into(),
        message: "touches a crate-root public API symbol — downstream consumers may break".into(),
        details: json!({
            "visibility": "pub",
            "parent_label": meta.parent_label,
            "file_path": meta.file_path,
        }),
    });
}

struct SymbolMeta {
    visibility: Option<String>,
    parent_is_file: bool,
    parent_label: String,
    file_path: Option<String>,
}

fn symbol_visibility_and_parent(store: &GraphStore, qualified_name: &str) -> Option<SymbolMeta> {
    let escaped = cypher_str(qualified_name);
    // 1) pull the symbol's visibility (labels that carry it).
    let vis = fetch_visibility(store, &escaped)?;
    // 2) Is the symbol defined directly by a File (not via a Module)?
    let file_defined = has_file_parent(store, &escaped);
    let file_path = lookup_file_path(store, &escaped);
    Some(SymbolMeta {
        visibility: vis,
        parent_is_file: file_defined,
        parent_label: if file_defined {
            "File".into()
        } else {
            "Module".into()
        },
        file_path,
    })
}

fn fetch_visibility(store: &GraphStore, escaped_qn: &str) -> Option<Option<String>> {
    // Labels that carry `visibility` per graph_store.rs DDL: Function, Method,
    // Struct, Enum, Trait, Field. Method receiver_type is irrelevant here.
    for label in ["Function", "Method", "Struct", "Enum", "Trait"] {
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = {escaped_qn} \
             RETURN n.visibility LIMIT 1"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if let Some(v) = row.first() {
                    let s = v.trim();
                    return Some(if s.is_empty() {
                        None
                    } else {
                        Some(s.to_string())
                    });
                }
            }
        }
    }
    None
}

fn has_file_parent(store: &GraphStore, escaped_qn: &str) -> bool {
    // Defines_File_* edges go from File to the symbol; crate-root pubs live
    // directly under the File (no intermediate Module).
    for rel in [
        "Defines_File_Function",
        "Defines_File_Struct",
        "Defines_File_Enum",
        "Defines_File_Trait",
        "Defines_File_Constant",
        "Defines_File_TypeAlias",
    ] {
        let cypher = format!(
            "MATCH (f:File)-[:{rel}]->(n) WHERE n.qualified_name = {escaped_qn} \
             RETURN f.path LIMIT 1"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if qr.rows.first().and_then(|r| r.first()).is_some() {
                return true;
            }
        }
    }
    false
}

fn lookup_file_path(store: &GraphStore, escaped_qn: &str) -> Option<String> {
    for rel in [
        "Defines_File_Function",
        "Defines_File_Struct",
        "Defines_File_Enum",
        "Defines_File_Trait",
        "Defines_File_Constant",
        "Defines_File_TypeAlias",
    ] {
        let cypher = format!(
            "MATCH (f:File)-[:{rel}]->(n) WHERE n.qualified_name = {escaped_qn} \
             RETURN f.path LIMIT 1"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if let Some(p) = row.first() {
                    if !p.is_empty() {
                        return Some(p.clone());
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// S4 — unresolved import introduction
// ---------------------------------------------------------------------------

pub(super) fn run_s4(store: &GraphStore, qualified_name: &str, flags: &mut Vec<SecurityFlag>) {
    // An Import node survives post-resolution iff the resolver could not
    // rewrite it into a concrete edge (source: semantic_diff.rs
    // count_unresolved — count(:Import) is the canonical unresolved metric).
    // Since the schema has no File->Import edge, we scope by qualified_name
    // prefix: Import nodes live under the file's scope (parser/rust.rs §
    // handle_use_declaration — qualified_name = qual(scope, display_name)).
    let escaped = cypher_str(qualified_name);
    let file_path = match lookup_file_path(store, &escaped) {
        Some(p) => p,
        None => return,
    };
    // File path matches the scope prefix used by qual() in the parser. Strip
    // any leading path component the resolver removes (search::strip_leading
    // mirrors this), then match qualified_name prefix.
    let scope = file_scope_from_path(&file_path);
    let escaped_scope_prefix = cypher_str(&format!("{scope}::"));
    let cypher = format!(
        "MATCH (i:Import) WHERE i.qualified_name STARTS WITH {escaped_scope_prefix} \
         RETURN count(i)"
    );
    let count: u64 = match store.execute_query(&cypher) {
        Ok(qr) => qr
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0),
        Err(_) => 0,
    };
    if count == 0 {
        return;
    }
    let severity = if count >= 2 { "critical" } else { "warning" };
    flags.push(SecurityFlag {
        gate: "unresolved_imports".into(),
        severity: severity.into(),
        symbol: qualified_name.into(),
        message: format!(
            "{count} unresolved Import node(s) in the changed symbol's file — drift or supply-chain risk"
        ),
        details: json!({ "file_path": file_path, "scope": scope, "unresolved_count": count }),
    });
}

// Strips any leading path component so it matches the parser's qualified_name
// convention (`main.rs` rather than `src/main.rs`). Mirrors
// search::strip_leading_path_component.
fn file_scope_from_path(file_path: &str) -> String {
    match file_path.find('/') {
        Some(idx) => file_path[idx + 1..].to_string(),
        None => file_path.to_string(),
    }
}

// ---------------------------------------------------------------------------
// S5 — test coverage structural gap
// ---------------------------------------------------------------------------

pub(super) fn run_s5(store: &GraphStore, qualified_name: &str, flags: &mut Vec<SecurityFlag>) {
    let escaped = cypher_str(qualified_name);
    let mut reached = 0u64;
    for label in ["Function", "Method"] {
        let rel = format!("ParticipatesIn_{label}_Process");
        let cypher = format!(
            "MATCH (n:{label})-[:{rel}]->(p:Process) \
             WHERE n.qualified_name = {escaped} AND p.entry_kind = 'test' \
             RETURN count(p)"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if let Some(c) = row.first() {
                    reached += c.parse::<u64>().unwrap_or(0);
                }
            }
        }
    }
    if reached > 0 {
        return;
    }
    flags.push(SecurityFlag {
        gate: "test_coverage_gap".into(),
        severity: "warning".into(),
        symbol: qualified_name.into(),
        message: "no ParticipatesIn path from any test-entry process — structural coverage gap"
            .into(),
        details: json!({ "test_processes_reaching_symbol": 0 }),
    });
}
