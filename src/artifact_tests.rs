// Unit tests for `artifact` (issue #55). Split into its own file so
// `src/artifact.rs` stays under the 500-line limit (coding-standards §4.1);
// included as a private child module via `#[path]`, so `super::*` still reaches
// the module's private items (`read_meta`, `write_meta`, `is_hex_sha`, …).

use super::*;

#[test]
fn bundled_manifest_entry_name_matches_the_indexer_manifest_filename() {
    // The archive stores the manifest under a hardcoded name so `artifact` need
    // not import the indexer. This guard keeps the two in lockstep: if the
    // indexer ever renames its manifest, the bundled entry would land in the
    // wrong place on import and the incremental fill would silently miss it.
    assert_eq!(
        ARTIFACT_MANIFEST_ENTRY,
        crate::indexer::manifest::MANIFEST_FILE,
        "bundled manifest entry name must equal the indexer's manifest filename"
    );
    assert_eq!(
        ARTIFACT_COVERAGE_ENTRY,
        crate::indexer::coverage::COVERAGE_FILE,
        "bundled coverage entry name must equal the indexer's coverage filename"
    );
}

#[test]
fn export_bundles_manifest_and_import_restores_it_beside_the_graph() {
    // Export with a manifest → the archive carries it → import into a fresh
    // output dir materialises both the graph AND file_manifest.json as siblings,
    // exactly where the incremental classifier looks.
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    let graph = out.join("graph");
    fs::create_dir_all(&graph).expect("mk graph");
    fs::write(graph.join("data.bin"), b"g").expect("write graph file");
    fs::create_dir_all(&repo).expect("mk repo");
    let manifest_path = out.join("file_manifest.json");
    fs::write(&manifest_path, b"{\"schema_version\":1,\"files\":{}}").expect("write manifest");

    export_artifact(&graph, &repo, 1, 1, Some(&manifest_path), None).expect("export");

    let fresh = tmp.path().join("fresh");
    let fresh_graph = fresh.join("graph");
    fs::create_dir_all(&fresh).expect("mk fresh");
    import_artifact(&repo, &fresh_graph).expect("import");
    assert!(fresh_graph.exists(), "graph must be restored");
    assert!(
        fresh.join("file_manifest.json").exists(),
        "bundled manifest must be restored beside the graph"
    );
}

#[test]
fn export_import_round_trips_a_directory_graph() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    let graph = out.join("graph");
    fs::create_dir_all(graph.join("sub")).expect("mk graph");
    fs::write(graph.join("a.bin"), b"payload-a").expect("write a");
    fs::write(graph.join("sub/b.bin"), b"payload-b").expect("write b");
    fs::create_dir_all(&repo).expect("mk repo");

    let stats = export_artifact(&graph, &repo, 3, 5, None, None).expect("export should succeed");
    assert!(stats.compressed_bytes > 0);
    assert!(artifact_exists(&repo));

    // Simulate a fresh clone: fresh output dir, no local graph.
    let fresh = tmp.path().join("fresh");
    let fresh_graph = fresh.join("graph");
    let meta = import_artifact(&repo, &fresh_graph).expect("import should succeed");
    assert_eq!(meta.node_count, 3);
    assert_eq!(meta.edge_count, 5);
    assert_eq!(meta.compression_level, ZSTD_LEVEL);

    assert_eq!(fs::read(fresh_graph.join("a.bin")).unwrap(), b"payload-a");
    assert_eq!(
        fs::read(fresh_graph.join("sub/b.bin")).unwrap(),
        b"payload-b"
    );
}

#[test]
fn gitattributes_entry_is_created_once_and_not_duplicated() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let graph = tmp.path().join("out/graph");
    fs::create_dir_all(&graph).expect("mk graph");
    fs::write(graph.join("x.bin"), b"x").expect("write x");
    fs::create_dir_all(&repo).expect("mk repo");

    export_artifact(&graph, &repo, 1, 0, None, None).expect("first export");
    export_artifact(&graph, &repo, 1, 0, None, None).expect("second export");

    let ga = fs::read_to_string(repo.join(".gitattributes")).expect("read gitattributes");
    let entry = format!("{ARTIFACT_DIR}/{ARTIFACT_FILE} binary merge=ours");
    let count = ga.lines().filter(|l| l.trim() == entry).count();
    assert_eq!(count, 1, "entry must appear exactly once, got:\n{ga}");
}

#[test]
fn artifact_exists_is_false_without_export() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(!artifact_exists(tmp.path()));
}

#[test]
fn import_refuses_newer_schema() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let graph = tmp.path().join("out/graph");
    fs::create_dir_all(&graph).expect("mk graph");
    fs::write(graph.join("x.bin"), b"x").expect("write x");
    fs::create_dir_all(&repo).expect("mk repo");
    export_artifact(&graph, &repo, 1, 1, None, None).expect("export");

    // Rewrite the sidecar with a future schema version.
    let mut meta = read_meta(&repo).expect("read meta");
    meta.schema_version = SCHEMA_VERSION + 1;
    write_meta(&repo, &meta).expect("rewrite meta");
    assert!(!artifact_exists(&repo));

    let err = import_artifact(&repo, &tmp.path().join("fresh/graph"))
        .expect_err("must refuse newer schema");
    assert!(err.contains("newer than supported"), "got: {err}");
}

#[test]
fn staleness_is_none_outside_a_git_repo() {
    // No git working tree → HEAD unavailable → staleness is not meaningful.
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(artifact_staleness(tmp.path(), "").is_none());
    assert!(artifact_staleness(tmp.path(), "deadbeef").is_none());
}

#[test]
fn is_hex_sha_guards_arg_injection() {
    assert!(is_hex_sha("0a1b2c3d4e5f"));
    assert!(!is_hex_sha("")); // absent provenance
    assert!(!is_hex_sha("--all")); // git flag smuggling attempt
    assert!(!is_hex_sha("HEAD~1")); // non-hex revision expression
}

// -- issue #195: legacy artifact-dir migration ------------------------------

#[test]
fn migrate_legacy_dir_renames_in_place() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let legacy = repo.join(LEGACY_ARTIFACT_DIR);
    fs::create_dir_all(&legacy).expect("mk legacy dir");
    fs::write(legacy.join(ARTIFACT_FILE), b"payload").expect("write legacy artifact");

    migrate_legacy_dir(&repo);

    assert!(!legacy.exists(), "legacy dir must be gone after migration");
    let current = repo.join(ARTIFACT_DIR);
    assert!(current.is_dir(), "current dir must exist after migration");
    assert_eq!(
        fs::read(current.join(ARTIFACT_FILE)).expect("read migrated artifact"),
        b"payload"
    );
}

#[test]
fn migrate_legacy_dir_is_a_noop_without_a_legacy_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).expect("mk repo");
    migrate_legacy_dir(&repo); // must not panic or create anything
    assert!(!repo.join(ARTIFACT_DIR).exists());
    assert!(!repo.join(LEGACY_ARTIFACT_DIR).exists());
}

#[test]
fn migrate_legacy_dir_never_clobbers_an_existing_current_dir() {
    // Both present (e.g. a re-export already happened under the new name
    // before an old .automatised-pipeline leftover was cleaned up): the
    // current dir wins untouched, and the legacy one is left for the caller
    // to notice — silently deleting it would be worse than a stray directory.
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let legacy = repo.join(LEGACY_ARTIFACT_DIR);
    let current = repo.join(ARTIFACT_DIR);
    fs::create_dir_all(&legacy).expect("mk legacy dir");
    fs::write(legacy.join(ARTIFACT_FILE), b"stale").expect("write legacy artifact");
    fs::create_dir_all(&current).expect("mk current dir");
    fs::write(current.join(ARTIFACT_FILE), b"fresh").expect("write current artifact");

    migrate_legacy_dir(&repo);

    assert!(
        legacy.exists(),
        "legacy dir must be left alone, not deleted"
    );
    assert_eq!(
        fs::read(current.join(ARTIFACT_FILE)).expect("read current artifact"),
        b"fresh",
        "current dir must not be clobbered"
    );
}

#[test]
fn export_artifact_migrates_a_legacy_dir_before_writing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let graph = tmp.path().join("out/graph");
    fs::create_dir_all(&graph).expect("mk graph");
    fs::write(graph.join("x.bin"), b"x").expect("write x");
    fs::create_dir_all(repo.join(LEGACY_ARTIFACT_DIR)).expect("mk legacy dir");

    export_artifact(&graph, &repo, 1, 0, None, None).expect("export");

    assert!(artifact_exists(&repo));
    assert!(!repo.join(LEGACY_ARTIFACT_DIR).exists());
}
