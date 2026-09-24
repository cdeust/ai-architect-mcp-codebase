// resolver_layers — Layer 4 (macro expansion) + Layer 5 (stdlib index)
// passes for the stage-3b-v2 resolver. Lives separate from resolver.rs so
// that Q8 (symbols-in-file) ground truth for resolver.rs remains stable as
// new passes are added. source: stages/stage-3b-v2.md §5.

use crate::ambiguity_policy::{confidence_for, resolution_label, Evidence};
use crate::graph_store::rust_macro_site_predicate;
use crate::graph_store::{call_site_rel_table, cypher_str, GraphStore, NODE_STDLIB_SYMBOL};
use crate::language_provider::extract_file_prefix_or_self;
use crate::macro_expansion::dispatch::{self, Basis, Decision, Destination, Dispatch};
use crate::macro_expansion::scope::Import;
use crate::resolver::{EdgeBuffer, PhaseResult, UnresolvedRef};
use std::collections::{HashMap, HashSet};

// source: issue #339 — the reasons an expansion gets no edge.
const REASON_AMBIGUOUS: &str = "ambiguous";
const REASON_NO_STABLE_TARGET: &str = "no stable target for this expansion";

/// Entry point for Layer 4 (macros + derives).
/// postcondition: returns the same `(resolved, total, unresolved)` shape as
/// every resolver.rs phase, so its counts fold cleanly into `total_refs`.
/// source: issue #28 — previously returned only `resolved` (a raw edge
/// count folded into the numerator with no matching denominator
/// contribution), which let `resolution_rate` exceed 1.0.
pub fn run_macro_expansion(
    store: &GraphStore,
    buf: &mut EdgeBuffer,
    ctx: &MacroContext,
) -> PhaseResult {
    let mut pass = MacroPass {
        store,
        buf,
        ctx,
        created: HashSet::new(),
        imports: HashMap::new(),
    };
    pass.run()
}

/// What the macro pass reads from the rest of the resolver.
pub struct MacroContext<'a> {
    pub caller_label_of: &'a dyn Fn(&str) -> String,
    /// True when the file (first argument) defines a struct, enum, trait or
    /// alias of that name (second argument).
    pub is_type_defined_in_file: &'a dyn Fn(&str, &str) -> bool,
}

/// One macro-marker `CallSite` (`callee_name` ending in `!`) as the graph
/// stores it.
struct MacroRow {
    cs_id: String,
    macro_name: String,
    receiver_hint: String,
    arg_shape: String,
}

struct MacroPass<'a> {
    store: &'a GraphStore,
    buf: &'a mut EdgeBuffer,
    ctx: &'a MacroContext<'a>,
    created: HashSet<String>,
    /// The `use` items of each file: name bound and path, aliases included.
    imports: HashMap<String, Vec<Import>>,
}

/// The macro-marker `CallSite` one expansion belongs to, its caller, and the
/// caller-level relationship table its edges go to.
struct MacroSite<'a> {
    cs_id: &'a str,
    caller_qn: &'a str,
    rel: &'a str,
}

impl MacroPass<'_> {
    /// Resolves each macro-marker CallSite to its expansion.
    ///
    /// Granularity: a fixed expansion (`println!`) is one syntactic reference
    /// whose resolution fans out into N edges, one per `emit_calls` entry, so
    /// it contributes N units (the denominator matches the numerator's
    /// granularity, as in resolver::resolve_field_type_uses). A decided
    /// expansion (`write!`, `vec!`) calls exactly one target: it contributes
    /// one unit, resolved when the target is determined, unresolved otherwise.
    /// A site with no attemptable emission contributes one unresolved unit.
    ///
    /// stages/stage-3.md §10.4: a CallSite's `is_resolved` flips when its
    /// callee resolved to a graph target, whichever phase found it (#335).
    fn run(&mut self) -> PhaseResult {
        self.imports = self.read_imports()?;
        let rows = self.read_rows()?;
        let (mut resolved, mut total, mut unresolved) = (0u64, 0u64, Vec::new());
        let mut resolved_ids: Vec<&str> = Vec::new();
        for row in &rows {
            let (r, t, u) = self.resolve_one(row)?;
            if r > 0 {
                resolved_ids.push(&row.cs_id);
            }
            resolved += r;
            total += t;
            unresolved.extend(u);
        }
        self.store.mark_nodes_resolved("CallSite", &resolved_ids)?;
        Ok((resolved, total, unresolved))
    }

    /// Every `Import` of the graph grouped by file. `file_imports` keeps the
    /// path only, which loses `use x::Y as Z`; a type placed by an alias needs
    /// the bound name too.
    fn read_imports(&self) -> Result<HashMap<String, Vec<Import>>, String> {
        let qr = self
            .store
            .execute_query("MATCH (i:Import) RETURN i.id, i.path, i.alias, i.is_glob")?;
        let mut by_file: HashMap<String, Vec<Import>> = HashMap::new();
        for r in qr.rows.iter().filter(|r| r.len() >= 4) {
            by_file
                .entry(extract_file_prefix_or_self(&r[0]))
                .or_default()
                .push(Import::new(&r[1], &r[2], r[3] == "true"));
        }
        Ok(by_file)
    }

    fn read_rows(&self) -> Result<Vec<MacroRow>, String> {
        self.store
            .ensure_node_column("CallSite", "receiver_hint", "STRING DEFAULT ''")?;
        self.store
            .ensure_node_column("CallSite", "macro_arg_shape", "STRING DEFAULT ''")?;
        let pred = rust_macro_site_predicate();
        let qr = self.store.execute_query(&format!(
            "MATCH (cs:CallSite) WHERE {pred} \
             RETURN cs.id, cs.callee_name, cs.receiver_hint, cs.macro_arg_shape"
        ))?;
        let mut rows = Vec::new();
        for r in qr.rows.iter().filter(|r| r.len() >= 4) {
            let Some(path) = r[1].strip_suffix('!') else {
                continue;
            };
            rows.push(MacroRow {
                cs_id: r[0].clone(),
                macro_name: path.rsplit("::").next().unwrap_or(path).to_string(),
                receiver_hint: r[2].clone(),
                arg_shape: r[3].clone(),
            });
        }
        Ok(rows)
    }

    fn resolve_one(&mut self, row: &MacroRow) -> PhaseResult {
        let none = |reason: &str| (0, 1, vec![unresolved_ref(row, &row.macro_name, reason)]);
        let caller_qn = caller_from_callsite(&row.cs_id);
        let caller_label = (self.ctx.caller_label_of)(&caller_qn);
        // source: stages/stage-3b.md §2 — Calls_*_StdlibSymbol is defined for
        // Function|Method callers only.
        if caller_label != "Function" && caller_label != "Method" {
            return Ok(none("caller is not a callable (Function|Method)"));
        }
        let rel = format!("Calls_{caller_label}_StdlibSymbol");
        let site = MacroSite {
            cs_id: &row.cs_id,
            caller_qn: &caller_qn,
            rel: &rel,
        };
        if let Some(rule) = dispatch::dispatch_for(&row.macro_name) {
            return self.resolve_decided(row, &site, rule);
        }
        let Some(expansion) = crate::macro_expansion::lookup("rust", &row.macro_name) else {
            return Ok(none("no macro-expansion table entry"));
        };
        if expansion.emit_calls.is_empty() {
            return Ok(none("expansion has no emit_calls entries"));
        }
        let (mut resolved, mut unresolved) = (0u64, Vec::new());
        for canonical in expansion.emit_calls {
            self.ensure_symbol(canonical)?;
            match stage_macro_emission(self.buf, &site, canonical, Evidence::MacroExpansion) {
                Some(miss) => unresolved.push(miss),
                None => resolved += 1,
            }
        }
        Ok((resolved, expansion.emit_calls.len() as u64, unresolved))
    }

    /// A macro whose one target is decided (issue #339): one unit, an edge
    /// only when the decision names a target.
    fn resolve_decided(&mut self, row: &MacroRow, site: &MacroSite, rule: Dispatch) -> PhaseResult {
        let decision = match rule {
            Dispatch::ReceiverType(alternatives) => {
                let file = extract_file_prefix_or_self(&row.cs_id);
                let imports = self.imports.get(&file).map_or(&[][..], Vec::as_slice);
                let name = row.receiver_hint.rsplit("::").next().unwrap_or_default();
                let dest = Destination {
                    declared: &row.receiver_hint,
                    defined_in_file: !name.is_empty()
                        && (self.ctx.is_type_defined_in_file)(&file, name),
                };
                dispatch::decide_by_receiver(alternatives, &dest, imports)
            }
            Dispatch::ArgShape(rules) => dispatch::decide_by_shape(rules, &row.arg_shape),
        };
        let target = &row.macro_name;
        match decision {
            Decision::Target { canonical, basis } => {
                self.ensure_symbol(canonical)?;
                let miss = stage_macro_emission(self.buf, site, canonical, evidence_of(basis));
                Ok(match miss {
                    Some(m) => (0, 1, vec![m]),
                    None => (1, 1, Vec::new()),
                })
            }
            Decision::Undetermined { candidates } => {
                let reason = if candidates > 1 {
                    format!("{REASON_AMBIGUOUS} ({candidates} candidates)")
                } else {
                    REASON_NO_STABLE_TARGET.to_string()
                };
                Ok((0, 1, vec![unresolved_ref(row, target, &reason)]))
            }
        }
    }

    fn ensure_symbol(&mut self, canonical: &str) -> Result<(), String> {
        ensure_stdlib_symbol(self.store, &mut self.created, canonical, "rust")
    }
}

fn unresolved_ref(row: &MacroRow, macro_name: &str, reason: &str) -> UnresolvedRef {
    UnresolvedRef {
        kind: "Calls".to_string(),
        from_id: row.cs_id.clone(),
        target_text: format!("{macro_name}!"),
        reason: reason.to_string(),
    }
}

fn evidence_of(basis: Basis) -> Evidence {
    match basis {
        Basis::ReceiverType => Evidence::MacroReceiverType,
        Basis::ArgShape => Evidence::MacroExpansion,
    }
}

/// Stages one expansion target: the caller-level edge plus its per-site twin
/// (issue #335), which is not a second reference and so is not counted.
/// Returns the unresolved record when the caller-level table is not declared.
fn stage_macro_emission(
    buf: &mut EdgeBuffer,
    site: &MacroSite,
    canonical: &str,
    evidence: Evidence,
) -> Option<UnresolvedRef> {
    if !crate::graph_store::is_known_rel_table(site.rel) {
        return Some(UnresolvedRef {
            kind: "Calls".to_string(),
            from_id: site.cs_id.to_string(),
            target_text: canonical.to_string(),
            reason: format!("unknown rel table {}", site.rel),
        });
    }
    let (confidence, method) = (confidence_for(evidence), resolution_label(evidence));
    buf.add(site.rel, site.caller_qn, canonical, confidence, method);
    if let Some(site_rel) = call_site_rel_table(NODE_STDLIB_SYMBOL) {
        buf.add(site_rel, site.cs_id, canonical, confidence, method);
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
