// iac_indexing — issue #63 end-to-end gate.
//
// External proof (§13 G3) that the infrastructure-as-code pass turns a realistic
// deployment surface into graph material and that every acceptance path is
// asserted against a concrete fixture:
//   1. Full index of a realistic k8s app → EXACT expected IaC nodes/edges.
//   2. A broken (Helm-templated) manifest → coverage-flagged, index survives.
//   3. Incremental edit of one manifest → only it re-processed; the others'
//      IaC nodes are untouched.
//   4. get_impact traverses the new edges in both directions across the
//      IaC/code boundary.
//   5. query_graph reaches the new labels.

use ai_architect_mcp::clustering::get_impact;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::coverage::{self, CoverageKind};
use ai_architect_mcp::indexer::{self, manifest, IndexOptions};
use std::fs;
use std::path::Path;

/// A source file the Dockerfile COPYs (proves the COPY-source edge) + the
/// Dockerfile that builds `web` from it.
fn write_dockerfile(root: &Path) {
    fs::write(
        root.join("services/web/app.js"),
        "export const run = () => 1;\n",
    )
    .unwrap();
    fs::write(
        root.join("services/web/Dockerfile"),
        "FROM node:18\nWORKDIR /app\nCOPY app.js /app/app.js\nEXPOSE 8080\nCMD [\"node\", \"/app/app.js\"]\n",
    )
    .unwrap();
}

/// The base overlay: Deployment (image web:latest, envFrom app-config),
/// Service, ConfigMap, and the kustomization that composes them.
fn write_base_manifests(root: &Path) {
    fs::write(
        root.join("k8s/base/deployment.yaml"),
        "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  template:\n    spec:\n      containers:\n        - name: web\n          image: web:latest\n          envFrom:\n            - configMapRef:\n                name: app-config\n",
    )
    .unwrap();
    fs::write(
        root.join("k8s/base/service.yaml"),
        "apiVersion: v1\nkind: Service\nmetadata:\n  name: web\nspec:\n  selector:\n    app: web\n  ports:\n    - port: 80\n",
    )
    .unwrap();
    fs::write(
        root.join("k8s/base/configmap.yaml"),
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: app-config\ndata:\n  LOG_LEVEL: info\n",
    )
    .unwrap();
    fs::write(
        root.join("k8s/base/kustomization.yaml"),
        "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nresources:\n  - deployment.yaml\n  - service.yaml\n  - configmap.yaml\n",
    )
    .unwrap();
}

/// The prod overlay: composes the base overlay + patches it.
fn write_prod_overlay(root: &Path) {
    fs::write(
        root.join("k8s/overlays/prod/kustomization.yaml"),
        "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nresources:\n  - ../../base\npatches:\n  - path: patch.yaml\n",
    )
    .unwrap();
    fs::write(
        root.join("k8s/overlays/prod/patch.yaml"),
        "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  replicas: 3\n",
    )
    .unwrap();
}

/// A Helm-templated manifest: looks like k8s but is unresolved — a parse gap.
fn write_broken_manifest(root: &Path) {
    fs::write(
        root.join("broken/templated.yaml"),
        "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: {{ .Values.name }}\nspec:\n  replicas: {{ .Values.replicas }}\n",
    )
    .unwrap();
}

/// Writes a realistic single-service k8s app: a Dockerfile that builds `web`,
/// a base overlay (Deployment + Service + ConfigMap + kustomization), a prod
/// overlay that patches the base, and one Helm-templated file that must be
/// reported as a parse gap.
fn write_fixture(root: &Path) {
    fs::create_dir_all(root.join("services/web")).unwrap();
    fs::create_dir_all(root.join("k8s/base")).unwrap();
    fs::create_dir_all(root.join("k8s/overlays/prod")).unwrap();
    fs::create_dir_all(root.join("broken")).unwrap();

    write_dockerfile(root);
    write_base_manifests(root);
    write_prod_overlay(root);
    write_broken_manifest(root);
}

fn count(store: &GraphStore, q: &str) -> u64 {
    let qr = store
        .execute_query(q)
        .unwrap_or_else(|e| panic!("[{q}]: {e}"));
    qr.rows
        .first()
        .and_then(|r| r.first())
        .and_then(|c| c.parse::<u64>().ok())
        .unwrap_or(0)
}

fn ids(store: &GraphStore, q: &str) -> Vec<String> {
    let qr = store
        .execute_query(q)
        .unwrap_or_else(|e| panic!("[{q}]: {e}"));
    let mut v: Vec<String> = qr
        .rows
        .into_iter()
        .filter_map(|r| r.into_iter().next())
        .collect();
    v.sort();
    v
}

/// -- Nodes -------------------------------------------------------------
/// 6 IacResource: 1 Dockerfile + 3 base manifests (Deployment/Service/
/// ConfigMap) + 1 prod patch.yaml (also a valid Deployment manifest) + 1
/// templated Deployment (best-effort, still a resource).
fn assert_iac_nodes(store: &GraphStore) {
    assert_eq!(
        count(store, "MATCH (r:IacResource) RETURN count(r)"),
        6,
        "1 Dockerfile + 3 base manifests + patch.yaml + 1 templated"
    );
    assert_eq!(
        count(
            store,
            "MATCH (r:IacResource) WHERE r.resource_kind = 'Dockerfile' RETURN count(r)"
        ),
        1
    );
    assert_eq!(
        count(store, "MATCH (m:IacModule) RETURN count(m)"),
        2,
        "base + prod kustomizations"
    );
    // IacImage: node:18 (dockerfile) + web:latest (deployment) + the templated
    // Deployment declares no image → 2 images total.
    assert_eq!(count(store, "MATCH (i:IacImage) RETURN count(i)"), 2);
}

/// -- Structural edges + source span ------------------------------------
fn assert_iac_structural_edges(store: &GraphStore) {
    assert_eq!(
        count(
            store,
            "MATCH (:File)-[:Defines_File_IacResource]->(:IacResource) RETURN count(*)"
        ),
        6
    );
    assert_eq!(
        count(
            store,
            "MATCH (:File)-[:Defines_File_IacModule]->(:IacModule) RETURN count(*)"
        ),
        2
    );
    assert_eq!(
        count(
            store,
            "MATCH (:IacResource)-[:Imports_IacResource_IacImage]->(:IacImage) RETURN count(*)"
        ),
        2
    );

    // Source span is recorded (the `int` start_line column): the single-document
    // deployment starts at line 1. Asserting the value guards start_line emission.
    let start_line = ids(
        store,
        "MATCH (r:IacResource) WHERE r.path = 'k8s/base/deployment.yaml' RETURN r.start_line",
    );
    assert_eq!(start_line, vec!["1"], "deployment doc start_line must be 1");
}

/// -- Reference edges — resource/module → resolved File targets ---------
fn assert_iac_reference_edges(store: &GraphStore) {
    // Deployment (web:latest) → Dockerfile File via directory image-build match.
    let img_build = ids(
        store,
        "MATCH (r:IacResource)-[e:Imports_IacResource_File]->(f:File) \
         WHERE e.resolution_method = 'iac-image-build' RETURN f.id",
    );
    assert_eq!(img_build, vec!["services/web/Dockerfile"]);

    // Deployment configMapRef app-config → configmap.yaml File (name-match).
    let name_match = ids(
        store,
        "MATCH (r:IacResource)-[e:Imports_IacResource_File]->(f:File) \
         WHERE e.resolution_method = 'iac-name-match:ConfigMap/app-config' RETURN f.id",
    );
    assert_eq!(name_match, vec!["k8s/base/configmap.yaml"]);

    // Dockerfile COPY app.js → the app.js File.
    let copy_src = ids(
        store,
        "MATCH (r:IacResource)-[e:Imports_IacResource_File]->(f:File) \
         WHERE e.resolution_method = 'iac-copy-source' RETURN f.id",
    );
    assert_eq!(copy_src, vec!["services/web/app.js"]);

    // Kustomize base overlay → its 3 resource files.
    let base_refs = ids(
        store,
        "MATCH (m:IacModule)-[:Imports_IacModule_File]->(f:File) \
         WHERE m.id = 'k8s/base/kustomization.yaml::kustomize' RETURN f.id",
    );
    assert_eq!(
        base_refs,
        vec![
            "k8s/base/configmap.yaml",
            "k8s/base/deployment.yaml",
            "k8s/base/service.yaml"
        ]
    );

    // Prod overlay → base overlay (IMPORTS module→module) + the patch file.
    assert_eq!(
        count(
            store,
            "MATCH (a:IacModule)-[:Imports_IacModule_IacModule]->(b:IacModule) \
             WHERE a.id = 'k8s/overlays/prod/kustomization.yaml::kustomize' \
             AND b.id = 'k8s/base/kustomization.yaml::kustomize' RETURN count(*)"
        ),
        1,
        "prod overlay IMPORTS the base overlay"
    );
    let prod_files = ids(
        store,
        "MATCH (m:IacModule)-[:Imports_IacModule_File]->(f:File) \
         WHERE m.id = 'k8s/overlays/prod/kustomization.yaml::kustomize' RETURN f.id",
    );
    assert_eq!(prod_files, vec!["k8s/overlays/prod/patch.yaml"]);
}

#[test]
fn full_index_extracts_exact_iac_nodes_and_edges() {
    let tmp = tempfile::Builder::new()
        .prefix("iac_full_")
        .tempdir()
        .unwrap();
    let repo = tmp.path().join("repo");
    write_fixture(&repo);

    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    let graph = out.join("graph");
    indexer::index_codebase_with_language(&repo, &graph, &IndexOptions::default())
        .expect("full index");

    let store = GraphStore::open_or_create(&graph).unwrap();

    assert_iac_nodes(&store);
    assert_iac_structural_edges(&store);
    assert_iac_reference_edges(&store);
}

#[test]
fn broken_manifest_is_coverage_flagged_and_index_survives() {
    let tmp = tempfile::Builder::new()
        .prefix("iac_broken_")
        .tempdir()
        .unwrap();
    let repo = tmp.path().join("repo");
    write_fixture(&repo);

    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    let graph = out.join("graph");
    // The index must COMPLETE despite the templated manifest.
    let result = indexer::index_codebase_with_language(&repo, &graph, &IndexOptions::default())
        .expect("index survives a broken manifest");
    assert!(result.node_count > 0);

    // The templated file is recorded as a parse gap in the coverage sidecar.
    let cov = &result.coverage;
    let gap = cov
        .files
        .get("broken/templated.yaml")
        .expect("templated manifest must be a recorded coverage gap");
    assert_eq!(gap.kind, CoverageKind::ParsePartial);
    assert!(
        !gap.error_ranges.is_empty(),
        "the gap must carry the templated line range"
    );

    // And it is persisted (issue #57 mechanism) — a fresh reader sees it too.
    coverage::save(&coverage::coverage_path(&out), cov).unwrap();
    let loaded = coverage::load(&coverage::coverage_path(&out)).expect("coverage loads");
    assert!(loaded.files.contains_key("broken/templated.yaml"));

    // A clean manifest is NOT flagged.
    assert!(!cov.files.contains_key("k8s/base/deployment.yaml"));
}

/// Snapshot the deployment resource's identity — used before/after the
/// unrelated-manifest edit to prove it was not touched.
fn deployment_ids(store: &GraphStore) -> Vec<String> {
    ids(
        store,
        "MATCH (r:IacResource) WHERE r.resource_kind = 'Deployment' AND r.name = 'web' \
         RETURN r.id",
    )
}

/// Assert the post-edit invariants: the service resource re-derived, the
/// deployment resource untouched, total count stable, unrelated edge intact.
fn assert_only_service_changed(store: &GraphStore, deploy_before: &[String]) {
    assert_eq!(
        count(
            store,
            "MATCH (r:IacResource) WHERE r.resource_kind = 'Service' AND r.name = 'web' \
             RETURN count(r)"
        ),
        1
    );
    let deploy_after = deployment_ids(store);
    assert_eq!(deploy_before, deploy_after, "unrelated manifest untouched");
    assert_eq!(count(store, "MATCH (r:IacResource) RETURN count(r)"), 6);
    assert_eq!(
        count(
            store,
            "MATCH (:IacResource)-[e:Imports_IacResource_File]->(:File) \
             WHERE e.resolution_method = 'iac-name-match:ConfigMap/app-config' RETURN count(*)"
        ),
        1
    );
}

#[test]
fn incremental_reprocesses_only_the_changed_manifest() {
    let tmp = tempfile::Builder::new()
        .prefix("iac_incr_")
        .tempdir()
        .unwrap();
    let repo = tmp.path().join("repo");
    write_fixture(&repo);

    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    let graph = out.join("graph");
    let manifest_p = manifest::manifest_path(&out);
    indexer::index_codebase_with_language(&repo, &graph, &IndexOptions::default()).unwrap();
    indexer::write_full_manifest(&repo, &manifest_p, &IndexOptions::default()).unwrap();

    // Snapshot the deployment resource's identity BEFORE the edit — it must not
    // be touched when we edit an unrelated manifest.
    let deploy_before = {
        let store = GraphStore::open_or_create(&graph).unwrap();
        deployment_ids(&store)
    };

    // Edit ONLY service.yaml (add a port — changes size, so it re-classifies).
    fs::write(
        repo.join("k8s/base/service.yaml"),
        "apiVersion: v1\nkind: Service\nmetadata:\n  name: web\nspec:\n  selector:\n    app: web\n  ports:\n    - port: 80\n    - port: 443\n",
    )
    .unwrap();

    let prior = manifest::load(&manifest_p).unwrap();
    let inc =
        indexer::index_incremental(&repo, &graph, &manifest_p, &IndexOptions::default(), &prior)
            .expect("incremental");
    assert_eq!(inc.changed, 1, "only service.yaml changed");
    assert_eq!(inc.files_reparsed, 1);

    let store = GraphStore::open_or_create(&graph).unwrap();
    assert_only_service_changed(&store, &deploy_before);
}

/// Direction 1 — a change to a code artifact a manifest references reports
/// the manifest: get_impact on the Dockerfile File surfaces the Deployment
/// resource that builds from it (image-build edge).
fn assert_impact_dockerfile_to_deployment(store: &GraphStore) {
    let impact_df = get_impact(store, "services/web/Dockerfile").expect("impact");
    assert!(
        impact_df
            .importers
            .iter()
            .any(|n| n.id.starts_with("k8s/base/deployment.yaml::k8s")),
        "get_impact(Dockerfile) must report the Deployment manifest, got {:?}",
        impact_df
            .importers
            .iter()
            .map(|n| &n.id)
            .collect::<Vec<_>>()
    );
}

/// Direction 2 — a change to a source file the IaC references reports the
/// IaC: get_impact on app.js surfaces the Dockerfile resource that COPYs it.
fn assert_impact_app_js_to_dockerfile(store: &GraphStore) {
    let impact_src = get_impact(store, "services/web/app.js").expect("impact");
    assert!(
        impact_src
            .importers
            .iter()
            .any(|n| n.id == "services/web/Dockerfile::dockerfile"),
        "get_impact(app.js) must report the Dockerfile that COPYs it, got {:?}",
        impact_src
            .importers
            .iter()
            .map(|n| &n.id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn get_impact_traverses_iac_edges_both_directions() {
    let tmp = tempfile::Builder::new()
        .prefix("iac_impact_")
        .tempdir()
        .unwrap();
    let repo = tmp.path().join("repo");
    write_fixture(&repo);

    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    let graph = out.join("graph");
    indexer::index_codebase_with_language(&repo, &graph, &IndexOptions::default()).unwrap();
    let store = GraphStore::open_or_create(&graph).unwrap();

    assert_impact_dockerfile_to_deployment(&store);
    assert_impact_app_js_to_dockerfile(&store);

    // Forward direction (manifest → code): the Deployment resource's outbound
    // reference reaches the ConfigMap manifest it injects.
    let fwd = store
        .execute_query(
            "MATCH (r:IacResource)-[:Imports_IacResource_File]->(f:File) \
             WHERE r.resource_kind = 'Deployment' AND f.id = 'k8s/base/configmap.yaml' \
             RETURN f.id",
        )
        .unwrap();
    assert!(
        !fwd.rows.is_empty(),
        "Deployment must reference the ConfigMap manifest"
    );
}

/// configmap.yaml is reached ONLY through the 0.7 name-match edge → heuristic
/// dependents present → lower bound.
fn assert_configmap_impact_is_lower_bound(store: &GraphStore) {
    use ai_architect_mcp::epistemic::Boundary;
    let heuristic = get_impact(store, "k8s/base/configmap.yaml").expect("impact");
    assert!(
        !heuristic.importers.is_empty(),
        "the ConfigMap must have the Deployment as a heuristic importer"
    );
    assert_eq!(
        heuristic.epistemic,
        Boundary::LowerBound,
        "a 0.7 name-match dependent must report a lower bound, not exact"
    );
}

/// An image node is reached ONLY through the 1.0 declared edge → no
/// heuristic carriers → exact.
fn assert_image_impact_is_exact(store: &GraphStore) {
    use ai_architect_mcp::epistemic::Boundary;
    let image_id = {
        let q = store
            .execute_query(
                "MATCH (r:IacResource)-[:Imports_IacResource_IacImage]->(i:IacImage) \
                 WHERE r.resource_kind = 'Dockerfile' RETURN i.id LIMIT 1",
            )
            .unwrap();
        q.rows[0][0].clone()
    };
    let confident = get_impact(store, &image_id).expect("impact");
    assert!(
        !confident.importers.is_empty(),
        "the base image must have the Dockerfile as an importer"
    );
    assert_eq!(
        confident.epistemic,
        Boundary::Exact,
        "a dependent reached only through 1.0 edges must be exact"
    );
}

#[test]
fn iac_heuristic_edges_surface_as_lower_bound_confident_edges_as_exact() {
    // The honesty contract (issue #63 criterion 3 / #57): a manifest reference
    // resolved heuristically (confidence < 1.0) must make get_impact report a
    // LOWER BOUND, while a directly-declared (1.0) reference is EXACT. This
    // proves the new IaC edges plug into the epistemic-boundary machinery.
    let tmp = tempfile::Builder::new()
        .prefix("iac_epi_")
        .tempdir()
        .unwrap();
    let repo = tmp.path().join("repo");
    write_fixture(&repo);
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    let graph = out.join("graph");
    indexer::index_codebase_with_language(&repo, &graph, &IndexOptions::default()).unwrap();
    let store = GraphStore::open_or_create(&graph).unwrap();

    assert_configmap_impact_is_lower_bound(&store);
    assert_image_impact_is_exact(&store);
}

#[test]
fn query_graph_reaches_iac_labels() {
    let tmp = tempfile::Builder::new()
        .prefix("iac_query_")
        .tempdir()
        .unwrap();
    let repo = tmp.path().join("repo");
    write_fixture(&repo);

    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    let graph = out.join("graph");
    indexer::index_codebase_with_language(&repo, &graph, &IndexOptions::default()).unwrap();
    let store = GraphStore::open_or_create(&graph).unwrap();

    // Every new label is reachable by an ordinary read query (what query_graph
    // executes) — the acceptance that the deployment surface is now queryable.
    for label in ["IacResource", "IacModule", "IacImage"] {
        let qr = store
            .execute_query(&format!("MATCH (n:{label}) RETURN n.id LIMIT 1"))
            .unwrap_or_else(|e| panic!("query for {label} failed: {e}"));
        assert!(!qr.rows.is_empty(), "query_graph must reach {label} nodes");
    }
}
