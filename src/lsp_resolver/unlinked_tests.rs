//! Issue #292. Drives `resolve_with_client` against a fake server that
//! advertises pull diagnostics (`tests/fixtures/lsp/fake_server_pull_diagnostics.py`)
//! over four files covering every cell of the cargo × rust-analyzer grid.
//! source: ADR-9845.

use super::outside_targets_tests::insert_caller_and_site;
use super::*;
use crate::indexer::cargo_targets;
use crate::lsp_client::{CargoAttribution, LspClient};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const FILES: [(&str, &str); 4] = [
    ("src/lib.rs", "inside"),
    ("src/orphan.rs", "orphan"),
    ("kani/g.rs", "agreed"),
    ("kani/h.rs", "contradicted"),
];

fn fixture(dir: &Path) -> (PathBuf, GraphStore) {
    let root = std::fs::canonicalize(dir).expect("canonicalize");
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::create_dir_all(root.join("kani")).expect("mkdir kani");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"unlinked_fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    let store = GraphStore::open_or_create(&root.join("db")).expect("open");
    store.create_schema().expect("schema");
    for (file, name) in FILES {
        std::fs::write(root.join(file), format!("fn {name}() {{}}\n")).expect("write");
        insert_caller_and_site(&store, file, name);
    }
    (root, store)
}

fn spawn_fake(root: &Path, log: &Path) -> LspClient {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/lsp/fake_server_pull_diagnostics.py"
    );
    let args = [
        script,
        log.to_str().expect("utf8"),
        "/src/orphan.rs",
        "/kani/g.rs",
    ];
    LspClient::start_unchecked("python3", &args, root, Duration::from_secs(10)).expect("spawn")
}

fn run_pass(root: &Path, store: &GraphStore, log: &Path) -> LspResolutionResult {
    let unresolved = collect_unresolved_callsites(store).expect("collect");
    let target_map = cargo_targets::discover(root);
    let node_index = build_node_position_index(store).expect("index");
    let plan = PassPlan {
        codebase_path: root,
        language: "rust",
        deadline: Duration::from_secs(5),
        start: Instant::now(),
        ctx: edges::SiteContext {
            node_index: &node_index,
            canonical_root: root,
        },
        target_map: &target_map,
    };
    let mut client = spawn_fake(root, log);
    let pass = resolve_with_client(store, &mut client, &plan, &unresolved).expect("healthy");
    let result = pass.into_result(0, client.server_health().clone());
    let _ = client.shutdown();
    result
}

#[test]
fn unlinked_verdicts_are_reported_against_the_cargo_attribution_in_both_directions() {
    if !lsp_client::is_command_available("python3") || !lsp_client::is_command_available("cargo") {
        eprintln!("skipping: python3 and cargo are both needed by this fixture");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, store) = fixture(dir.path());
    let log = dir.path().join("requests.log");
    let result = run_pass(&root, &store, &log);
    let check = &result.unlinked_check;

    assert!(check.pull_supported);
    assert_eq!(
        check.files_checked, 4,
        "every file the pass reached is pulled once"
    );
    let unlinked: Vec<(&str, CargoAttribution)> = check
        .unlinked
        .iter()
        .map(|f| (f.rel_path.as_str(), f.cargo_attribution))
        .collect();
    assert_eq!(
        unlinked,
        vec![
            ("kani/g.rs", CargoAttribution::OutsideBuildTargets),
            ("src/orphan.rs", CargoAttribution::InsideBuildTargets),
        ],
        "agreement and cargo-says-inside disagreement are both reported"
    );
    assert_eq!(
        check.linked_despite_outside_targets,
        vec!["kani/h.rs".to_string()]
    );
    assert_eq!(
        result.outside_targets_count, 2,
        "#284 attribution is unchanged"
    );

    let events = std::fs::read_to_string(&log).expect("read log");
    for outside in ["kani/g.rs", "kani/h.rs"] {
        let uri = lsp_client::path_to_file_uri(&root.join(outside));
        assert!(
            !events.contains(&format!("definition_requested:{uri}")),
            "#284: an outside-target file still never gets a definition request: {events}"
        );
    }
}
