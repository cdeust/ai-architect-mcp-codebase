// parser::spec::embedded_tests — exercises the embedded-language re-parse path
// (`walk_embedded` / `reparse_embedded`). The ten core specs carry
// `embedded: &[]` (ADR-0055 §3: no core grammar is a host), so this path would
// otherwise be unexercised. To test the machinery WITHOUT vendoring a host
// grammar (Vue/Svelte), we pair Go-with-Go: a Go raw string literal whose
// content is itself a valid Go file. `raw_string_literal_content`'s text
// excludes the backtick delimiters and preserves real newlines, so slicing it
// yields a clean embedded file the Go grammar re-parses.

use super::conventions::LanguageConventions;
use super::go::GO_SPEC;
use super::lang_spec::{EmbeddedSpec, LangSpec};
use super::walkers::parse_with_spec;
use crate::parser::Language;

// A synthetic host: identical to the Go spec but declaring one embedded rule —
// re-parse the content of a Go raw string literal as a Go file.
// source: tree-sitter-go 0.23.4 node-types.json (raw_string_literal /
// raw_string_literal_content).
static HOST_EMBEDDED: [EmbeddedSpec; 1] = [EmbeddedSpec {
    script_node_kind: "raw_string_literal",
    content_node_kind: "raw_string_literal_content",
    embedded_language: Language::Go,
}];

// Build a `&'static dyn LanguageConventions` from the Go spec's own.
fn go_conventions() -> &'static dyn LanguageConventions {
    GO_SPEC.conventions
}

fn host_spec() -> LangSpec {
    // Identical to the Go spec but with one embedded rule and an explicit
    // conventions handle. All `LangSpec` fields are `Copy` (references, `Option`s,
    // and a fn pointer), so struct-update from the `static` copies the rest.
    LangSpec {
        embedded: &HOST_EMBEDDED,
        conventions: go_conventions(),
        ..GO_SPEC
    }
}

// A Go host file whose raw string literal is a complete, valid Go sub-file.
const HOST_SRC: &str = "package host\n\nvar embedded = `package emb\nfunc g() {}\n`\n";

#[test]
fn walk_embedded_reparses_and_extracts_inner_symbols() {
    let spec = host_spec();
    let r = parse_with_spec(&spec, HOST_SRC, "host.go").expect("host parse");
    // The embedded Go file declares `func g()`; the re-parse must surface it.
    let has_inner_fn = r
        .nodes
        .iter()
        .any(|n| n.label == "Function" && n.name == "g");
    assert!(
        has_inner_fn,
        "walk_embedded must re-parse the raw-string content and extract inner `g`; got {:?}",
        r.nodes
            .iter()
            .map(|n| (&n.label, &n.name))
            .collect::<Vec<_>>()
    );
}

#[test]
fn empty_embedded_does_not_reparse() {
    // With the real Go spec (embedded: &[]), the same source yields NO inner
    // `g` — proving the extraction above comes from the embedded path, not from
    // ordinary walking. Kills the mutant where walk_embedded ignores its rules.
    let r = parse_with_spec(&GO_SPEC, HOST_SRC, "host.go").expect("host parse");
    let has_inner_fn = r
        .nodes
        .iter()
        .any(|n| n.label == "Function" && n.name == "g");
    assert!(
        !has_inner_fn,
        "with empty embedded, inner `g` must NOT appear (no re-parse)"
    );
}
