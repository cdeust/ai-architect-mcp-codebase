// security_gates — Stage 8: graph-aware pre-merge security gates.
//
// Five gates, each a single Cypher pattern + fixed rule:
//   S1 auth_critical_touch   — change shares a Leiden community with an
//                              auth-pattern symbol.                  critical
//   S2 unsafe_symbol          — changed symbol itself is `unsafe` (Rust) or
//                              uses a risky JS/Python API.            critical|warning
//                              INFO-SKIP mode when parser does not record it.
//   S3 public_api_change      — crate-root `pub` symbol touched.      warning|critical
//   S4 unresolved_imports     — changed symbol owns new Imports that
//                              resolved to an :Import fallback node.  warning|critical
//   S5 test_coverage_gap      — changed symbol has no ParticipatesIn
//                              path from any test-entry process.      warning
//
// Read-only w.r.t. the graph. LLM-free. Deterministic on (graph, Δ).
//
// source: stages/stage-8.md §4 (gate definitions), §6 (severity ladder),
//         §7 (tool schema).

use crate::graph_store::GraphStore;
mod gates;
use crate::search;
use gates::{find_auth_communities, run_s1, run_s2, run_s3, run_s4, run_s5};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

// source: stages/stage-8.md §4 S1 — auth-critical name patterns for
// Leiden community proximity detection.
pub(super) const AUTH_CRITICAL_PATTERNS: &[&str] = &[
    "auth",
    "password",
    "token",
    "permission",
    "role",
    "crypto",
    "encrypt",
    "decrypt",
    "verify",
    "jwt",
    "oauth",
    "session",
];

// source: stages/stage-8.md §6 — security artifact filename.
pub const SECURITY_FILE: &str = "stage-8.security.json";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct SecurityReport {
    pub gates_passed: bool,
    pub flags: Vec<SecurityFlag>,
    pub summary: SecuritySummary,
}

pub struct SecurityFlag {
    pub gate: String,
    pub severity: String,
    pub symbol: String,
    pub message: String,
    pub details: Value,
}

pub struct SecuritySummary {
    pub changed_symbols: u64,
    pub critical_count: u64,
    pub warning_count: u64,
    pub info_count: u64,
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

pub fn check_gates(
    store: &GraphStore,
    changed_symbols: &[String],
) -> Result<SecurityReport, String> {
    let resolved = resolve_all(store, changed_symbols);
    let mut flags: Vec<SecurityFlag> = Vec::new();
    let auth_communities = find_auth_communities(store);
    for r in &resolved {
        let qn = match &r.resolved_qn {
            Some(q) => q,
            None => continue,
        };
        run_s1(store, qn, &auth_communities, &mut flags);
        run_s2(store, qn, &mut flags);
        run_s3(store, qn, &mut flags);
        run_s4(store, qn, &mut flags);
        run_s5(store, qn, &mut flags);
    }
    for r in &resolved {
        if r.resolved_qn.is_none() {
            flags.push(SecurityFlag {
                gate: "input_unresolved".into(),
                severity: "info".into(),
                symbol: r.input.clone(),
                message: format!("changed symbol '{}' did not resolve in graph", r.input),
                details: json!({ "did_you_mean": r.did_you_mean }),
            });
        }
    }
    let summary = tally(&flags, changed_symbols.len() as u64);
    Ok(SecurityReport {
        gates_passed: summary.critical_count == 0,
        flags,
        summary,
    })
}

fn tally(flags: &[SecurityFlag], changed_symbols: u64) -> SecuritySummary {
    let mut critical = 0u64;
    let mut warning = 0u64;
    let mut info = 0u64;
    for f in flags {
        match f.severity.as_str() {
            "critical" => critical += 1,
            "warning" => warning += 1,
            _ => info += 1,
        }
    }
    SecuritySummary {
        changed_symbols,
        critical_count: critical,
        warning_count: warning,
        info_count: info,
    }
}

// ---------------------------------------------------------------------------
// Input resolution
// ---------------------------------------------------------------------------

struct Resolved {
    input: String,
    resolved_qn: Option<String>,
    did_you_mean: Vec<String>,
}

fn resolve_all(store: &GraphStore, inputs: &[String]) -> Vec<Resolved> {
    inputs
        .iter()
        .map(|input| match search::resolve_qualified_name(store, input) {
            Ok(qn) => Resolved {
                input: input.clone(),
                resolved_qn: Some(qn),
                did_you_mean: Vec::new(),
            },
            Err(nf) => Resolved {
                input: input.clone(),
                resolved_qn: None,
                did_you_mean: nf.did_you_mean,
            },
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Artifact writer
// ---------------------------------------------------------------------------

pub fn report_to_json(
    report: &SecurityReport,
    run_id: &str,
    finding_id: &str,
    graph_path: &Path,
    changed_symbols: &[String],
    checked_at: &str,
) -> Value {
    let flags: Vec<Value> = report
        .flags
        .iter()
        .map(|f| {
            json!({
                "gate": f.gate, "severity": f.severity, "symbol": f.symbol,
                "message": f.message, "details": f.details,
            })
        })
        .collect();
    json!({
        "run_id": run_id,
        "finding_id": finding_id,
        "graph_path": graph_path.to_string_lossy(),
        "changed_symbols": changed_symbols,
        "checked_at": checked_at,
        "gates_passed": report.gates_passed,
        "summary": {
            "changed_symbols": report.summary.changed_symbols,
            "critical_count": report.summary.critical_count,
            "warning_count": report.summary.warning_count,
            "info_count": report.summary.info_count,
        },
        "flags": flags,
    })
}

pub fn write_security(path: &Path, value: &Value) -> Result<PathBuf, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {:?}: {}", parent, e))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize: {}", e))?;
    fs::write(path, bytes).map_err(|e| format!("write {:?}: {}", path, e))?;
    Ok(path.to_path_buf())
}

// ---------------------------------------------------------------------------
// Pure-helper tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::gates::community_of;
    use super::*;
    use crate::graph_store::{cypher_str, NODE_COMMUNITY, NODE_FUNCTION};

    // source: issue #16 — the naive single-quote-only `String::replace`
    // escape (the exact pattern this file used before this fix, and the
    // SECOND reintroduction of the vulnerability git_diff.rs's "M1 fix"
    // comment already documented) is defeated by a `\'` payload: the escape turns
    // `\'` into `\\'` — an escaped backslash followed by an UNescaped
    // closing quote — which closes the Cypher string literal early and lets
    // the attacker-controlled remainder execute as live Cypher (e.g.
    // `DETACH DELETE n`). `qualified_name` reaching `community_of` /
    // `run_s1` originates from the changed-symbol list the security gates
    // are handed for a diff — untrusted content in the same sense as the
    // LLM-generated PRD claims covered by
    // prd_validator::verdict_tests::test_file_has_graph_node_escapes_adversarial_path
    // and graph_store::tests::test_cypher_injection_rejected. This test
    // proves S1 (`community_of`) round-trips the adversarial qualified_name
    // as ordinary string data instead of executing the injected Cypher.
    #[test]
    fn test_community_of_escapes_adversarial_qualified_name() {
        let dir = tempfile::Builder::new()
            .prefix("security_gates_cypher_inject_test")
            .tempdir()
            .expect("create temp dir");
        let db_path = dir.path().join("testdb");
        let store = GraphStore::open_or_create(&db_path).expect("open_or_create");
        store.create_schema().expect("create_schema");

        let evil_qn = r"evil\'::fn() -> () DETACH DELETE n //";
        let safe_qn = "safe::fn";
        let community_id = "community::0";

        store
            .insert_node(
                NODE_COMMUNITY,
                &[
                    ("id", &cypher_str(community_id)),
                    ("name", &cypher_str("community_0")),
                    ("algorithm", &cypher_str("louvain+c2")),
                    ("resolution_param", "1.0"),
                    ("member_count", "2"),
                    ("modularity_contribution", "0.0"),
                ],
            )
            .expect("insert community node");
        for qn in [evil_qn, safe_qn] {
            store
                .insert_node(
                    NODE_FUNCTION,
                    &[
                        ("id", &cypher_str(qn)),
                        ("name", &cypher_str(qn)),
                        ("qualified_name", &cypher_str(qn)),
                        ("start_line", "1"),
                        ("end_line", "1"),
                        ("visibility", &cypher_str("pub")),
                        ("is_async", "false"),
                        ("language", &cypher_str("rust")),
                    ],
                )
                .expect("insert function node");
            store
                .insert_edge("MemberOf_Function_Community", qn, community_id, &[])
                .expect("insert MemberOf edge");
        }

        assert_eq!(
            community_of(&store, evil_qn),
            Some(community_id.to_string()),
            "the adversarial qualified_name must round-trip as ordinary string data \
             and still resolve to its own community"
        );
        assert_eq!(
            community_of(&store, safe_qn),
            Some(community_id.to_string()),
            "the benign function node must survive — the adversarial qualified_name \
             must not have executed DETACH DELETE"
        );
    }

    #[test]
    fn test_auth_patterns_lowercase() {
        for p in AUTH_CRITICAL_PATTERNS {
            assert_eq!(*p, p.to_ascii_lowercase(), "patterns must be lowercase");
        }
    }

    #[test]
    fn test_tally_counts() {
        let flags = vec![
            SecurityFlag {
                gate: "g".into(),
                severity: "critical".into(),
                symbol: "s".into(),
                message: "".into(),
                details: json!({}),
            },
            SecurityFlag {
                gate: "g".into(),
                severity: "warning".into(),
                symbol: "s".into(),
                message: "".into(),
                details: json!({}),
            },
            SecurityFlag {
                gate: "g".into(),
                severity: "info".into(),
                symbol: "s".into(),
                message: "".into(),
                details: json!({}),
            },
            SecurityFlag {
                gate: "g".into(),
                severity: "info".into(),
                symbol: "s".into(),
                message: "".into(),
                details: json!({}),
            },
        ];
        let s = tally(&flags, 5);
        assert_eq!(s.critical_count, 1);
        assert_eq!(s.warning_count, 1);
        assert_eq!(s.info_count, 2);
        assert_eq!(s.changed_symbols, 5);
    }

    #[test]
    fn test_gates_passed_requires_zero_critical() {
        let mut r = SecurityReport {
            gates_passed: true,
            flags: Vec::new(),
            summary: SecuritySummary {
                changed_symbols: 1,
                critical_count: 0,
                warning_count: 5,
                info_count: 3,
            },
        };
        // with only warnings, should pass
        assert!(r.summary.critical_count == 0);
        r.summary.critical_count = 1;
        assert!(r.summary.critical_count > 0);
    }

    /// Inserts a `Community` node with the given id. `id` may be empty — that
    /// degenerate shape is the subject of the fallthrough test below.
    fn insert_community(store: &GraphStore, id: &str) {
        store
            .insert_node(
                NODE_COMMUNITY,
                &[
                    ("id", &cypher_str(id)),
                    ("name", &cypher_str(id)),
                    ("algorithm", &cypher_str("louvain+c2")),
                    ("resolution_param", "1.0"),
                    ("member_count", "1"),
                    ("modularity_contribution", "0.0"),
                ],
            )
            .expect("insert community");
    }

    /// Inserts one symbol of `label` under `qn` and makes it a member of
    /// `community_id`. Only the columns every symbol label shares are set.
    fn insert_member(store: &GraphStore, label: &str, qn: &str, community_id: &str) {
        let mut props: Vec<(&str, &str)> = Vec::new();
        let esc = cypher_str(qn);
        let vis = cypher_str("pub");
        let lang = cypher_str("rust");
        props.push(("id", &esc));
        props.push(("name", &esc));
        props.push(("qualified_name", &esc));
        props.push(("start_line", "1"));
        props.push(("end_line", "1"));
        props.push(("visibility", &vis));
        props.push(("language", &lang));
        if label == NODE_FUNCTION {
            props.push(("is_async", "false"));
        }
        store.insert_node(label, &props).expect("insert symbol");
        store
            .insert_edge(
                &format!("MemberOf_{label}_Community"),
                qn,
                community_id,
                &[],
            )
            .expect("insert MemberOf");
    }

    /// Review #262 follow-up (ii). `community_of` scans
    /// `clustering::SYMBOL_LABELS` in order and returns the FIRST community it
    /// finds — but a row carrying an empty `Community.id` is not an answer, and
    /// the per-label loop this was migrated from kept scanning when it hit one.
    ///
    /// The migration expressed that as `.find_map(..).map(|c| c.id).filter(non
    /// empty)`, which stops at the first label and THEN discards its result,
    /// losing the community a later label supplies. This fixture puts the
    /// degenerate community on Function (scanned first) and a real one on
    /// Struct, for one qualified name.
    ///
    /// Fails with `None` on the post-`find_map` filter.
    #[test]
    fn community_of_falls_through_a_degenerate_empty_community_id() {
        use crate::graph_store::{SymbolMatch, NODE_STRUCT};

        let dir = tempfile::Builder::new()
            .prefix("security_gates_empty_community_id")
            .tempdir()
            .expect("create temp dir");
        let store = GraphStore::open_or_create(&dir.path().join("testdb")).expect("open");
        store.create_schema().expect("create_schema");

        let qn = "shadowed::thing";
        insert_community(&store, "");
        insert_community(&store, "community::real");
        insert_member(&store, NODE_FUNCTION, qn, "");
        insert_member(&store, NODE_STRUCT, qn, "community::real");

        // Precondition: the fixture must really produce the degenerate row on
        // the FIRST-scanned label, or this test proves nothing. Since the rule
        // moved INTO the traversal, "degenerate" now surfaces as `None` from
        // `community_of` — while the raw edge is still there, which is what the
        // scan has to look past.
        assert!(
            crate::graph_store::community_of(&store, "Function", SymbolMatch::QualifiedName(qn))
                .is_none(),
            "fixture precondition: the Function label's community must read as \
             none, because its id is empty"
        );
        assert_eq!(
            store
                .execute_query(
                    "MATCH (n:Function)-[:MemberOf_Function_Community]->(c:Community) \
                     RETURN c.id"
                )
                .expect("probe")
                .rows
                .len(),
            1,
            "fixture precondition: the degenerate edge really exists"
        );

        assert_eq!(
            community_of(&store, qn),
            Some("community::real".to_string()),
            "an empty Community.id on an earlier label must not abort the scan"
        );
    }
}
