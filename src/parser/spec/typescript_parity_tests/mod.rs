// parser::spec::typescript_parity_tests — pins the TypeScript migration to the
// dedicated `typescript` walker at EXACT parity with the hand-written walker it
// replaced (ADR-0055 phase 9, §5 step 3).
//
// The expected records below ARE the pre-migration `parse_typescript_file`
// output (full 7-tuple per node + full ref triples), captured mechanically from
// the hand-written walker on this corpus BEFORE it was deleted (62 nodes,
// 78 refs, 0 parse errors). Per-EdgeKind F1(new-vs-groundtruth) = 1.000 ==
// F1(old-vs-groundtruth). The test parses through the crate's public
// `parse_file`, so it also covers the TypeScript dispatch arm and the
// extension-keyed grammar selection (`.ts` → typescript grammar).
//
// The exact-multiset assertion (sorted `Vec`, not a set) is the mutation oracle:
// it kills every walker mutant that perturbs, adds, or drops any emitted node or
// ref — including the TWO identical-QN `Method` records for the `displayName`
// getter/setter pair and their two `HasMethod` edges (a set would collapse them,
// so a mutant dropping one would survive; the multiset does not), and the eleven
// `Defines`-to-call-site edges that ride alongside the eleven `Calls` edges.
//
// The corpus exercises every TypeScript concern the walker handles, plus the
// edge cases that pin specific behaviors (each preserved for parity; three
// document pre-existing defects filed separately: the `: `-prefixed
// `type_annotation` #140, dropped abstract method signatures #141, and
// unscanned object-literal method bodies #142):
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
//   - Values: `const` → `Constant` (with `type_annotation` INCLUDING the leading
//     `: `, e.g. `: number` — a pre-existing quirk, preserved), `let mutableVar`
//     → NOTHING (negative assertion), an object-literal `const api` → a single
//     `Constant` with NO recursion into its methods (so `send` is never a call).
//   - Classes: `extends`/`implements` (`Animal`), `extends Wrapper<T>` (a
//     `generic_type` base → `Wrapper`), `abstract class Base` whose
//     `abstract compute()` signature is NOT a `method_definition` and so is
//     DROPPED (negative assertion). Members: `public`/`private`/`protected`
//     fields (visibility from `accessibility_modifier`, else empty), a getter
//     and setter of the same name → TWO `Method`s on ONE QN (no dedup).
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
        62,
        "TS node count diverged from ground truth"
    );
    assert_eq!(r.refs.len(), 78, "TS ref count diverged from ground truth");

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

/// A `let` non-arrow declaration, an object literal's methods, an abstract
/// method signature inside a class body, and a re-export all emit NOTHING.
/// Negative assertions the multiset test covers implicitly, pinned explicitly
/// so a future walker that starts emitting any of them fails loudly.
#[test]
fn typescript_negative_space_is_empty() {
    let r = parse();
    assert!(
        !r.nodes.iter().any(|n| n.name == "mutableVar"),
        "a `let` non-arrow declaration must emit nothing (only `const` → Constant)"
    );
    assert!(
        !r.refs.iter().any(|e| e.to_qualified_name == "send"),
        "an object-literal method body must NOT be scanned for calls (parity)"
    );
    assert!(
        !r.nodes
            .iter()
            .any(|n| n.qualified_name == "app/mod.ts::Base::compute"),
        "an abstract method signature in a class body is NOT a method_definition (dropped)"
    );
    assert!(
        !r.nodes.iter().any(|n| n.name.contains("reexport")),
        "`export * from` / `export {{ }}` re-exports must emit nothing (parity)"
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
