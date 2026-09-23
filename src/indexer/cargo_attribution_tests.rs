// Tests for `cargo_attribution` (issue #316).

use super::*;
use std::fs;
use std::process::Command;

fn cargo_available() -> bool {
    Command::new("cargo").arg("--version").output().is_ok()
}

fn rust_files(rels: &[&str]) -> BTreeSet<PathBuf> {
    rels.iter().map(PathBuf::from).collect()
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, body).expect("write");
}

const CRATE_MANIFEST: &str =
    "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

fn crate_with_kani_harness(root: &Path) {
    write(root, "Cargo.toml", CRATE_MANIFEST);
    write(root, "src/lib.rs", "pub fn f() {}\n");
    write(root, "kani/h.rs", "fn proof() { probe::f(); }\n");
}

#[test]
fn no_cargo_toml_is_not_applicable_and_spawns_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(dir.path(), "src/lib.rs", "pub fn f() {}\n");
    let out = attribute(dir.path(), &rust_files(&["src/lib.rs"]));
    assert_eq!(out.status.as_str(), "not_applicable");
    assert_eq!(
        out.status.detail(),
        Some("no Cargo.toml at the analyzed root")
    );
}

#[test]
fn a_manifest_without_rust_files_is_not_applicable() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(dir.path(), "Cargo.toml", CRATE_MANIFEST);
    let out = attribute(dir.path(), &BTreeSet::new());
    assert_eq!(out.status.as_str(), "not_applicable");
}

/// The #316 shape: a crate nested under a parent `[workspace] members = []`
/// makes `cargo metadata` exit non-zero. The status must say `unknown` and
/// carry cargo's own words, and both buckets must stay empty.
#[test]
fn a_nested_crate_cargo_refuses_is_unknown_with_cargos_stderr() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH (cargo_targets::discover needs it)");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    write(dir.path(), "Cargo.toml", "[workspace]\nmembers = []\n");
    let child = dir.path().join("child");
    crate_with_kani_harness(&child);
    let out = attribute(&child, &rust_files(&["src/lib.rs", "kani/h.rs"]));
    let CargoAttributionStatus::Unknown { detail } = &out.status else {
        panic!("expected unknown, got {:?}", out.status);
    };
    assert!(
        detail.contains("believes it's in a workspace"),
        "detail must carry cargo's stderr, got {detail}"
    );
    assert!(out.outside_targets.is_empty() && out.feature_gated.is_empty());
}

#[test]
fn a_crate_cargo_loads_is_known_and_attributes_the_harness() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH (cargo_targets::discover needs it)");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    // Its own `[workspace]` table keeps cargo from walking up into any
    // manifest above the temp dir.
    crate_with_kani_harness(dir.path());
    let manifest = dir.path().join("Cargo.toml");
    let body = fs::read_to_string(&manifest).expect("read") + "\n[workspace]\n";
    fs::write(&manifest, body).expect("write");
    let out = attribute(dir.path(), &rust_files(&["src/lib.rs", "kani/h.rs"]));
    assert_eq!(out.status, CargoAttributionStatus::Known);
    assert_eq!(out.outside_targets, ["kani/h.rs"]);
}

#[test]
fn the_status_serializes_as_a_tagged_object() {
    let unknown = CargoAttributionStatus::Unknown {
        detail: "cargo not found on PATH".into(),
    };
    assert_eq!(
        serde_json::to_value(&unknown).expect("encode"),
        serde_json::json!({"status": "unknown", "detail": "cargo not found on PATH"})
    );
    assert_eq!(
        serde_json::to_value(CargoAttributionStatus::Known).expect("encode"),
        serde_json::json!({"status": "known"})
    );
}
