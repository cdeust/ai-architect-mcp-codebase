// resolver::uses — Stage-3b Phase 5: Uses (type-usage) resolution
//
// Extracted from resolver.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared
// resolution types/helpers exactly as when this lived in one module.

use super::*;

// ---------------------------------------------------------------------------
// Phase 5: Uses (type-usage) resolution
// source: stages/stage-3b.md §5.5
// ---------------------------------------------------------------------------

pub(super) fn resolve_uses(
    store: &GraphStore,
    idx: &SymbolIndex,
    file_imports: &HashMap<String, Vec<String>>,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();

    // Resolve field type annotations -> Struct/Enum/Trait
    let field_result = resolve_field_type_uses(store, idx, file_imports, buf)?;
    resolved += field_result.0;
    total += field_result.1;
    unresolved.extend(field_result.2);

    // Resolve return-type + type-construction references on Function/Method
    // (issue #92) -> Uses_<caller>_<Type>.
    let callable_result = resolve_callable_type_uses(store, idx, buf)?;
    resolved += callable_result.0;
    total += callable_result.1;
    unresolved.extend(callable_result.2);

    Ok((resolved, total, unresolved))
}

/// Resolves the `return_type` and `constructed_types` references recorded on
/// each Function/Method (issue #92) into `Uses_<caller_label>_<TargetLabel>`
/// edges. The two properties are the parser's record of the types a callable
/// names in its return-type annotation and constructs in its body; neither is a
/// call the call walker captures for Go composite literals / Rust struct
/// literals / TS `new` (Python constructs via a plain call, already covered by
/// `stage_call_edge`), so this phase is what surfaces `core.go`/`core.rs`/
/// `core.ts` as users of `OrderConfig` in the #64 eval's D4 rows.
///
/// postcondition: `resolved + unresolved.len() as u64 == total`; `total` counts
/// one unit per extracted type identifier across BOTH properties (matching
/// `resolve_field_type_uses`' per-identifier accounting, so a callable that both
/// returns and constructs the same type contributes 2 — the second `buf.add` is
/// a `DuplicateInRun` no-op edge-wise but still a resolved reference).
fn resolve_callable_type_uses(
    store: &GraphStore,
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();

    // Function and Method carry the same two columns (graph_store COLS_*); each
    // is its own caller label in the emitted `Uses_<label>_<Type>` table.
    for caller_label in ["Function", "Method"] {
        let query = format!(
            "MATCH (f:{caller_label}) RETURN f.id, f.return_type, f.constructed_types, f.language"
        );
        let qr = store.execute_query(&query)?;
        for row in &qr.rows {
            if row.len() < 4 {
                continue;
            }
            let callable_id = &row[0];
            let provider = crate::language_provider::provider_for(&row[3]);
            let prims = provider.primitives();
            // Return-type identifiers, then construction identifiers — both fed
            // through the same nominal-type extractor the field path uses.
            let names: Vec<String> = extract_type_identifiers(&row[1], prims)
                .into_iter()
                .chain(extract_type_identifiers(&row[2], prims))
                .collect();
            for type_name in &names {
                total += 1;
                let (r, u) =
                    resolve_one_callable_type_use(idx, buf, callable_id, caller_label, type_name);
                resolved += r;
                unresolved.extend(u);
            }
        }
    }
    Ok((resolved, total, unresolved))
}

/// Resolves one extracted type identifier for one Function/Method into a
/// `Uses_<caller_label>_<TargetLabel>` edge.
/// postcondition: `(1, vec![])` iff the identifier matched a known type-like
/// node AND the schema declares the resulting rel table; `(0, vec![one entry])`
/// otherwise (type not found, or no rel table for the label combination).
fn resolve_one_callable_type_use(
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
    callable_id: &str,
    caller_label: &str,
    type_name: &str,
) -> (u64, Vec<UnresolvedRef>) {
    let target = match find_type_target(idx, type_name) {
        Some(t) => t,
        None => {
            return (
                0,
                vec![UnresolvedRef {
                    kind: "Uses".to_string(),
                    from_id: callable_id.to_string(),
                    target_text: type_name.to_string(),
                    reason: "type not found in graph".to_string(),
                }],
            )
        }
    };
    let table = format!("Uses_{caller_label}_{}", target.label);
    if !check_known_rel_table(&table, callable_id, &target.id) {
        return (
            0,
            vec![UnresolvedRef {
                kind: "Uses".to_string(),
                from_id: callable_id.to_string(),
                target_text: type_name.to_string(),
                reason: format!("unknown rel table {table}"),
            }],
        );
    }
    buf.add(&table, callable_id, &target.id, 0.9, "type-reference-parse");
    (1, vec![])
}

/// Resolves every nominal type identifier extracted from each Field's type
/// annotation.
/// postcondition: `resolved + unresolved.len() as u64 == total` — `total`
/// counts per extracted type identifier, not per Field row. A field typed
/// `HashMap<Foo, Bar>` extracts two identifiers (`Foo`, `Bar`; `HashMap`
/// itself is a provider-declared primitive/container and is skipped), so it
/// contributes 2 to `total`, matching the up-to-2 edges it can produce.
/// source: issue #28 — previously `total = qr.rows.len()` (1 per Field),
/// while `resolved`/`unresolved` counted per type identifier, so a
/// multi-identifier field inflated the numerator past the denominator.
fn resolve_field_type_uses(
    store: &GraphStore,
    idx: &SymbolIndex,
    _file_imports: &HashMap<String, Vec<String>>,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    let qr = store.execute_query("MATCH (f:Field) RETURN f.id, f.type_annotation, f.language")?;
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();

    for row in &qr.rows {
        if row.len() < 3 {
            continue;
        }
        let field_id = &row[0];
        let type_ann = &row[1];
        let provider = crate::language_provider::provider_for(&row[2]);
        let type_names = extract_type_identifiers(type_ann, provider.primitives());

        for type_name in &type_names {
            total += 1;
            let (r, u) = resolve_one_field_type_use(idx, buf, field_id, type_name);
            resolved += r;
            unresolved.extend(u);
        }
    }
    Ok((resolved, total, unresolved))
}

/// Resolves one extracted type identifier for one Field.
/// postcondition: `(1, vec![])` iff the identifier matched a known type and
/// a schema-known rel table exists for it; `(0, vec![one entry])`
/// otherwise.
fn resolve_one_field_type_use(
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
    field_id: &str,
    type_name: &str,
) -> (u64, Vec<UnresolvedRef>) {
    let target = match find_type_target(idx, type_name) {
        Some(t) => t,
        None => {
            return (
                0,
                vec![UnresolvedRef {
                    kind: "Uses".to_string(),
                    from_id: field_id.to_string(),
                    target_text: type_name.to_string(),
                    reason: "type not found in graph".to_string(),
                }],
            )
        }
    };
    let table = format!("Uses_Field_{}", target.label);
    if !check_known_rel_table(&table, field_id, &target.id) {
        return (
            0,
            vec![UnresolvedRef {
                kind: "Uses".to_string(),
                from_id: field_id.to_string(),
                target_text: type_name.to_string(),
                reason: format!("unknown rel table {table}"),
            }],
        );
    }
    buf.add(&table, field_id, &target.id, 0.9, "type-annotation-parse");
    (1, vec![])
}

/// Extract type identifiers from a type annotation string.
/// Strips generics, references, lifetimes, and finds nominal types. The
/// `primitives` skip-set is language-specific (provided by LanguageProvider);
/// lowercase builtins are additionally skipped by the uppercase-identifier
/// convention below.
pub(super) fn extract_type_identifiers(type_ann: &str, primitives: &[&str]) -> Vec<String> {
    let mut result = Vec::new();
    let cleaned = type_ann.replace(['&', '*', '<', '>', ',', '(', ')', '[', ']'], " ");
    for token in cleaned.split_whitespace() {
        // Skip lifetimes, keywords, primitives
        if token.starts_with('\'') || token == "mut" || token == "dyn" || token == "impl" {
            continue;
        }
        if primitives.contains(&token) {
            continue;
        }
        // Must start with uppercase to be a type name (convention)
        if token.chars().next().is_some_and(|c| c.is_uppercase()) {
            result.push(token.to_string());
        }
    }
    result
}

fn find_type_target<'a>(idx: &'a SymbolIndex, type_name: &str) -> Option<&'a SymbolEntry> {
    let candidates = idx.by_name.get(type_name)?;
    // Filter to type-like labels only
    let types: Vec<_> = candidates
        .iter()
        .filter(|e| matches!(e.label.as_str(), "Struct" | "Enum" | "Trait" | "TypeAlias"))
        .collect();
    if types.len() == 1 {
        return Some(types[0]);
    }
    if types.is_empty() {
        return None;
    }
    // Ambiguous: return first match with lower confidence (handled by caller)
    Some(types[0])
}
