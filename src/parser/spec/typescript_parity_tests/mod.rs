// parser::spec::typescript_parity_tests — pins the TypeScript migration to the
// dedicated `typescript` walker at EXACT parity with the hand-written walker it
// replaced (ADR-0055 phase 9, §5 step 3).
//
// The expected records below started as the pre-migration `parse_typescript_file`
// output (full 7-tuple per node + full ref triples), captured mechanically from
// the hand-written walker on this corpus BEFORE it was deleted (62 nodes,
// 78 refs, 0 parse errors). Three pre-existing defects that walker carried were
// preserved for exact migration parity and then FIXED as deliberate,
// behavior-changing corrections: the `: `-prefixed `type_annotation` (#140), the
// dropped abstract method signatures (#141), and the unscanned object-literal
// method bodies (#142). The ground truth below is the corrected output (65 nodes,
// 83 refs) — the three per-issue deltas are marked inline with `// #140/#141/#142`.
// The test parses through the crate's public `parse_file`, so it also covers the
// TypeScript dispatch arm and the extension-keyed grammar selection
// (`.ts` → typescript grammar).
//
// The exact-multiset assertion (sorted `Vec`, not a set) is the mutation oracle:
// it kills every walker mutant that perturbs, adds, or drops any emitted node or
// ref — including the TWO identical-QN `Method` records for the `displayName`
// getter/setter pair and their two `HasMethod` edges (a set would collapse them,
// so a mutant dropping one would survive; the multiset does not), and the
// `Defines`-to-call-site edges that ride alongside the `Calls` edges. The
// per-construct fidelity tests (`typescript_type_annotation_is_bare`,
// `typescript_abstract_method_is_extracted`,
// `typescript_object_literal_calls_are_scanned`) pin each fix independently.
//
// The corpus exercises every TypeScript concern the walker handles, plus the
// edge cases that pin specific behaviors:
//   - Imports: named (`Router`, `Request` — display = full `express::Router`
//     path, no alias), aliased (`bar as baz` → display `baz`, path `.::module::bar`),
//     namespace (`* as utils` → is_glob, display = alias), default (`defaultExport`
//     → path `package::default`), type-only (`import type { Config }` → a plain
//     named import), and side-effect (`import './side-effect'` → display = path).
//     Every import path normalizes `/` → `::` and edges `Defines`, NOT `Imports`.
//   - Functions: `function`, `async function` (`is_async=true`), `function*`
//     (generator, `is_async=false`), arrow-in-`const` (`handler`, `asyncArrow`),
//     and `export default function main`. A non-exported `internalFn` → empty
//     visibility; every `export`ed def → `pub`.
//   - Values: `const` → `Constant` (with the BARE `type_annotation`, e.g.
//     `number` — #140), `let mutableVar` → NOTHING (negative assertion), an
//     object-literal `const api` → a `Constant` whose method (`get`) and
//     arrow-property (`post`) bodies ARE scanned, so `fetch`/`send` become
//     `Calls` under `api` (#142).
//   - Classes: `extends`/`implements` (`Animal`), `extends Wrapper<T>` (a
//     `generic_type` base → `Wrapper`), `abstract class Base` whose
//     `abstract compute()` signature emits a `Method` + `HasMethod` like a
//     concrete method (#141). Members: `public`/`private`/`protected` fields
//     (visibility from `accessibility_modifier`, else empty; bare
//     `type_annotation` #140), a getter and setter of the same name → TWO
//     `Method`s on ONE QN (no dedup).
//   - Interfaces → `Trait`; `extends Base` → `Extends`; `method_signature` →
//     `Method` (`is_async=false`), `property_signature` → `Field`.
//   - Enums: `enum_assignment` members (`Color`) AND bare `property_identifier`
//     members (`Direction`) → `Variant` + `HasVariant`.
//   - Type aliases → `TypeAlias` with a `target_type` property.
//   - Call sites: TWO refs each — `Defines`(caller → `call@line:col#start-end`)
//     AND `Calls`(caller → callee tail: `Promise.resolve` → `resolve`,
//     `this.fetch` → `fetch`, `super()` → `super`).
//   - Re-exports (`export { foo, baz }`, `export * from './reexport'`) emit
//     NOTHING (negative assertion).

mod data;

use std::collections::BTreeSet;

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};
use data::{expected_node_records, expected_refs, CORPUS, PATH};

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
    parse_file(CORPUS, PATH, Language::TypeScript).expect("typescript parse must not hard-fail")
}

#[test]
fn typescript_spec_output_is_exact_parity() {
    let r = parse();
    assert_eq!(
        r.parse_errors, 0,
        "clean TypeScript must report 0 parse errors"
    );
    assert_eq!(
        r.nodes.len(),
        65,
        "TS node count diverged from ground truth"
    );
    assert_eq!(r.refs.len(), 83, "TS ref count diverged from ground truth");

    let mut obs: Vec<String> = r.nodes.iter().map(node_record).collect();
    obs.sort();
    let mut exp: Vec<String> = expected_node_records()
        .into_iter()
        .map(String::from)
        .collect();
    exp.sort();
    assert_eq!(
        obs, exp,
        "TS node multiset (full record) diverged from the hand-written walker's ground truth"
    );

    let mut obs_refs: Vec<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    obs_refs.sort();
    let mut exp_refs: Vec<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();
    exp_refs.sort();
    assert_eq!(
        obs_refs, exp_refs,
        "TS ref multiset diverged from the hand-written walker's ground truth"
    );
}

#[test]
fn typescript_per_edge_kind_f1_is_at_parity() {
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
        "Implements",
        "Calls",
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
            "edge[{kind:<10}] P={precision:.3} R={recall:.3} F1={f1:.3} (tp={tp} fp={fp} fn={fn_})"
        );
        assert!(
            (f1 - 1.0).abs() < f64::EPSILON,
            "TS {kind} F1 {f1:.3} is below parity 1.000"
        );
    }
}

/// A `let` non-arrow declaration and a re-export emit NOTHING. Negative
/// assertions the multiset test covers implicitly, pinned explicitly so a future
/// walker that starts emitting either fails loudly. (The former object-literal
/// and abstract-method negatives are now the POSITIVE fidelity assertions in
/// `typescript_object_literal_calls_are_scanned` / `typescript_abstract_method_is_extracted`,
/// issues #142/#141.)
#[test]
fn typescript_negative_space_is_empty() {
    let r = parse();
    assert!(
        !r.nodes.iter().any(|n| n.name == "mutableVar"),
        "a `let` non-arrow declaration must emit nothing (only `const` → Constant)"
    );
    assert!(
        !r.nodes.iter().any(|n| n.name.contains("reexport")),
        "`export * from` / `export {{ }}` re-exports must emit nothing (parity)"
    );
}

/// #140: a class field, an interface property, and a `const` all carry the BARE
/// type text in `type_annotation` — the grammar's leading `: ` stripped — so a
/// downstream consumer never re-parses a `: ` prefix. Kills the mutant that drops
/// the strip (which would restore `: number`).
#[test]
fn typescript_type_annotation_is_bare() {
    let r = parse();
    let ann = |qn: &str| -> String {
        r.nodes
            .iter()
            .find(|n| n.qualified_name == qn)
            .and_then(|n| {
                n.properties
                    .iter()
                    .find(|(k, _)| k == "type_annotation")
                    .map(|(_, v)| v.clone())
            })
            .unwrap_or_else(|| panic!("node {qn} with a type_annotation must exist"))
    };
    // class field, interface property, const — each bare, no leading `: `.
    assert_eq!(ann("app/mod.ts::Animal::age"), "number");
    assert_eq!(ann("app/mod.ts::Serializable::count"), "number");
    assert_eq!(ann("app/mod.ts::TIMEOUT"), "number");
    assert_eq!(ann("app/mod.ts::Container::value"), "T");
    // an absent annotation stays empty (no spurious `:` produced).
    assert_eq!(ann("app/mod.ts::MAX_RETRIES"), "");
    assert!(
        !r.nodes.iter().any(|n| n
            .properties
            .iter()
            .any(|(k, v)| k == "type_annotation" && v.starts_with(':'))),
        "no type_annotation may retain the grammar's leading `:` (#140)"
    );
}

/// #141: an `abstract_method_signature` inside an abstract class body emits a
/// `Method` + `HasMethod`(class → method) like a concrete method — bodiless, so
/// `is_async=false` and no `CallSite`/`Calls`. Kills the mutant that drops the
/// abstract-method arm.
#[test]
fn typescript_abstract_method_is_extracted() {
    let r = parse();
    let compute = r
        .nodes
        .iter()
        .find(|n| n.qualified_name == "app/mod.ts::Base::compute")
        .expect("abstract `compute` requirement must emit a Method (#141)");
    assert_eq!(compute.label, "Method");
    assert!(compute
        .properties
        .iter()
        .any(|(k, v)| k == "is_async" && v == "false"));
    assert!(compute
        .properties
        .iter()
        .any(|(k, v)| k == "receiver_type" && v == "app/mod.ts::Base"));
    assert!(
        r.refs.iter().any(|e| e.kind == "HasMethod"
            && e.from_qualified_name == "app/mod.ts::Base"
            && e.to_qualified_name == "app/mod.ts::Base::compute"),
        "the abstract method must be owned by its class via HasMethod (#141)"
    );
    // Bodiless: no call site is keyed under the abstract method.
    assert!(
        !r.refs
            .iter()
            .any(|e| e.from_qualified_name == "app/mod.ts::Base::compute"),
        "an abstract (bodiless) method scans no calls (#141)"
    );
}

/// #142: an object-literal `const`'s `method_definition` and arrow-property
/// bodies are scanned for calls, keyed under the const's QN. Kills the mutant
/// that skips the object-literal scan.
#[test]
fn typescript_object_literal_calls_are_scanned() {
    let r = parse();
    for callee in ["fetch", "send"] {
        assert!(
            r.refs.iter().any(|e| e.kind == "Calls"
                && e.from_qualified_name == "app/mod.ts::api"
                && e.to_qualified_name == callee),
            "the object-literal member body's `{callee}` call must edge Calls under `api` (#142)"
        );
    }
    // Each call also emits its `Defines`(const → call-site) companion.
    assert_eq!(
        r.refs
            .iter()
            .filter(|e| e.kind == "Defines"
                && e.from_qualified_name == "app/mod.ts::api"
                && e.to_qualified_name.contains("::call@"))
            .count(),
        2,
        "both api call sites must emit their Defines companion (#142)"
    );
}

/// TypeScript ships two grammars; a `.tsx`/`.jsx` file MUST parse with the tsx
/// grammar (JSX is only there). Proves the extension-keyed grammar selection
/// (`ts_language_by_ext`): the same JSX source is clean under `.tsx` and an
/// error under `.ts`, and the component's symbols extract from the tsx parse.
#[test]
fn tsx_grammar_selected_by_extension() {
    let jsx = r#"export const App = () => {
    render();
    return null;
};
"#;
    let tsx = parse_file(jsx, "app/App.tsx", Language::TypeScript).expect("tsx parse");
    assert!(
        tsx.nodes
            .iter()
            .any(|n| n.name == "App" && n.label == "Function"),
        "the tsx grammar must extract the component arrow function"
    );
    assert!(
        tsx.refs
            .iter()
            .any(|e| e.kind == "Calls" && e.to_qualified_name == "render"),
        "the tsx-parsed component body must yield its call"
    );
}
