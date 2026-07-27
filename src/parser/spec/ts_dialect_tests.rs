// parser::spec::ts_dialect_tests — pins the TSX GRAMMAR DIALECT selection and
// its extraction parity (ADR-0055 phase 7, TypeScript).
//
// tree-sitter-typescript ships two grammars. JSX (`<Component/>`) exists only in
// `tsx`: parsed with `typescript`, every JSX element becomes an ERROR node and
// the symbols inside it are dropped — a silent-degradation failure this repo has
// paid for before. The pre-migration walker selected `tsx` for
// `.tsx`/`.jsx`/`.js`/`.mjs`/`.cjs` and `typescript` otherwise; the migration
// moves that decision into `LangSpec::dialect` (data) + `walkers::grammar_for`,
// and these tests pin it with a positive case per extension AND the negative
// controls that make those cases non-vacuous.
//
// The expected records are the pre-migration `parse_typescript_file` output on
// this corpus (4 nodes / 6 refs / 0 parse errors), captured before deletion; the
// old-vs-new diff was empty. Split out of `ts_parity_tests` to keep both files
// inside the §4.1 500-line cap; the record helpers are reused from there.

use super::ts_parity_tests::{assert_records, parse_ts};

/// JSX source: parses clean ONLY under the `tsx` dialect. Both a `const`-arrow
/// component (whose JSX body holds a call) and a `function` component (whose JSX
/// attribute holds an arrow with a call), so a dialect regression shows up as
/// BOTH a parse-error count and a dropped `Calls` edge.
const TSX_CORPUS: &str = r#"export const App = () => (
    <div className="x">{greet()}</div>
);

export function Page(): JSX.Element {
    return <App onClick={() => handle()} />;
}
"#;

fn expected_tsx_node_records() -> Vec<&'static str> {
    vec![
        "Function|App|ui/App.tsx::App|1|3|pub|[(\"is_async\", \"false\")]",
        "CallSite|greet|ui/App.tsx::App::call@2:24#51-58|2|2||[(\"callee_name\", \"greet\"), (\"caller_qn\", \"ui/App.tsx::App\")]",
        "Function|Page|ui/App.tsx::Page|5|7|pub|[(\"is_async\", \"false\")]",
        "CallSite|handle|ui/App.tsx::Page::call@6:31#139-147|6|6||[(\"callee_name\", \"handle\"), (\"caller_qn\", \"ui/App.tsx::Page\")]",
    ]
}

fn expected_tsx_ref_records() -> Vec<&'static str> {
    vec![
        "Defines|ui/App.tsx|ui/App.tsx::App",
        "Defines|ui/App.tsx::App|ui/App.tsx::App::call@2:24#51-58",
        "Calls|ui/App.tsx::App|greet",
        "Defines|ui/App.tsx|ui/App.tsx::Page",
        "Defines|ui/App.tsx::Page|ui/App.tsx::Page::call@6:31#139-147",
        "Calls|ui/App.tsx::Page|handle",
    ]
}

#[test]
fn tsx_spec_output_is_exact_parity() {
    let r = parse_ts(TSX_CORPUS, "ui/App.tsx");
    assert_eq!(r.parse_errors, 0, "JSX under the tsx dialect must be clean");
    assert_records(
        &r,
        expected_tsx_node_records(),
        expected_tsx_ref_records(),
        "TypeScript .tsx corpus",
    );
}

/// The JS/TSX family routes to the `tsx` grammar. Each extension is asserted
/// individually, so dropping ONE from `TSX_DIALECT.extensions` fails here rather
/// than silently degrading that file type to ERROR nodes.
#[test]
fn tsx_dialect_is_selected_per_extension() {
    for ext in ["tsx", "jsx", "js", "mjs", "cjs"] {
        let r = parse_ts(TSX_CORPUS, &format!("ui/App.{ext}"));
        assert_eq!(
            r.parse_errors, 0,
            ".{ext} must parse JSX with the tsx grammar (got {} errors)",
            r.parse_errors
        );
        assert!(
            r.refs
                .iter()
                .any(|e| e.kind == "Calls" && e.to_qualified_name == "greet"),
            ".{ext}: the call inside the JSX expression was dropped"
        );
    }
}

/// The negative control for the test above: without it, a dialect that returned
/// `tsx` unconditionally would pass. `.ts` keeps the `typescript` grammar, under
/// which JSX degrades into ERROR nodes.
#[test]
fn ts_extension_keeps_the_typescript_grammar() {
    let r = parse_ts(TSX_CORPUS, "ui/App.ts");
    assert!(
        r.parse_errors > 0,
        "JSX under the typescript grammar must report parse errors; 0 means .ts \
         is being parsed as tsx"
    );
    // Case-sensitive, as before the migration: `.TSX` is not `.tsx`.
    let upper = parse_ts(TSX_CORPUS, "ui/App.TSX");
    assert!(
        upper.parse_errors > 0,
        "extension matching must stay case-sensitive"
    );
    // A path with no extension has no dialect and uses the default grammar.
    let dotless = parse_ts("export function f() {}\n", "Makefile");
    assert_eq!(
        dotless.parse_errors, 0,
        "a dotless path must still parse with the default grammar"
    );
    assert!(
        dotless.nodes.iter().any(|n| n.name == "f"),
        "a dotless path must still extract symbols"
    );
}

/// The full export-visibility decision table, including the degraded parses.
///
/// `decl_visibility` recognizes exactly ONE export signal: the enclosing
/// `export_statement` wrapper. The hand-written walker had a second — "my
/// previous sibling is the `export` token" — which mutation testing flagged as
/// unreachable; forcing it to `false` changed nothing on any of these seven
/// inputs, clean or degraded, so it was removed as dead code (§12.1 / §9). This
/// test is what the redundant branch was obscuring, and it is the regression
/// pin for the removal: every row below is the pre-migration walker's answer.
///
/// The two rows that make the table non-obvious:
/// - `export\nfunction b() {}` is NOT exported. Automatic semicolon insertion
///   ends the `export` statement at the newline, so `b` is a separate top-level
///   declaration with no wrapper — and the old walker agreed (its sibling check
///   also saw no `export` sibling, because `b` is a child of the program).
/// - `export export function d() {}` IS exported (with one parse error): the
///   recovery still nests `d` under an `export_statement`.
#[test]
fn export_visibility_decision_table() {
    // (source, name, expected visibility, expected parse-error count)
    let cases: &[(&str, &str, &str, u32)] = &[
        ("export function a() {}\n", "a", "pub", 0),
        // ASI splits the statement: `b` is NOT exported.
        ("export\nfunction b() {}\n", "b", "", 0),
        ("export ; function c() {}\n", "c", "", 0),
        // Degraded, but the declaration still lands under the wrapper.
        ("export export function d() {}\n", "d", "pub", 1),
        ("export default class E {}\n", "E", "pub", 0),
        ("function plain() {}\n", "plain", "", 0),
        ("export const k = 1;\n", "k", "pub", 0),
    ];
    for (src, name, expected_visibility, expected_errors) in cases {
        let r = parse_ts(src, "p.ts");
        assert_eq!(
            r.parse_errors, *expected_errors,
            "{src:?}: parse-error count changed"
        );
        let node = r
            .nodes
            .iter()
            .find(|n| n.name == *name)
            .unwrap_or_else(|| panic!("{src:?}: {name} was not extracted at all"));
        assert_eq!(
            node.visibility, *expected_visibility,
            "{src:?}: {name} visibility"
        );
    }
    // A declaration inside a block statement is not a top-level definition at
    // all — the walker never descends into one (negative control).
    let nested = parse_ts("{ function h() {} }\n", "p.ts");
    assert!(
        !nested.nodes.iter().any(|n| n.name == "h"),
        "a function inside a block statement must not be extracted"
    );
}
