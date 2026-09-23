// resolver_layers — Layer 4 (macro expansion) + Layer 5 (stdlib index)
// passes for the stage-3b-v2 resolver. Lives separate from resolver.rs so
// that Q8 (symbols-in-file) ground truth for resolver.rs remains stable as
// new passes are added. source: stages/stage-3b-v2.md §5.

use crate::graph_store::{call_site_rel_table, cypher_str, GraphStore, NODE_STDLIB_SYMBOL};
use crate::resolver::{PhaseResult, UnresolvedRef};
use std::collections::HashSet;

// source: stages/stage-3b-v2.md §5 Layer 4 — rule-based macro expansion is
// stored at confidence 0.85 with method "macro-expansion".
const MACRO_CONFIDENCE: f64 = 0.85;
const MACRO_METHOD: &str = "macro-expansion";

/// Entry point for Layer 4 (macros + derives).
/// postcondition: returns the same `(resolved, total, unresolved)` shape as
/// every resolver.rs phase, so its counts fold cleanly into `total_refs`.
/// source: issue #28 — previously returned only `resolved` (a raw edge
/// count folded into the numerator with no matching denominator
/// contribution), which let `resolution_rate` exceed 1.0.
pub fn run_macro_expansion(
    store: &GraphStore,
    buf: &mut crate::resolver::EdgeBuffer,
    caller_label_of: &dyn Fn(&str) -> String,
) -> PhaseResult {
    let mut created: HashSet<String> = HashSet::new();
    expand_macro_calls(store, buf, caller_label_of, &mut created)
}

/// Resolves each legacy macro-marker CallSite (`callee_name` ending in
/// `!`) to its expansion table entry, then to one Calls_*_StdlibSymbol
/// edge per `emit_calls` entry.
///
/// Granularity: one macro invocation is one syntactic reference, but its
/// resolution fans out into N edges (one per `emit_calls` entry) — mirrors
/// resolver::resolve_field_type_uses, where the denominator matches the
/// numerator's granularity rather than the row's. A macro invocation with
/// zero attemptable emissions (unknown macro name, non-callable caller, or
/// an expansion with an empty `emit_calls`) contributes exactly 1 unresolved
/// unit; a macro invocation with N emissions contributes N total units,
/// split resolved/unresolved per the same rules other phases use
/// (unknown-rel-table drops are unresolved, not silently skipped).
fn expand_macro_calls(
    store: &GraphStore,
    buf: &mut crate::resolver::EdgeBuffer,
    caller_label_of: &dyn Fn(&str) -> String,
    created: &mut HashSet<String>,
) -> PhaseResult {
    // The parser emits synthetic ExtractedRefs with kind="CallsMacro" that
    // the indexer drops (no matching rel-table). Re-reading is impossible
    // without another parse, so Layer 4 reads CallSite-less fallback: any
    // Function / Method whose body contains a `name!(...)` invocation is
    // represented in the graph as a CallSite only when the file has been
    // parsed by this binary. For files parsed under the new extractor, the
    // CallsMacro refs are dropped — so we rely on the post-parse resolver
    // pass looking at the raw `CallSite` nodes via a fast path: any
    // `callee_name` ending with `!` is a macro marker the parser emitted
    // BEFORE the CallsMacro rewrite (legacy). The new extractor does not
    // emit CallSite nodes for macros; therefore this pass currently covers
    // the legacy path only. When the indexer learns CallsMacro, the full
    // Layer 4 will light up.
    let qr = store.execute_query("MATCH (cs:CallSite) RETURN cs.id, cs.callee_name")?;
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();
    for row in &qr.rows {
        if row.len() < 2 {
            continue;
        }
        let cs_id = &row[0];
        let callee = &row[1];
        let macro_name = match callee.strip_suffix('!') {
            Some(n) => n,
            None => continue,
        };
        let (r, t, u) =
            resolve_one_macro_call_site(store, buf, caller_label_of, created, cs_id, macro_name)?;
        resolved += r;
        total += t;
        unresolved.extend(u);
    }
    Ok((resolved, total, unresolved))
}

fn resolve_one_macro_call_site(
    store: &GraphStore,
    buf: &mut crate::resolver::EdgeBuffer,
    caller_label_of: &dyn Fn(&str) -> String,
    created: &mut HashSet<String>,
    cs_id: &str,
    macro_name: &str,
) -> PhaseResult {
    let one_unresolved = |reason: &str| {
        (
            0,
            1,
            vec![UnresolvedRef {
                kind: "Calls".to_string(),
                from_id: cs_id.to_string(),
                target_text: format!("{macro_name}!"),
                reason: reason.to_string(),
            }],
        )
    };
    let expansion = match crate::macro_expansion::lookup("rust", macro_name) {
        Some(e) => e,
        None => return Ok(one_unresolved("no macro-expansion table entry")),
    };
    let caller_qn = caller_from_callsite(cs_id);
    let caller_label = caller_label_of(&caller_qn);
    // source: stages/stage-3b.md §2 — Calls_*_StdlibSymbol is defined for
    // Function|Method callers only.
    if caller_label != "Function" && caller_label != "Method" {
        return Ok(one_unresolved("caller is not a callable (Function|Method)"));
    }
    if expansion.emit_calls.is_empty() {
        return Ok(one_unresolved("expansion has no emit_calls entries"));
    }
    let (mut resolved, mut unresolved) = (0u64, Vec::new());
    let total = expansion.emit_calls.len() as u64;
    let rel = format!("Calls_{caller_label}_StdlibSymbol");
    let site = MacroSite {
        cs_id,
        caller_qn: &caller_qn,
    };
    for canonical in expansion.emit_calls {
        ensure_stdlib_symbol(store, created, canonical, "rust")?;
        match stage_macro_emission(buf, &rel, &site, canonical) {
            Some(miss) => unresolved.push(miss),
            None => resolved += 1,
        }
    }
    Ok((resolved, total, unresolved))
}

/// The macro-marker `CallSite` one expansion belongs to, and its caller.
struct MacroSite<'a> {
    cs_id: &'a str,
    caller_qn: &'a str,
}

/// Stages one expansion target: the caller-level `rel` edge plus its per-site
/// twin (issue #335), which is not a second reference and so is not counted.
/// Returns the unresolved record when `rel` is not a declared table.
fn stage_macro_emission(
    buf: &mut crate::resolver::EdgeBuffer,
    rel: &str,
    site: &MacroSite,
    canonical: &str,
) -> Option<UnresolvedRef> {
    if !crate::graph_store::is_known_rel_table(rel) {
        return Some(UnresolvedRef {
            kind: "Calls".to_string(),
            from_id: site.cs_id.to_string(),
            target_text: canonical.to_string(),
            reason: format!("unknown rel table {rel}"),
        });
    }
    buf.add(
        rel,
        site.caller_qn,
        canonical,
        MACRO_CONFIDENCE,
        MACRO_METHOD,
    );
    if let Some(site_rel) = call_site_rel_table(NODE_STDLIB_SYMBOL) {
        buf.add(
            site_rel,
            site.cs_id,
            canonical,
            MACRO_CONFIDENCE,
            MACRO_METHOD,
        );
    }
    None
}

fn caller_from_callsite(cs_id: &str) -> String {
    if let Some(idx) = cs_id.rfind("::call@") {
        cs_id[..idx].to_string()
    } else {
        cs_id.to_string()
    }
}

/// Idempotently create a StdlibSymbol node. source: stages/stage-3b-v2.md §5.
pub fn ensure_stdlib_symbol(
    store: &GraphStore,
    created: &mut HashSet<String>,
    canonical_path: &str,
    language: &str,
) -> Result<(), String> {
    if created.contains(canonical_path) {
        return Ok(());
    }
    let name = canonical_path.rsplit("::").next().unwrap_or(canonical_path);
    let receiver_type = crate::stdlib_index::get_stdlib_table(language)
        .and_then(|t| {
            t.symbols()
                .iter()
                .find(|s| s.canonical_path == canonical_path)
        })
        .map(|s| s.receiver_type)
        .unwrap_or("");
    let cypher = format!(
        "CREATE (n:StdlibSymbol {{id: {}, name: {}, language: {}, \
         receiver_type: {}, canonical_path: {}}})",
        cypher_str(canonical_path),
        cypher_str(name),
        cypher_str(language),
        cypher_str(receiver_type),
        cypher_str(canonical_path),
    );
    // Ignore duplicate-key errors; the idempotency set prevents double
    // creation within a run, and LadybugDB rejects duplicate primary keys.
    let _ = store.execute_query(&cypher);
    created.insert(canonical_path.to_string());
    Ok(())
}
