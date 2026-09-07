//! Real indexed-import contracts from the promise audit.
use ai_architect_mcp::{graph_store::GraphStore, indexer, resolver, security_gates};
use std::{fs, path::Path};

fn fixture(root: &Path) -> GraphStore {
    let src = root.join("repo");
    fs::create_dir_all(src.join("src/nested")).unwrap();
    fs::write(src.join("src/lib.rs"), "mod good;\nuse good::helper;\nuse missing_one::x;\nuse missing_two::y;\npub fn risky() {}\npub struct Widget;\nimpl Widget { pub fn run(&self) {} }\n").unwrap();
    fs::write(src.join("src/good.rs"), "pub fn helper() {}\n").unwrap();
    fs::write(
        src.join("src/nested/service.rs"),
        "use missing_three::z;\npub fn nested() {}\n",
    )
    .unwrap();
    let graph = root.join("graph");
    indexer::index_codebase(&src, &graph).unwrap();
    let store = GraphStore::open_or_create(&graph).unwrap();
    resolver::resolve_graph(&store).unwrap();
    store
}

#[test]
fn unresolved_imports_are_scoped_by_full_file_identity_and_status() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(root.path());
    for (symbol, count, severity) in [
        ("src/lib.rs::risky", 2, "critical"),
        ("src/nested/service.rs::nested", 1, "warning"),
        ("src/lib.rs::Widget::run", 2, "critical"),
    ] {
        let report = security_gates::check_gates(&store, &[symbol.into()]).unwrap();
        let flag = report
            .flags
            .iter()
            .find(|f| f.gate == "unresolved_imports")
            .unwrap_or_else(|| panic!("missing unresolved-import flag for {symbol}"));
        assert_eq!(flag.details["unresolved_count"], count);
        assert_eq!(flag.severity, severity);
        if severity == "critical" {
            assert!(!report.gates_passed);
        }
    }
    let clean = security_gates::check_gates(&store, &["src/good.rs::helper".into()]).unwrap();
    assert!(!clean.flags.iter().any(|f| f.gate == "unresolved_imports"));
}

#[test]
fn broken_import_schema_is_not_treated_as_no_security_risk() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(root.path());
    store
        .execute_query("ALTER TABLE Import DROP is_resolved")
        .unwrap();
    let error = match security_gates::check_gates(&store, &["src/lib.rs::risky".into()]) {
        Ok(_) => panic!("an unavailable import check must fail explicitly"),
        Err(error) => error,
    };
    assert!(error.contains("unresolved imports"), "{error}");
}

#[test]
fn skipped_or_unknown_checks_cannot_claim_complete_assessment() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(root.path());
    for symbol in ["src/good.rs::helper", "unknown_symbol"] {
        let symbols = vec![symbol.to_owned()];
        let report = security_gates::check_gates(&store, &symbols).unwrap();
        let json = security_gates::report_to_json(&report, "", "", root.path(), &symbols, "fixed");
        assert_eq!(json["assessment_complete"], false);
    }
}

#[cfg(unix)]
#[test]
fn delimiter_in_filename_does_not_cross_count_imports() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("repo");
    fs::create_dir_all(src.join("src")).unwrap();
    fs::write(src.join("src/a.rs"), "pub fn clean() {}\n").unwrap();
    fs::write(
        src.join("src/a.rs::b.rs"),
        "use missing_one::x;\nuse missing_two::y;\npub fn risky() {}\n",
    )
    .unwrap();
    let graph = root.path().join("graph");
    indexer::index_codebase(&src, &graph).unwrap();
    let store = GraphStore::open_or_create(&graph).unwrap();
    resolver::resolve_graph(&store).unwrap();
    let clean = security_gates::check_gates(&store, &["src/a.rs::clean".into()]).unwrap();
    assert!(!clean.flags.iter().any(|f| f.gate == "unresolved_imports"));
    let risky = security_gates::check_gates(&store, &["src/a.rs::b.rs::risky".into()]).unwrap();
    let flag = risky
        .flags
        .iter()
        .find(|f| f.gate == "unresolved_imports")
        .unwrap();
    assert_eq!(flag.details["unresolved_count"], 2);
    assert_eq!(flag.severity, "critical");
    assert!(!risky.gates_passed);
}

#[test]
fn no_changed_symbols_does_not_claim_complete_assessment() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(root.path());
    let report = security_gates::check_gates(&store, &[]).unwrap();
    assert_eq!(report.summary.changed_symbols, 0);
    assert!(
        report.gates_passed,
        "zero critical flags retains its existing meaning"
    );
    assert!(!report.assessment_complete());
    let json = security_gates::report_to_json(&report, "", "", root.path(), &[], "fixed");
    assert_eq!(json["assessment_complete"], false);
}
