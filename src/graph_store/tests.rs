// graph_store tests — moved verbatim from graph_store.rs (Fowler "Move").
// No behavior change.

use super::*;

/// issue #143: when the store cannot be created because the target's
/// directory is unwritable (a full disk in production; a non-existent
/// parent as the portable seam here), `open_or_create` must fail with a
/// message that names the path and the OS-level write condition — not the
/// bare opaque lbug abort. Asserts the emission itself (§13.1-F1/A3).
#[test]
fn open_or_create_names_the_path_when_the_target_is_unwritable() {
    let target = Path::new("/no-such-root-ai-architect-143/inner/testdb");
    let err = GraphStore::open_or_create(target)
        .err()
        .expect("opening a store under a non-existent root must fail");
    assert!(
        err.contains("/no-such-root-ai-architect-143/inner"),
        "error must name the unwritable directory, got: {err}"
    );
    assert!(
        err.contains("write probe") && err.contains("failed"),
        "error must name the OS-level write condition, got: {err}"
    );
    // The original lbug text is still present for continuity.
    assert!(
        err.contains("lbug database open failed"),
        "underlying store error preserved, got: {err}"
    );
}

#[test]
fn test_create_and_query() {
    // source: issue #21 — a fixed `temp_dir().join("graph_store_test")`
    // path collides under default parallel `cargo test` execution (the
    // embedded DB's file lock races across test threads). tempfile::
    // TempDir allocates a unique-per-call directory (mirrors the #13-fix
    // pattern in tests/lbug_bulk_investigation.rs).
    let dir = tempfile::Builder::new()
        .prefix("graph_store_test")
        .tempdir()
        .expect("create temp dir");
    let db_path = dir.path().join("testdb");

    let store = GraphStore::open_or_create(&db_path).expect("open_or_create");
    store.create_schema().expect("create_schema");

    store
        .insert_node(
            NODE_FUNCTION,
            &[
                ("id", "'fn1'"),
                ("name", "'main'"),
                ("qualified_name", "'crate::main'"),
                ("start_line", "1"),
                ("end_line", "10"),
                ("visibility", "'pub'"),
                ("is_async", "false"),
            ],
        )
        .expect("insert Function node");

    let qr = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'main' RETURN f.name")
        .expect("query");
    assert_eq!(qr.columns, vec!["f.name"]);
    assert!(!qr.rows.is_empty(), "expected at least one row");
    assert_eq!(qr.rows[0][0], "main");

    let count = store.node_count().expect("node_count");
    assert!(count >= 1, "expected node_count >= 1, got {count}");
}

#[test]
fn test_bulk_insert_nodes_and_edges() {
    // source: issue #21 — unique-per-call TempDir; see test_create_and_query.
    let dir = tempfile::Builder::new()
        .prefix("graph_store_bulk_test")
        .tempdir()
        .expect("create temp dir");
    let db_path = dir.path().join("testdb");

    let store = GraphStore::open_or_create(&db_path).expect("open");
    store.create_schema().expect("schema");

    let mut files: Vec<Vec<(String, String)>> = Vec::new();
    for i in 0..7 {
        let id = format!("f{i}.rs");
        files.push(vec![
            ("id".into(), cypher_str(&id)),
            ("path".into(), cypher_str(&id)),
            ("name".into(), cypher_str(&id)),
            ("extension".into(), cypher_str("rs")),
            ("size_bytes".into(), "0".into()),
        ]);
    }
    let n = store.bulk_insert_nodes("File", &files).expect("bulk nodes");
    assert_eq!(n, 7);

    let mut edges: PropEdgeList = Vec::new();
    for i in 0..6 {
        edges.push((format!("f{i}.rs"), format!("f{}.rs", i + 1), Vec::new()));
    }
    let e = store
        .bulk_insert_edges("Imports_File_File", &edges)
        .expect("bulk edges");
    assert_eq!(e, 6);

    let qr = store
        .execute_query("MATCH (f:File) RETURN count(f)")
        .expect("count");
    let c: u64 = qr.rows[0][0].parse().unwrap_or(0);
    assert_eq!(c, 7);
}

#[test]
fn label_has_qualified_name_matches_the_schema() {
    // Contract: true for exactly the labels whose node table declares a
    // `qualified_name` column (mirrors node_column_types). A read-side
    // traversal binds `n.qualified_name` only when this is true, so a wrong
    // answer either drops rows (false-for-a-qn-label) or crashes the query
    // with a Binder exception (true-for-a-non-qn-label). Assert both arms.
    for yes in [
        NODE_MODULE,
        NODE_FUNCTION,
        NODE_METHOD,
        NODE_STRUCT,
        NODE_ENUM,
        NODE_VARIANT,
        NODE_TRAIT,
        NODE_CONSTANT,
        NODE_TYPE_ALIAS,
        NODE_VERSION,
        NODE_IAC_RESOURCE,
        NODE_IAC_MODULE,
    ] {
        assert!(
            label_has_qualified_name(yes),
            "{yes} declares qualified_name in its DDL"
        );
    }
    for no in [
        NODE_FILE,
        NODE_DIRECTORY,
        NODE_FIELD,
        NODE_IMPORT,
        NODE_CALL_SITE,
        NODE_COMMUNITY,
        NODE_PROCESS,
        NODE_STDLIB_SYMBOL,
        NODE_COMMIT,
        NODE_IAC_IMAGE,
    ] {
        assert!(
            !label_has_qualified_name(no),
            "{no} has no qualified_name column"
        );
    }
}

#[test]
fn test_cypher_str_escape_rules() {
    // Backslash must be escaped FIRST, then quote.
    // Input: `foo'bar`  → literal should contain `\'`.
    assert_eq!(cypher_str("foo'bar"), "'foo\\'bar'");
    // Input: `a\b`      → literal should contain `\\`.
    assert_eq!(cypher_str("a\\b"), "'a\\\\b'");
    // Input: `x\'y` (naive replace('\'', "\\'") would produce `'x\\\\'y'`
    // which re-opens the literal after `\\`). Our rule:
    //   \  -> \\     (escape backslash first)
    //   '  -> \'     (then escape quotes)
    // So `x\'y` becomes `x\\\'y` inside the quotes.
    assert_eq!(cypher_str("x\\'y"), "'x\\\\\\'y'");
}

#[test]
fn test_cypher_injection_rejected() {
    // An id containing an unescaped single quote used to allow a caller
    // to inject arbitrary Cypher (including DETACH DELETE). After the
    // C1 fix, the injection attempt becomes an ordinary string literal
    // that round-trips through the DB safely.
    // source: issue #21 — unique-per-call TempDir; see test_create_and_query.
    let dir = tempfile::Builder::new()
        .prefix("graph_store_inject_test")
        .tempdir()
        .expect("create temp dir");
    let db_path = dir.path().join("testdb");

    let store = GraphStore::open_or_create(&db_path).expect("open_or_create");
    store.create_schema().expect("create_schema");

    // Insert two File nodes so insert_edge has something to MATCH.
    store
        .insert_node(
            NODE_FILE,
            &[
                ("id", &cypher_str("src/a.rs")),
                ("path", &cypher_str("src/a.rs")),
                ("name", &cypher_str("a.rs")),
                ("extension", &cypher_str("rs")),
                ("size_bytes", "0"),
            ],
        )
        .expect("insert file a");

    // Adversarial id: contains `'` and would-be Cypher payload.
    let evil_id = "src/b.rs' DETACH DELETE n //";
    store
        .insert_node(
            NODE_FILE,
            &[
                ("id", &cypher_str(evil_id)),
                ("path", &cypher_str(evil_id)),
                ("name", &cypher_str("b.rs")),
                ("extension", &cypher_str("rs")),
                ("size_bytes", "0"),
            ],
        )
        .expect("insert file b");

    // Edge insert used to be the injection site. Must succeed safely now.
    store
        .insert_edge("Imports_File_File", "src/a.rs", evil_id, &[])
        .expect("insert_edge with quote-containing id must not inject");

    // If injection had worked, DETACH DELETE would have wiped nodes.
    // Verify both File nodes are still present.
    let cnt = store
        .execute_query("MATCH (f:File) RETURN count(f)")
        .expect("count query");
    let count_val: u64 = cnt.rows[0][0].parse().unwrap_or(0);
    assert_eq!(count_val, 2, "injection attempt must not delete nodes");
}

// -----------------------------------------------------------------
// issue #25 — max_db_size validation and the production default.
// Pure-function tests: no env var mutation, so these are safe under
// cargo test's default parallel (threaded) execution alongside every
// other test in this binary.
// -----------------------------------------------------------------

#[test]
fn max_db_size_rejects_non_numeric() {
    let err = parse_and_validate_max_db_size("not-a-number", "AP_LBUG_TEST_MAX_DB_SIZE")
        .expect_err("non-numeric value must be rejected");
    assert!(
        err.contains("AP_LBUG_TEST_MAX_DB_SIZE"),
        "error must name the offending var: {err}"
    );
}

#[test]
fn max_db_size_rejects_below_lbug_floor() {
    // 4 MiB is below the 8 MiB floor lbug's verifySizeParams enforces.
    let err = parse_and_validate_max_db_size("4194304", "AP_LBUG_MAX_DB_SIZE")
        .expect_err("below-floor value must be rejected");
    assert!(
        err.contains("minimum"),
        "error must explain the floor: {err}"
    );
}

#[test]
fn max_db_size_rejects_non_power_of_two() {
    // 8 MiB + 1 byte: above the floor but not a power of two.
    let err = parse_and_validate_max_db_size("8388609", "AP_LBUG_MAX_DB_SIZE")
        .expect_err("non-power-of-two value must be rejected");
    assert!(
        err.contains("power of two"),
        "error must explain the constraint: {err}"
    );
}

#[test]
fn max_db_size_accepts_valid_power_of_two() {
    // 512 MiB — the existing test bound (.cargo/config.toml).
    let bytes = parse_and_validate_max_db_size("536870912", "AP_LBUG_TEST_MAX_DB_SIZE")
        .expect("valid power-of-two above the floor must be accepted");
    assert_eq!(bytes, 536_870_912);
}

#[test]
// clippy::assertions_on_constants: both operands are `const` today, so
// clippy can prove this at lint time — but the whole point of this test
// is to keep proving it if a future edit changes either constant. A
// `const { assert!(..) }` block would only run at compile time (same
// blind spot); the ordinary runtime assert here is deliberate, not an
// oversight.
#[allow(clippy::assertions_on_constants)]
fn prod_default_is_valid_per_lbug_constraints() {
    // The production default itself must satisfy the exact constraints
    // parse_and_validate_max_db_size enforces for an operator-supplied
    // override, otherwise system_config()'s fallback would build a
    // SystemConfig that lbug's own verifySizeParams rejects at open time.
    assert!(
        DEFAULT_PROD_MAX_DB_SIZE_BYTES >= MIN_MAX_DB_SIZE_BYTES,
        "prod default must be at least lbug's 8 MiB floor"
    );
    assert_eq!(
        DEFAULT_PROD_MAX_DB_SIZE_BYTES & (DEFAULT_PROD_MAX_DB_SIZE_BYTES - 1),
        0,
        "prod default must be a power of two"
    );
    assert_eq!(
        DEFAULT_PROD_MAX_DB_SIZE_BYTES,
        8 * 1024 * 1024 * 1024,
        "prod default must be exactly 8 GiB"
    );
}

#[test]
fn prod_default_config_opens_a_real_database() {
    // Proves DEFAULT_PROD_MAX_DB_SIZE_BYTES is not just internally
    // consistent (prod_default_is_valid_per_lbug_constraints, above) but
    // actually accepted by lbug's C++ BufferManager::verifySizeParams.
    // Built directly via SystemConfig rather than through
    // system_config(), because .cargo/config.toml's [env] table sets
    // AP_LBUG_TEST_MAX_DB_SIZE for every cargo-spawned process (issue
    // #21/#24) and mutating that process-wide var at runtime here would
    // race other tests in this binary — see graph_cache.rs's
    // `prod_default_bound_opens_max_cached_graphs_simultaneously` for the
    // multi-open concurrency proof at this exact bound.
    let dir = tempfile::Builder::new()
        .prefix("graph_store_prod_default_test")
        .tempdir()
        .expect("create temp dir");
    let cfg = SystemConfig::default().max_db_size(DEFAULT_PROD_MAX_DB_SIZE_BYTES);
    let _store = GraphStore::open_or_create_with_config(&dir.path().join("testdb"), cfg)
        .expect("prod default max_db_size must be accepted by lbug");
}
