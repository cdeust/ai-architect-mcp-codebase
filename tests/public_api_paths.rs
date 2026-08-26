// public_api_paths — pins the Rust import paths this crate publishes.
//
// Cargo.toml's [lib] section states the promise these tests enforce: "Keep the
// Rust import path stable for workspace consumers and downstream users." A
// promise nothing checks is a promise that gets broken by a refactor nobody
// reads as an API change — which is exactly what happened (review round 3,
// finding 5). Splitting `search/bm25.rs` into a private submodule narrowed
// `build_schema` and `Bm25Fields` from `pub` to crate-internal as a side
// effect. Every gate stayed green: the crate still compiled, every test still
// passed, clippy was clean. Nothing inside the crate can see the difference.
//
// This file lives in `tests/` deliberately. An integration test links the crate
// as an EXTERNAL dependency, so it sees exactly the surface a downstream user
// sees — a `pub(crate)` item does not resolve here. An equivalent assertion
// written inside `src/` would pass under both visibilities and prove nothing.
//
// Adding a path here is a deliberate act: it declares that path public API and
// makes narrowing it a test failure rather than a silent break.

use ai_architect_mcp::search::bm25;

/// `search::bm25::build_schema` and `search::bm25::Bm25Fields` were `pub`
/// before fleet-watch#112 and must stay reachable from outside the crate.
///
/// This does not merely name the path — it CALLS it and binds the returned
/// struct's fields, so neither the function nor the type can be narrowed,
/// renamed, or have its public shape reduced without failing here.
#[test]
fn bm25_schema_construction_stays_public() {
    let (schema, fields): (tantivy::schema::Schema, bm25::Bm25Fields) = bm25::build_schema();

    // The field handles are public members of a public struct; reading them is
    // part of the surface a downstream consumer relies on.
    for (name, field) in [
        ("qualified_name", fields.qualified_name),
        ("name", fields.name),
        ("label", fields.label),
        ("file_path", fields.file_path),
        ("body", fields.body),
    ] {
        assert_eq!(
            schema.get_field_name(field),
            name,
            "public schema field `{name}` moved or was renamed"
        );
    }
}

/// `search::bm25::tokenize_symbol` is public and is consumed by
/// `search::vector`; it is also the documented tokenization contract a caller
/// must reproduce to build an equivalent query.
#[test]
fn tokenize_symbol_stays_public() {
    assert_eq!(
        bm25::tokenize_symbol("handle_tool_call"),
        "handle tool call"
    );
}

/// `search::bm25::indexes_doc_bodies` is the capability probe a caller needs to
/// tell a doc-content-capable index from one built before that field existed
/// (review round 2, finding 4). It is public because that question is asked
/// from outside this module.
#[test]
fn the_doc_body_capability_probe_stays_public() {
    // A directory holding no index answers `false` rather than panicking, which
    // is the contract an external caller depends on.
    assert!(!bm25::indexes_doc_bodies(std::path::Path::new(
        "/nonexistent/search_index/bm25"
    )));
}
