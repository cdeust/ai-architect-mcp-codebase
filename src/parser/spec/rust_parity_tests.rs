// parser::spec::rust_parity_tests — pins the Rust migration to the dedicated
// `rust` walker at EXACT parity with the hand-written walker it replaced
// (ADR-0055 phase 8, §5 step 3).
//
// The expected records below ARE the pre-migration `parse_rust_file` output
// (full 7-tuple per node + full ref triples), captured mechanically from the
// hand-written walker on this corpus BEFORE it was deleted: **61 nodes, 66 refs,
// 0 parse errors**. Per-EdgeKind F1(new-vs-groundtruth) = 1.000. The assertion is
// on the ORDERED sequence, not just the set, so it also pins emission order — a
// stronger oracle than the set-only pins of phases 3-7, which matters here
// because Rust is the language AP is written in and its own graph/eval corpora
// key off these records.
//
// The exact-sequence assertion is the mutation oracle: it kills every walker
// mutant that perturbs any emitted node, ref, property, line, or order.
//
// The corpus exercises every Rust concern the walker handles, plus the edge cases
// that pin specific behaviors (each preserved for parity; the two that are
// pre-existing defects are filed separately):
//   - `extern crate serde;` → an `Import` (`import:serde`) with an **`Imports`**
//     edge to the crate, versus every `use` leaf's file-local **`Defines`** edge:
//     one language, two import edge kinds.
//   - `use` expansion: plain path, brace list, nested brace list
//     (`a::{b, c::{d, e}}` → three atomic imports), `as` alias (display name
//     becomes the alias), glob (`a::b::*`), glob nested in a list (`n::{m::*, o}`),
//     and `{self, BufRead}` (which imports `std::io` itself). No leaf keeps a raw
//     brace.
//   - `const` and `static` → `Constant` + `type_annotation`; `type` → `TypeAlias`
//     + `target_type`; `macro_rules!` → `Constant` (`is_macro=true`).
//   - `#[derive(Debug, Clone)]` → an `implements` property + one
//     `DeriveImplements` ref per trait, emitted AFTER the struct's own fields. A
//     comment between the attribute and the item does NOT reset the accumulator
//     (`Config` still derives), and a following non-derive attribute
//     (`#[non_exhaustive]`) contributes nothing but also does not reset it
//     (`State` still derives `Debug`).
//   - `struct Config { … }` → `Struct` + one `Field`/`HasField` per named member
//     with per-field visibility; the tuple struct `Wrapper<T>(T)` and the unit
//     struct `Unit` yield NO fields (neither has a `field_declaration_list`).
//   - `union Value` → a `Struct` with fields (a union names and lists its members
//     exactly as a struct does).
//   - `enum State` → `Enum` + one `Variant`/`HasVariant` per member, whatever the
//     member's own shape (unit `Idle`, tuple `Running(u32)`, struct-bodied
//     `Failed { code }`); a variant carries no visibility and no properties.
//   - `trait Handler: Clone + Send` → `Trait` + a `supertraits` property + one
//     `Extends` per supertrait, and its requirements as `Method`s receiver-typed
//     to the trait. The defaulted `describe`'s body is NOT scanned — there is no
//     `String::from` call site under `Handler::describe` (pre-existing gap,
//     preserved; filed as a follow-up).
//   - `impl Config` / `impl Handler for Config` → NO node for the block; methods
//     scope to `src/lib.rs::Config`, the trait impl adds `trait_name=Handler`, and
//     method bodies ARE scanned. `impl<T> Wrapper<T>` scopes to
//     `src/lib.rs::Wrapper<T>` — the `type` field text verbatim, generics
//     included.
//   - `impl Inner` inside `mod helpers` scopes to `src/lib.rs::Inner`, NOT
//     `src/lib.rs::helpers::Inner` — the FILE scope regardless of the enclosing
//     module (pre-existing defect, preserved; filed as a follow-up).
//   - `async fn` → `is_async=true` on functions and impl methods; a bodiless
//     trait requirement is reported `is_async=false` unconditionally.
//   - `pub` / `pub(crate)` / `pub(super)` / (none) → the modifier's verbatim text,
//     empty for private.
//   - `mod helpers { … }` → `Module` + `Defines`, recursing under the module QN;
//     `mod declared_elsewhere;` → a `Module` with nothing inside.
//   - Call sites: a direct call, a scoped call (`helpers::normalize`), a method
//     call (`name.is_empty`), a CHAINED method call
//     (`input.trim().to_string` AND `input.trim` — same start byte, so the
//     (start,end) byte span is the id discriminator), a call inside a closure
//     (`s.len`), a macro invocation (`shout!`, callee carries the `!`), and one
//     extra call site per function-value argument (`run_all(validate, input)`
//     yields `run_all`, then `validate`, then `input` — issue #87). The stack DFS
//     emits a body's calls in REVERSE source order.

use super::rust_parity_corpus::{expected_node_records, expected_refs, CORPUS, PATH};
use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

fn node_record(n: &ExtractedNode) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{:?}",
        n.label, n.name, n.qualified_name, n.start_line, n.end_line, n.visibility, n.properties
    )
}

fn ref_triple(e: &ExtractedRef) -> (String, String, String) {
    (
        e.kind.clone(),
        e.from_qualified_name.clone(),
        e.to_qualified_name.clone(),
    )
}

fn parse() -> ParseResult {
    parse_file(CORPUS, PATH, Language::Rust).expect("rust parse must not hard-fail")
}

#[test]
fn rust_spec_output_is_exact_parity() {
    let r = parse();
    assert_eq!(r.parse_errors, 0, "clean Rust must report 0 parse errors");
    assert!(
        r.error_ranges.is_empty(),
        "clean Rust must report no error ranges, got {:?}",
        r.error_ranges
    );

    let obs: Vec<String> = r.nodes.iter().map(node_record).collect();
    let exp: Vec<String> = expected_node_records()
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        obs, exp,
        "Rust node sequence (full record, in emission order) diverged from the \
         hand-written walker's ground truth"
    );

    let obs_refs: Vec<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp_refs: Vec<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();
    assert_eq!(
        obs_refs, exp_refs,
        "Rust ref sequence diverged from the hand-written walker's ground truth"
    );
}

/// The counts the pre-deletion capture recorded. A separate assertion so a
/// wholesale drift in either direction names itself before the diff of 61 records
/// is read.
#[test]
fn rust_parity_floor_counts_hold() {
    let r = parse();
    assert_eq!(r.nodes.len(), 61, "pre-migration floor: 61 nodes");
    assert_eq!(r.refs.len(), 66, "pre-migration floor: 66 refs");
}

/// Negative assertions: behaviors the ground truth does NOT contain. Without
/// these, a walker that emitted MORE than the old one would still pass the
/// sequence assertion's spirit only by luck of ordering — and each of these is a
/// preserved pre-existing gap a future fix must change deliberately.
#[test]
fn rust_parity_negatives_hold() {
    let r = parse();
    let callers: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.label == "CallSite")
        .filter_map(|n| {
            n.properties
                .iter()
                .find(|(k, _)| k == "caller_qn")
                .map(|(_, v)| v.as_str())
        })
        .collect();
    // A trait requirement's default body is not scanned for calls.
    assert!(
        !callers.contains(&"src/lib.rs::Handler::describe"),
        "a trait default body must stay unscanned (preserved pre-existing gap), \
         got callers {callers:?}"
    );
    // An `impl` block emits no node of its own — only its methods.
    assert!(
        !r.nodes.iter().any(|n| n.name == "impl"),
        "an impl block must emit no node"
    );
    // A tuple struct has no `field_declaration_list`, so no fields.
    assert!(
        !r.refs
            .iter()
            .any(|e| e.kind == "HasField" && e.from_qualified_name == "src/lib.rs::Wrapper"),
        "a tuple struct must yield no fields"
    );
    // No `use` leaf keeps a raw brace — the regression q9/q14 depend on.
    for n in r.nodes.iter().filter(|n| n.label == "Import") {
        assert!(
            !n.name.contains('{') && !n.name.contains('}'),
            "raw brace leaked into an Import name: {}",
            n.name
        );
    }
}

#[test]
fn rust_per_edge_kind_f1_is_at_parity() {
    use std::collections::BTreeSet;

    let r = parse();
    let obs_refs: Vec<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp: Vec<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();

    for kind in [
        "Defines",
        "HasMethod",
        "HasField",
        "HasVariant",
        "Extends",
        "Imports",
        "DeriveImplements",
    ] {
        let exp_k: BTreeSet<(String, String)> = exp
            .iter()
            .filter(|(k, _, _)| k == kind)
            .map(|(_, f, t)| (f.clone(), t.clone()))
            .collect();
        let obs_k: BTreeSet<(String, String)> = obs_refs
            .iter()
            .filter(|(k, _, _)| k == kind)
            .map(|(_, f, t)| (f.clone(), t.clone()))
            .collect();
        assert!(
            !exp_k.is_empty(),
            "the corpus must exercise edge kind {kind}; an empty expectation \
             would make its F1 vacuously 1.000"
        );
        let tp = exp_k.intersection(&obs_k).count();
        let fp = obs_k.difference(&exp_k).count();
        let fn_ = exp_k.difference(&obs_k).count();
        let precision = if tp + fp == 0 {
            1.0
        } else {
            tp as f64 / (tp + fp) as f64
        };
        let recall = if tp + fn_ == 0 {
            1.0
        } else {
            tp as f64 / (tp + fn_) as f64
        };
        let f1 = if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        };
        println!(
            "edge[{kind:<17}] P={precision:.3} R={recall:.3} F1={f1:.3} (tp={tp} fp={fp} fn={fn_})"
        );
        assert!(
            (f1 - 1.0).abs() < f64::EPSILON,
            "Rust {kind} F1 {f1:.3} is below parity 1.000"
        );
    }
}
