use super::*;
use crate::tool_profile::ToolProfile;
use std::path::Path;

/// An indexed one-file crate and its README, through the dispatch table.
fn fixture(tmp: &Path) -> (String, String) {
    let code = tmp.join("code");
    std::fs::create_dir_all(code.join("src")).expect("src");
    std::fs::write(
        code.join("Cargo.toml"),
        "[package]\nname = \"fxdoc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("manifest");
    std::fs::write(
        code.join("src/lib.rs"),
        "pub fn f() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n",
    )
    .expect("lib");
    std::fs::write(code.join("README.md"), "Exports `f`.\nIt has 1 test.\n").expect("readme");
    let out = tmp.join("out");
    let response = crate::dispatch_tool(
        "index_codebase",
        &json!({ "path": code.to_string_lossy(), "output_dir": out.to_string_lossy(),
            "language": "rust" }),
        ToolProfile::Full,
    )
    .expect("registered");
    assert_eq!(response["status"], "ok", "{response}");
    (
        out.join("graph").to_string_lossy().to_string(),
        code.to_string_lossy().to_string(),
    )
}

fn call(args: Value) -> Value {
    crate::dispatch_tool("check_doc_claims", &args, ToolProfile::Full).expect("registered")
}

#[test]
fn compact_output_counts_everything_and_lists_only_what_is_not_supported() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (graph, root) = fixture(tmp.path());
    let claims = json!([
        { "text": "`f`", "file": "README.md", "line": 1, "kind": "is_public", "subject": "f" },
        { "id": "t", "text": "1 test", "file": "README.md", "line": 2, "kind": "test_count",
          "expected": 1 },
        { "id": "x", "text": "`f`", "file": "README.md", "line": 2, "kind": "symbol_exists",
          "subject": "f" }
    ]);
    let args = json!({ "graph_path": graph, "repo_root": root, "claims": claims });
    let out = call(args.clone());
    assert_eq!(out["status"], "ok", "{out}");
    assert_eq!(out["claim_count"], 3);
    assert_eq!(out["counts"]["supported"], 2, "{out}");
    assert_eq!(out["counts"]["rejected_anchor"], 1, "{out}");
    assert_eq!(out["rows"].as_array().expect("rows").len(), 1);
    assert_eq!(out["rows"][0]["id"], "x");

    // Deterministic to the byte: no clock, one order.
    assert_eq!(out.to_string(), call(args.clone()).to_string());

    let mut full = args;
    full["detail"] = json!("full");
    full["format"] = json!("tabular");
    let out = call(full);
    assert_eq!(out["rows"].as_array().expect("rows").len(), 3);
    assert_eq!(out["columns"][7], "verdict");
    assert_eq!(out["rows"][0][0], "c0");
}

#[test]
fn malformed_input_is_refused_with_the_field_named() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (graph, root) = fixture(tmp.path());
    let base = |claims: Value| json!({ "graph_path": graph, "repo_root": root, "claims": claims });
    for (claims, needle) in [
        (
            json!([{ "file": "README.md", "line": 1, "kind": "symbol_exists" }]),
            "missing 'text'",
        ),
        (
            json!([{ "text": "a", "file": "README.md", "line": 0, "kind": "k" }]),
            "'line'",
        ),
        (
            json!([{ "text": "a", "file": "README.md", "line": 1, "kind": "k", "x": 1 }]),
            "unknown field 'x'",
        ),
        (json!("nope"), "claims"),
    ] {
        let out = call(base(claims));
        assert_eq!(out["status"], "error", "{out}");
        assert!(
            out["message"].as_str().expect("msg").contains(needle),
            "{out}"
        );
    }
    let out = call(json!({ "graph_path": graph, "repo_root": "relative", "claims": [] }));
    assert_eq!(out["status"], "error");
    let mut bad_detail = base(json!([]));
    bad_detail["detail"] = json!("ids");
    assert_eq!(call(bad_detail)["status"], "error");
}
