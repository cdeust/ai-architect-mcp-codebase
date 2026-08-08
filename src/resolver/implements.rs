// resolver::implements — Stage-3b Phase 3: Implements resolution
//
// Extracted from resolver.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared
// resolution types/helpers exactly as when this lived in one module.

use super::*;

// ---------------------------------------------------------------------------
// Phase 3: Implements resolution
// source: stages/stage-3b.md §5.3
// ---------------------------------------------------------------------------

/// Resolves Implements edges from DECLARED facts, not method-name guesses.
/// Two sources:
///   (A) the `implements` CSV column on Struct/Enum (`#[derive(...)]` names,
///       and, for other languages, `implements`/interface clauses), and
///   (B) `impl Trait for Type` blocks, which the parser stamps onto each
///       method as `trait_name` + the receiver's QN.
///
/// source: implements fix — replaces the prior fuzzy `trait-name-match`
/// heuristic (which guessed Struct→Trait whenever a method name coincided
/// with a trait method, producing false edges and missing every declared
/// impl). Mirrors resolve_extends and finally wires the
/// macro_expansion.emit_implements table (Debug → std::fmt::Debug, …).
pub(super) fn resolve_implements(
    store: &GraphStore,
    idx: &SymbolIndex,
    file_imports: &HashMap<String, Vec<String>>,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();
    let mut created_stdlib: HashSet<String> = HashSet::new();

    // (A) Declared/derived trait names on Struct/Enum.
    for label in &["Struct", "Enum"] {
        let q = format!("MATCH (s:{label}) RETURN s.id, s.implements, s.language");
        let qr = match store.execute_query(&q) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() < 3 {
                continue;
            }
            let from_id = &row[0];
            let csv = &row[1];
            let provider = crate::language_provider::provider_for(&row[2]);
            if csv.is_empty() || csv == "Null(String)" {
                continue;
            }
            for raw in csv.split(',') {
                let name = raw.trim();
                if name.is_empty() {
                    continue;
                }
                total += 1;
                let ctx = ResolveContext { store, idx };
                let candidate = ImplementsCandidate {
                    provider,
                    label,
                    from_id,
                    name,
                };
                if resolve_one_implements(&ctx, buf, &candidate, &mut created_stdlib)? {
                    resolved += 1;
                } else {
                    // issue #216: same external-vs-missing classification as
                    // resolve_one_extends_base — see unresolved_base_reason.
                    let lookup = provider.import_last_segment(name);
                    unresolved.push(UnresolvedRef {
                        kind: "Implements".to_string(),
                        from_id: from_id.clone(),
                        target_text: name.to_string(),
                        reason: unresolved_base_reason(provider, file_imports, from_id, lookup),
                    });
                }
            }
        }
    }

    // (B) `impl Trait for Type` blocks.
    let (b_res, b_total, b_unresolved) = resolve_impl_trait_blocks(store, idx, buf)?;
    resolved += b_res;
    total += b_total;
    unresolved.extend(b_unresolved);

    Ok((resolved, total, unresolved))
}

/// Resolve one implemented-trait NAME for a Struct/Enum. Prefers a local Trait
/// in the corpus (`Implements_<Label>_Trait`, confidence 0.95); otherwise a
/// stdlib trait via the derive macro table (`Implements_<Label>_StdlibSymbol`,
/// 0.9).
/// postcondition: returns `Ok(true)` iff a target trait was found — staging
/// outcome (`AddOutcome`) does not gate this; a ref whose edge is already
/// persisted or duplicated within this run still resolved to a real trait.
///
/// Read-only lookup context shared by resolve_one_implements — bundles the
/// two params it only ever reads together, per coding-standards.md §4.4
/// (>4 params is a missing data type).
struct ResolveContext<'a> {
    store: &'a GraphStore,
    idx: &'a SymbolIndex,
}

/// One implemented-trait-name candidate being resolved: which Struct/Enum
/// declared it, in what language, and the raw trait name text.
struct ImplementsCandidate<'a> {
    provider: &'a dyn crate::language_provider::LanguageProvider,
    label: &'a str,
    from_id: &'a str,
    name: &'a str,
}

fn resolve_one_implements(
    ctx: &ResolveContext,
    buf: &mut EdgeBuffer,
    candidate: &ImplementsCandidate,
    created_stdlib: &mut HashSet<String>,
) -> Result<bool, String> {
    let ImplementsCandidate {
        provider,
        label,
        from_id,
        name,
    } = *candidate;
    let lookup = provider.import_last_segment(name);
    if let Some(t) = ctx
        .idx
        .by_name
        .get(lookup)
        .and_then(|c| c.iter().find(|e| e.label == "Trait"))
    {
        let table = format!("Implements_{label}_Trait");
        buf.add(&table, from_id, &t.id, 0.95, "declared-implements");
        return Ok(true);
    }
    // Derive/decorator → stdlib-trait expansion, only for languages that
    // declare such a table (Rust derives). `None` → no fabricated edges.
    let macro_key = match provider.derive_macro_key() {
        Some(k) => k,
        None => return Ok(false),
    };
    if let Some(exp) = crate::macro_expansion::lookup(macro_key, &format!("derive_{name}")) {
        let mut any = false;
        let table = format!("Implements_{label}_StdlibSymbol");
        for canonical in exp.emit_implements {
            crate::resolver_layers::ensure_stdlib_symbol(
                ctx.store,
                created_stdlib,
                canonical,
                "rust",
            )?;
            buf.add(&table, from_id, canonical, 0.9, "derive-macro");
            any = true;
        }
        return Ok(any);
    }
    Ok(false)
}

/// Resolve `impl Trait for Type` blocks. The parser stamps each method in such
/// a block with `trait_name` and the receiver's QN; we emit one
/// `Implements_<Label>_Trait` edge per block. buf.add collapses the repeated
/// methods of a block to a single edge.
fn resolve_impl_trait_blocks(
    store: &GraphStore,
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();
    let qr = store.execute_query(
        "MATCH (m:Method) WHERE m.trait_name <> '' AND m.receiver_type <> '' \
         RETURN m.receiver_type, m.trait_name",
    )?;
    for row in &qr.rows {
        if row.len() < 2 {
            continue;
        }
        let receiver_qn = row[0].trim();
        let trait_name = row[1].trim();
        if receiver_qn.is_empty() || trait_name.is_empty() {
            continue;
        }
        let recv = match idx.by_qn.get(receiver_qn) {
            Some(e) if e.label == "Struct" || e.label == "Enum" => e,
            _ => continue,
        };
        total += 1;
        let lookup = trait_name.rsplit("::").next().unwrap_or(trait_name);
        match idx
            .by_name
            .get(lookup)
            .and_then(|c| c.iter().find(|e| e.label == "Trait"))
        {
            Some(t) => {
                let table = format!("Implements_{}_Trait", recv.label);
                buf.add(&table, &recv.id, &t.id, 0.95, "impl-block");
                resolved += 1;
            }
            None => unresolved.push(UnresolvedRef {
                kind: "Implements".to_string(),
                from_id: recv.id.clone(),
                target_text: trait_name.to_string(),
                reason: "no_target_in_corpus".to_string(),
            }),
        }
    }
    Ok((resolved, total, unresolved))
}
