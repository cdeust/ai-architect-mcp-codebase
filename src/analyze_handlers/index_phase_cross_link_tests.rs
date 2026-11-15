//! Codex review of #523: three edits after which the refreshed graph differed
//! from a fresh analysis of the same tree. Each test compares the two.

use super::tests::{analyze, edge_rows, node_rows, outcome};
use crate::test_support::{TempDirExt, TestTempDir};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

fn temp_repo(prefix: &str) -> (PathBuf, TestTempDir) {
    let base = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let repo = base.join("repo");
    fs::create_dir_all(&repo).expect("mk repo");
    (repo, base)
}

#[test]
fn an_added_js_target_links_an_unchanged_importer() {
    // `a.js` (unchanged) imports `./util`, absent in the first run. A fresh
    // analysis after `util.js` is added draws `Imports_File_File` a.js →
    // util.js; the refresh drew the light links of the re-parsed files only.
    let (repo, base) = temp_repo("analyze523_js_");
    fs::write(repo.join("a.js"), "import { u } from \"./util\";\nu();\n").expect("a.js");
    fs::write(repo.join("lib.rs"), "pub fn root() {}\n").expect("lib.rs");
    let auto = json!({ "language": "auto" });
    let out = base.join("out");
    analyze(&repo, &out, auto.clone());

    fs::write(repo.join("util.js"), "export function u() {}\n").expect("add util.js");
    let second = analyze(&repo, &out, auto.clone());
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    assert!(
        edge_rows(&second).contains(&"Imports_File_File|a.js|util.js".to_string()),
        "the unchanged importer links the added file"
    );
    let fresh = analyze(&repo, &base.join("fresh"), auto);
    assert_eq!(outcome(&second), outcome(&fresh));
}

const DEPLOYMENT: &str = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  \
     template:\n    spec:\n      containers:\n        - name: web\n          image: web:latest\n          \
     envFrom:\n            - configMapRef:\n                name: app-config\n";

/// The name-match edge from the unchanged Deployment to `configmap.yaml`.
const CONFIG_MAP_REFERENCE: &str =
    "Imports_IacResource_File|deployment.yaml::k8s::0|configmap.yaml";

fn config_map(name: &str) -> String {
    format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {name}\ndata:\n  k: v\n")
}

#[test]
fn a_renamed_config_map_drops_the_unchanged_reference_without_a_rebuild() {
    // Only the ConfigMap's `metadata.name` changes: the unchanged Deployment's
    // name-match edge no longer holds. The refresh draws the IaC layer again
    // instead of rebuilding (issue #443; the rebuild of #527).
    let (repo, base) = temp_repo("analyze443_iac_rename_");
    fs::write(repo.join("deployment.yaml"), DEPLOYMENT).expect("deployment.yaml");
    fs::write(repo.join("configmap.yaml"), config_map("app-config")).expect("configmap.yaml");
    let auto = json!({ "language": "auto" });
    let out = base.join("out");
    let first = analyze(&repo, &out, auto.clone());
    assert!(
        edge_rows(&first).iter().any(|r| r == CONFIG_MAP_REFERENCE),
        "the first run draws the name-match edge: {:?}",
        edge_rows(&first)
    );

    fs::write(repo.join("configmap.yaml"), config_map("other")).expect("rename");
    let second = analyze(&repo, &out, auto.clone());
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    assert!(!edge_rows(&second).iter().any(|r| r == CONFIG_MAP_REFERENCE));
    let fresh = analyze(&repo, &base.join("fresh"), auto);
    assert_eq!(outcome(&second), outcome(&fresh));
}

#[test]
fn an_added_config_map_gives_the_unchanged_deployment_its_reference() {
    // The Deployment names `app-config`, absent in the first run. A fresh
    // analysis after `configmap.yaml` is added draws the name-match edge from
    // the unchanged Deployment; the reparsed-only IaC step did not.
    let (repo, base) = temp_repo("analyze443_iac_add_");
    fs::write(repo.join("deployment.yaml"), DEPLOYMENT).expect("deployment.yaml");
    let auto = json!({ "language": "auto" });
    let out = base.join("out");
    let first = analyze(&repo, &out, auto.clone());
    assert!(!edge_rows(&first).iter().any(|r| r == CONFIG_MAP_REFERENCE));

    fs::write(repo.join("configmap.yaml"), config_map("app-config")).expect("add");
    let second = analyze(&repo, &out, auto.clone());
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    assert!(
        edge_rows(&second).iter().any(|r| r == CONFIG_MAP_REFERENCE),
        "the unchanged Deployment references the added ConfigMap: {:?}",
        edge_rows(&second)
    );
    let fresh = analyze(&repo, &base.join("fresh"), auto);
    assert_eq!(outcome(&second), outcome(&fresh));
}

#[test]
fn a_deleted_base_overlay_drops_the_unchanged_kustomize_reference() {
    // `overlay/kustomization.yaml` (unchanged) names `../base`, whose
    // Kustomization the second run deletes: the module-to-module edge goes,
    // and the reference falls back to what a fresh analysis draws.
    let (repo, base) = temp_repo("analyze443_iac_kustomize_");
    fs::create_dir_all(repo.join("base")).expect("mk base");
    fs::create_dir_all(repo.join("overlay")).expect("mk overlay");
    fs::write(
        repo.join("base/kustomization.yaml"),
        "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nresources:\n  - configmap.yaml\n",
    )
    .expect("base kustomization");
    fs::write(repo.join("base/configmap.yaml"), config_map("app-config")).expect("base cm");
    fs::write(
        repo.join("overlay/kustomization.yaml"),
        "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nresources:\n  - ../base\n",
    )
    .expect("overlay kustomization");
    let auto = json!({ "language": "auto" });
    let out = base.join("out");
    let first = analyze(&repo, &out, auto.clone());
    let module_edge = "Imports_IacModule_IacModule|overlay/kustomization.yaml::kustomize|base/kustomization.yaml::kustomize";
    assert!(
        edge_rows(&first).iter().any(|r| r == module_edge),
        "the first run draws the base edge: {:?}",
        edge_rows(&first)
    );

    fs::remove_file(repo.join("base/kustomization.yaml")).expect("delete base");
    let second = analyze(&repo, &out, auto.clone());
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    assert!(!edge_rows(&second).iter().any(|r| r == module_edge));
    let fresh = analyze(&repo, &base.join("fresh"), auto);
    assert_eq!(outcome(&second), outcome(&fresh));
}

#[test]
fn a_site_resolved_without_a_call_row_reopens_when_its_type_goes() {
    // A Go conversion `Count(1)` resolves to the type and writes a `Uses_*`
    // row, no per-site `Calls_*` row. Renamed, the type no longer resolves the
    // site; the retraction reopened only sites with a static `Calls_*` row, so
    // the refreshed site kept `is_resolved = true`.
    let (repo, base) = temp_repo("analyze523_go_");
    fs::write(repo.join("go.mod"), "module example.com/m\n\ngo 1.21\n").expect("go.mod");
    fs::write(repo.join("model.go"), "package m\n\ntype Count int\n").expect("model.go");
    fs::write(
        repo.join("use.go"),
        "package m\n\nfunc total() int {\n\treturn int(Count(1))\n}\n",
    )
    .expect("use.go");
    let go = json!({ "language": "go" });
    let out = base.join("out");
    let first = analyze(&repo, &out, go.clone());
    let rowless: Vec<String> = node_rows(&first)
        .into_iter()
        .filter(|r| r.starts_with("CallSite|") && r.ends_with("|true"))
        .filter(|r| {
            let id = r.split('|').nth(1).unwrap_or("");
            !edge_rows(&first)
                .iter()
                .any(|e| e.starts_with("Calls_CallSite_") && e.split('|').nth(1) == Some(id))
        })
        .collect();
    assert!(
        !rowless.is_empty(),
        "the first run resolves a site without a per-site row: {:?}",
        node_rows(&first)
    );

    fs::write(repo.join("model.go"), "package m\n\ntype Total int\n").expect("rename type");
    let second = analyze(&repo, &out, go.clone());
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    let fresh = analyze(&repo, &base.join("fresh"), go);
    assert_eq!(outcome(&second), outcome(&fresh));
}

#[test]
fn a_site_resolved_through_a_type_an_added_namesake_makes_ambiguous_matches_a_fresh_analysis() {
    // The type survives: the `Uses_*` row is still there when the resolver
    // runs, and a second `Count` added elsewhere may change its decision
    // (`resolver::retract::type_resolved_sites`).
    let (repo, base) = temp_repo("analyze523_go_ambiguous_");
    fs::write(repo.join("go.mod"), "module example.com/m\n\ngo 1.21\n").expect("go.mod");
    fs::write(repo.join("model.go"), "package m\n\ntype Count int\n").expect("model.go");
    fs::write(
        repo.join("use.go"),
        "package m\n\nfunc total() int {\n\treturn int(Count(1))\n}\n",
    )
    .expect("use.go");
    let go = json!({ "language": "go" });
    let out = base.join("out");
    analyze(&repo, &out, go.clone());
    fs::create_dir_all(repo.join("other")).expect("mk other");
    fs::write(
        repo.join("other/count.go"),
        "package other\n\ntype Count int\n",
    )
    .expect("add");
    let second = analyze(&repo, &out, go.clone());
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    let fresh = analyze(&repo, &base.join("fresh"), go);
    assert_eq!(outcome(&second), outcome(&fresh));
}
