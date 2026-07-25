// iac — infrastructure-as-code indexing pass (issue #63).
//
// Runs as a POST-PASS after the main index loop, exactly like `light_link`:
// every File node already exists, so a manifest can reference another file's
// node with no forward-reference problem. The pass ADDS typed nodes/edges to
// the deployment surface; it never touches the File nodes the walker created.
//
// Layer (coding-standards §2.2): a pipeline pass inside the indexer. It depends
// on graph_store (the port) and its own parsers (dockerfile/k8s/kustomize); the
// graph store depends on nothing here. No new dependency crosses outward
// (issue #63 criterion 7).
//
// Node model (all ids `<file-rel>::…`-prefixed so the incremental per-file
// symbol purge reclaims them on reparse — issue #62 integration; no new purge
// code):
//   IacResource `<rel>::dockerfile`         — a Dockerfile build target
//   IacResource `<rel>::k8s::<doc>`          — one per K8s manifest document
//   IacModule   `<rel>::kustomize`           — a Kustomize overlay
//   IacImage    `<rel>::image::<n>`          — a referenced container image
//
// Edge model:
//   Defines_File_IacResource / _IacModule    (1.0, structural — file contains it)
//   Imports_IacResource_IacImage             (1.0, image literally declared)
//   Imports_IacResource_File                 (heuristic: image→Dockerfile,
//                                             ConfigMap/Secret/Service name-match,
//                                             Dockerfile COPY source)
//   Imports_IacModule_File / _IacModule      (kustomize path references)
//
// Cross-file references target STABLE File nodes (which survive a modify), never
// another file's purgeable IacResource, so an unchanged referencer's edge is not
// collaterally dropped when a referenced file is reparsed. Heuristic edges carry
// confidence < 1.0 and a `resolution_method` provenance string; unresolved
// references simply produce no edge (a reported miss, not a fabricated one —
// issue #63 criterion 3).

mod dockerfile;
mod k8s;
mod kustomize;

use crate::graph_store::{cypher_str, GraphStore, PropEdgeList};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// Confidence + provenance for each edge kind. Heuristic values follow the
/// codebase convention (light_link.rs uses 0.9 for path-resolved links; the
/// epistemic module treats any confidence < 1.0 as a heuristic carrier). Direct
/// facts (the file literally contains the manifest / declares the image) are 1.0.
// source: light_link.rs (0.9 path resolution) + epistemic.rs (heuristic < 1.0).
const CONF_DIRECT: &str = "1.0";
const CONF_KUSTOMIZE_REF: &str = "0.95"; // path is declared; resolution can still miss
const CONF_COPY_SOURCE: &str = "0.9"; // COPY source path resolved to a File
const CONF_NAME_MATCH: &str = "0.7"; // configmap/secret/service by name+namespace
const CONF_IMAGE_BUILD: &str = "0.6"; // image name → Dockerfile by directory heuristic

/// The parse gaps a single IaC pass detected, handed to the coverage collector
/// by the caller (full index or incremental). Mirrors the ParseOutcome split.
#[derive(Debug, Default)]
pub(super) struct IacGaps {
    /// (rel, 1-based inclusive line ranges) — indexed but incomplete
    /// (templated/malformed manifest documents).
    pub partial: Vec<(String, Vec<(u32, u32)>)>,
    /// (rel, reason) — an IaC file that could not be read at all.
    pub skipped: Vec<(String, String)>,
}

/// Full-index entry point: process every IaC file in `files`, resolving cross
/// references against that same set. source: mirrors light_link::link_loose_file_imports.
pub(super) fn run_iac_pass(
    store: &GraphStore,
    root: &Path,
    files: &[PathBuf],
) -> Result<IacGaps, String> {
    run_iac_pass_for(store, root, files, files)
}

/// Incremental-friendly core: derive IaC nodes/edges OUT of `sources` (the
/// reparsed files), resolving references against `all_files` (the full current
/// tree) and against the IaC nodes already in the graph.
///
/// Preconditions: every File node for `all_files` exists in `store` (post-pass,
/// like light_link); each source file's stale `<rel>::` IaC nodes have already
/// been purged by the incremental machinery (or the graph is fresh). Postcondition
/// on `Ok(gaps)`: for every IaC file in `sources`, its IacResource/IacModule/
/// IacImage nodes and structural edges exist, and its resolvable reference edges
/// (to stable File nodes / images) have been drawn; `gaps` lists the documents
/// that parsed incompletely. Unresolved references produce no edge; read errors
/// become a `skipped` gap, never a hard error.
pub(super) fn run_iac_pass_for(
    store: &GraphStore,
    root: &Path,
    sources: &[PathBuf],
    all_files: &[PathBuf],
) -> Result<IacGaps, String> {
    let mut gaps = IacGaps::default();
    let mut parsed = ParsedIac::default();

    // ---- Phase 1: parse each source IaC file --------------------------------
    for path in sources {
        let rel = rel_id(root, path);
        match classify(path) {
            IacKind::Dockerfile => parse_dockerfile(path, &rel, &mut parsed, &mut gaps),
            IacKind::Kustomization => parse_kustomization(path, &rel, &mut parsed, &mut gaps),
            IacKind::MaybeManifest => parse_manifest(path, &rel, &mut parsed, &mut gaps),
            IacKind::Other => {}
        }
    }

    // ---- Phase 2: emit nodes + intra-file edges -----------------------------
    emit_nodes(store, &parsed)?;

    // ---- Phase 3: resolve + emit cross-file reference edges -----------------
    // Resolution indexes are built from the FULL current tree and the graph's
    // existing IaC nodes, so a changed manifest resolves against unchanged
    // targets (issue #63 criterion 5).
    let file_ids: HashSet<String> = all_files.iter().map(|f| rel_id(root, f)).collect();
    let indexes = ResolutionIndexes::build(store, &file_ids)?;
    emit_reference_edges(store, &parsed, &indexes)?;

    Ok(gaps)
}

// ---------------------------------------------------------------------------
// File classification
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum IacKind {
    Dockerfile,
    Kustomization,
    MaybeManifest,
    Other,
}

/// Classifies a file by name (and, for `.yaml`/`.yml`, by a cheap content
/// sniff deferred to the parse step). source: pass_infrascan.c cbm_is_dockerfile
/// + pass_k8s.c file dispatch.
fn classify(path: &Path) -> IacKind {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name == "dockerfile" || name.starts_with("dockerfile.") || name.ends_with(".dockerfile") {
        return IacKind::Dockerfile;
    }
    if name == "kustomization.yaml" || name == "kustomization.yml" || name == "kustomization" {
        return IacKind::Kustomization;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "yaml" || ext == "yml" {
        return IacKind::MaybeManifest;
    }
    IacKind::Other
}

// ---------------------------------------------------------------------------
// Parsing phase — accumulate parsed structs keyed by file rel
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ParsedIac {
    dockerfiles: Vec<(String, dockerfile::DockerfileInfo)>,
    manifests: Vec<(String, Vec<k8s::K8sDoc>)>,
    kustomizations: Vec<(String, kustomize::Kustomization)>,
}

fn parse_dockerfile(path: &Path, rel: &str, parsed: &mut ParsedIac, gaps: &mut IacGaps) {
    let src = match read_text(path) {
        Some(s) => s,
        None => {
            gaps.skipped
                .push((rel.to_string(), "unreadable/oversized".into()));
            return;
        }
    };
    match dockerfile::parse(&src) {
        Some(info) => parsed.dockerfiles.push((rel.to_string(), info)),
        // A Dockerfile with no FROM is a parse gap: it is IaC we could not
        // interpret (issue #63 criterion 4), not a silent skip.
        None => gaps.partial.push((
            rel.to_string(),
            vec![(1, src.lines().count().max(1) as u32)],
        )),
    }
}

fn parse_kustomization(path: &Path, rel: &str, parsed: &mut ParsedIac, gaps: &mut IacGaps) {
    let src = match read_text(path) {
        Some(s) => s,
        None => {
            gaps.skipped
                .push((rel.to_string(), "unreadable/oversized".into()));
            return;
        }
    };
    parsed
        .kustomizations
        .push((rel.to_string(), kustomize::parse(&src)));
}

fn parse_manifest(path: &Path, rel: &str, parsed: &mut ParsedIac, gaps: &mut IacGaps) {
    let src = match read_text(path) {
        Some(s) => s,
        None => return, // unreadable .yaml that is not known IaC — ignore silently
    };
    // Only treat a .yaml/.yml as IaC if it declares apiVersion + kind. Arbitrary
    // config YAML is not IaC and must not pollute the coverage gap signal.
    if !k8s::looks_like_manifest(&src) {
        return;
    }
    let scanned = k8s::scan(&src);
    if !scanned.gap_ranges.is_empty() {
        gaps.partial.push((rel.to_string(), scanned.gap_ranges));
    }
    if !scanned.docs.is_empty() {
        parsed.manifests.push((rel.to_string(), scanned.docs));
    }
}

/// Reads a file as UTF-8 within the parse cap; None on binary / oversize / error.
fn read_text(path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    if s.len() as u64 > super::MAX_PARSE_BYTES {
        return None;
    }
    Some(s)
}

// ---------------------------------------------------------------------------
// Node emission
// ---------------------------------------------------------------------------

/// A staged node: (label, id, row of (col, formatted-value)). String values are
/// cypher_str-quoted; ints are bare — matching bulk_insert_nodes' expectations.
type NodeRow = Vec<(String, String)>;

fn emit_nodes(store: &GraphStore, parsed: &ParsedIac) -> Result<(), String> {
    let mut nodes: HashMap<&'static str, Vec<NodeRow>> = HashMap::new();
    let mut edges: HashMap<&'static str, PropEdgeList> = HashMap::new();

    for (rel, info) in &parsed.dockerfiles {
        emit_dockerfile_nodes(rel, info, &mut nodes, &mut edges);
    }
    for (rel, docs) in &parsed.manifests {
        emit_manifest_nodes(rel, docs, &mut nodes, &mut edges);
    }
    for (rel, _) in &parsed.kustomizations {
        emit_module_node(rel, &mut nodes, &mut edges);
    }

    // Nodes first, then edges (edges reference the node ids just inserted, plus
    // the pre-existing File nodes).
    for (label, rows) in &nodes {
        store.bulk_insert_nodes(label, rows)?;
    }
    for (table, es) in &edges {
        store.bulk_insert_edges(table, es)?;
    }
    Ok(())
}

fn emit_dockerfile_nodes(
    rel: &str,
    info: &dockerfile::DockerfileInfo,
    nodes: &mut HashMap<&'static str, Vec<NodeRow>>,
    edges: &mut HashMap<&'static str, PropEdgeList>,
) {
    let res_id = format!("{rel}::dockerfile");
    let name = basename(rel);
    let ports = info.exposed_ports.join(",");
    nodes.entry("IacResource").or_default().push(vec![
        col("id", &res_id),
        col("name", &name),
        col("qualified_name", &res_id),
        col("resource_kind", "Dockerfile"),
        col("image", &info.base_image),
        col("ports", &ports),
        col("entrypoint", &info.entrypoint),
        col("workdir", &info.workdir),
        col("source", "dockerfile"),
        col("path", rel),
        int("start_line", 1),
    ]);
    edges
        .entry("Defines_File_IacResource")
        .or_default()
        .push(structural_edge(rel, &res_id));

    for (i, image) in info.stage_images.iter().enumerate() {
        let img_id = format!("{rel}::image::{i}");
        push_image_node(nodes, &img_id, image);
        edges
            .entry("Imports_IacResource_IacImage")
            .or_default()
            .push(ref_edge(&res_id, &img_id, CONF_DIRECT, "iac-image-decl"));
    }
}

fn emit_manifest_nodes(
    rel: &str,
    docs: &[k8s::K8sDoc],
    nodes: &mut HashMap<&'static str, Vec<NodeRow>>,
    edges: &mut HashMap<&'static str, PropEdgeList>,
) {
    for (doc_idx, doc) in docs.iter().enumerate() {
        let res_id = format!("{rel}::k8s::{doc_idx}");
        let primary_image = doc.images.first().cloned().unwrap_or_default();
        nodes.entry("IacResource").or_default().push(vec![
            col("id", &res_id),
            col("name", &doc.name),
            col("qualified_name", &res_id),
            col("resource_kind", &doc.kind),
            col("api_version", &doc.api_version),
            col("namespace", &doc.namespace),
            col("image", &primary_image),
            col("source", "k8s"),
            col("path", rel),
            int("start_line", doc.start_line as i64),
        ]);
        edges
            .entry("Defines_File_IacResource")
            .or_default()
            .push(structural_edge(rel, &res_id));

        for (i, image) in doc.images.iter().enumerate() {
            let img_id = format!("{rel}::image::k8s::{doc_idx}::{i}");
            push_image_node(nodes, &img_id, image);
            edges
                .entry("Imports_IacResource_IacImage")
                .or_default()
                .push(ref_edge(&res_id, &img_id, CONF_DIRECT, "iac-image-decl"));
        }
    }
}

fn emit_module_node(
    rel: &str,
    nodes: &mut HashMap<&'static str, Vec<NodeRow>>,
    edges: &mut HashMap<&'static str, PropEdgeList>,
) {
    let mod_id = format!("{rel}::kustomize");
    nodes.entry("IacModule").or_default().push(vec![
        col("id", &mod_id),
        col("name", &basename(rel)),
        col("qualified_name", &mod_id),
        col("resource_kind", "Kustomization"),
        col("source", "kustomize"),
        col("path", rel),
        int("start_line", 1),
    ]);
    edges
        .entry("Defines_File_IacModule")
        .or_default()
        .push(structural_edge(rel, &mod_id));
}

fn push_image_node(nodes: &mut HashMap<&'static str, Vec<NodeRow>>, id: &str, reference: &str) {
    let (registry, name, tag) = split_image_ref(reference);
    nodes.entry("IacImage").or_default().push(vec![
        col("id", id),
        col("reference", reference),
        col("name", &name),
        col("tag", &tag),
        col("registry", &registry),
    ]);
}

// ---------------------------------------------------------------------------
// Reference-edge resolution + emission
// ---------------------------------------------------------------------------

/// Indexes used to resolve heuristic cross-file references, built once per pass.
struct ResolutionIndexes {
    /// Every indexed File rel — for path resolution.
    file_ids: HashSet<String>,
    /// (kind, name, namespace) → File rel that declares that k8s resource.
    /// namespace "" is normalized to "default" on both sides.
    resource_by_ident: HashMap<(String, String, String), String>,
    /// directory-basename → Dockerfile File rel (the image-build heuristic).
    dockerfile_by_dir: HashMap<String, String>,
    /// File rels that carry an IacModule (kustomize overlays), for base links.
    module_files: HashSet<String>,
}

impl ResolutionIndexes {
    /// Builds the indexes by querying the graph's current IaC nodes plus the
    /// supplied File-id set. Querying the graph (not re-scanning files) lets a
    /// changed manifest resolve against unchanged targets already indexed.
    fn build(store: &GraphStore, file_ids: &HashSet<String>) -> Result<Self, String> {
        let mut resource_by_ident = HashMap::new();
        let mut dockerfile_by_dir = HashMap::new();
        let qr = store.execute_query(
            "MATCH (r:IacResource) \
             RETURN r.resource_kind, r.name, r.namespace, r.path",
        )?;
        for row in &qr.rows {
            if row.len() < 4 {
                continue;
            }
            let (kind, name, ns, path) = (&row[0], &row[1], &row[2], &row[3]);
            if kind == "Dockerfile" {
                if let Some(dir) = parent_dir(path) {
                    dockerfile_by_dir.insert(basename(&dir), path.clone());
                }
            } else if !name.is_empty() {
                resource_by_ident.insert((kind.clone(), name.clone(), norm_ns(ns)), path.clone());
            }
        }
        let mut module_files = HashSet::new();
        let mq = store.execute_query("MATCH (m:IacModule) RETURN m.path")?;
        for row in &mq.rows {
            if let Some(p) = row.first() {
                module_files.insert(p.clone());
            }
        }
        Ok(ResolutionIndexes {
            file_ids: file_ids.clone(),
            resource_by_ident,
            dockerfile_by_dir,
            module_files,
        })
    }
}

fn emit_reference_edges(
    store: &GraphStore,
    parsed: &ParsedIac,
    idx: &ResolutionIndexes,
) -> Result<(), String> {
    let mut edges: HashMap<&'static str, PropEdgeList> = HashMap::new();
    let mut seen: HashSet<(String, String, &'static str)> = HashSet::new();

    // Dockerfile COPY sources → the File they copy (if indexed).
    for (rel, info) in &parsed.dockerfiles {
        let res_id = format!("{rel}::dockerfile");
        for src in &info.copy_sources {
            if let Some(target) = resolve_path(rel, src, &idx.file_ids) {
                stage(
                    &mut edges,
                    &mut seen,
                    "Imports_IacResource_File",
                    &res_id,
                    &target,
                    CONF_COPY_SOURCE,
                    "iac-copy-source",
                );
            }
        }
    }

    // K8s documents → Dockerfile (image-build heuristic) + ConfigMap/Secret/
    // Service name matches.
    for (rel, docs) in &parsed.manifests {
        for (doc_idx, doc) in docs.iter().enumerate() {
            let res_id = format!("{rel}::k8s::{doc_idx}");
            for image in &doc.images {
                if let Some(df) = idx.dockerfile_by_dir.get(&image_name(image)) {
                    stage(
                        &mut edges,
                        &mut seen,
                        "Imports_IacResource_File",
                        &res_id,
                        df,
                        CONF_IMAGE_BUILD,
                        "iac-image-build",
                    );
                }
            }
            for cref in &doc.config_refs {
                let key = (
                    cref.kind.clone(),
                    cref.name.clone(),
                    norm_ns(&doc.namespace),
                );
                if let Some(target) = idx.resource_by_ident.get(&key) {
                    let method = format!("iac-name-match:{}/{}", cref.kind, cref.name);
                    stage_owned(
                        &mut edges,
                        &mut seen,
                        "Imports_IacResource_File",
                        &res_id,
                        target,
                        CONF_NAME_MATCH,
                        method,
                    );
                }
            }
        }
    }

    // Kustomize overlays → referenced files / base overlays.
    for (rel, kust) in &parsed.kustomizations {
        let mod_id = format!("{rel}::kustomize");
        for spec in &kust.refs {
            let Some(target) = resolve_path(rel, spec, &idx.file_ids) else {
                continue;
            };
            // A resolved base directory points at its kustomization.yaml (an
            // IacModule) → overlay→base IMPORTS edge; a plain file → File ref.
            if idx.module_files.contains(&target) && target != *rel {
                let base_mod = format!("{target}::kustomize");
                stage(
                    &mut edges,
                    &mut seen,
                    "Imports_IacModule_IacModule",
                    &mod_id,
                    &base_mod,
                    CONF_KUSTOMIZE_REF,
                    "kustomize-base",
                );
            } else {
                stage(
                    &mut edges,
                    &mut seen,
                    "Imports_IacModule_File",
                    &mod_id,
                    &target,
                    CONF_KUSTOMIZE_REF,
                    "kustomize-ref",
                );
            }
        }
    }

    for (table, es) in &edges {
        store.bulk_insert_edges(table, es)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Edge/row helpers
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn stage(
    edges: &mut HashMap<&'static str, PropEdgeList>,
    seen: &mut HashSet<(String, String, &'static str)>,
    table: &'static str,
    from: &str,
    to: &str,
    confidence: &str,
    method: &str,
) {
    stage_owned(edges, seen, table, from, to, confidence, method.to_string());
}

#[allow(clippy::too_many_arguments)]
fn stage_owned(
    edges: &mut HashMap<&'static str, PropEdgeList>,
    seen: &mut HashSet<(String, String, &'static str)>,
    table: &'static str,
    from: &str,
    to: &str,
    confidence: &str,
    method: String,
) {
    if !seen.insert((from.to_string(), to.to_string(), table)) {
        return;
    }
    edges
        .entry(table)
        .or_default()
        .push(ref_edge(from, to, confidence, &method));
}

/// A structural (1.0, "iac-direct") File→node edge — the file literally
/// contains this manifest. from/to are RAW ids (bulk_insert_edges quotes them).
fn structural_edge(from: &str, to: &str) -> (String, String, Vec<(String, String)>) {
    ref_edge(from, to, CONF_DIRECT, "iac-direct")
}

fn ref_edge(
    from: &str,
    to: &str,
    confidence: &str,
    method: &str,
) -> (String, String, Vec<(String, String)>) {
    (
        from.to_string(),
        to.to_string(),
        vec![
            ("confidence".to_string(), confidence.to_string()),
            ("resolution_method".to_string(), cypher_str(method)),
        ],
    )
}

/// A cypher_str-quoted string column cell.
fn col(name: &str, value: &str) -> (String, String) {
    (name.to_string(), cypher_str(value))
}

/// A bare-integer column cell.
fn int(name: &str, value: i64) -> (String, String) {
    (name.to_string(), value.to_string())
}

// ---------------------------------------------------------------------------
// Path + image helpers
// ---------------------------------------------------------------------------

/// Repo-relative, forward-slash File id for a path under `root`.
fn rel_id(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

fn basename(rel: &str) -> String {
    rel.rsplit('/').next().unwrap_or(rel).to_string()
}

fn parent_dir(rel: &str) -> Option<String> {
    rel.rsplit_once('/').map(|(dir, _)| dir.to_string())
}

/// Normalizes a k8s namespace: an empty namespace is the implicit `default`.
fn norm_ns(ns: &str) -> String {
    if ns.is_empty() {
        "default".to_string()
    } else {
        ns.to_string()
    }
}

/// Resolves a relative reference `spec` (from the directory of `from_rel`) to an
/// indexed File id. Tries the file directly, then as an overlay directory
/// (`<spec>/kustomization.yaml|yml`). Returns the matched File id or None.
fn resolve_path(from_rel: &str, spec: &str, file_ids: &HashSet<String>) -> Option<String> {
    let from_dir = Path::new(from_rel)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let joined = normalize(&from_dir.join(spec));
    let base = joined.to_string_lossy().replace('\\', "/");
    if base.is_empty() {
        return None;
    }
    if file_ids.contains(&base) {
        return Some(base);
    }
    for suffix in [
        "/kustomization.yaml",
        "/kustomization.yml",
        "/kustomization",
    ] {
        let candidate = format!("{base}{suffix}");
        if file_ids.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Lexically normalizes a path (resolves `.`/`..`) without touching the disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s.to_os_string()),
            _ => {}
        }
    }
    let mut pb = PathBuf::new();
    for s in out {
        pb.push(s);
    }
    pb
}

/// Splits a container image reference into (registry, name, tag).
/// `registry.io/team/app:1.2` → ("registry.io/team", "app", "1.2");
/// `nginx` → ("", "nginx", ""). A `@sha256:` digest is treated as the tag.
fn split_image_ref(reference: &str) -> (String, String, String) {
    let (path, tag) = if let Some((p, t)) = reference.split_once('@') {
        (p, t.to_string())
    } else if let Some(idx) = reference.rfind(':') {
        // A ':' before a '/' is a registry port, not a tag — guard against it.
        if reference[idx..].contains('/') {
            (reference, String::new())
        } else {
            (&reference[..idx], reference[idx + 1..].to_string())
        }
    } else {
        (reference, String::new())
    };
    match path.rsplit_once('/') {
        Some((registry, name)) => (registry.to_string(), name.to_string(), tag),
        None => (String::new(), path.to_string(), tag),
    }
}

/// The bare image name (repo component, no registry/tag) used by the
/// image→Dockerfile directory heuristic.
fn image_name(reference: &str) -> String {
    split_image_ref(reference).1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_image_refs() {
        assert_eq!(
            split_image_ref("registry.io/team/app:1.2"),
            ("registry.io/team".into(), "app".into(), "1.2".into())
        );
        assert_eq!(
            split_image_ref("nginx"),
            (String::new(), "nginx".into(), String::new())
        );
        assert_eq!(
            split_image_ref("localhost:5000/web"),
            ("localhost:5000".into(), "web".into(), String::new())
        );
        assert_eq!(image_name("registry.io/team/web:latest"), "web");
    }

    #[test]
    fn resolves_relative_and_base_paths() {
        let ids: HashSet<String> = [
            "k8s/overlays/prod/kustomization.yaml",
            "k8s/base/kustomization.yaml",
            "k8s/base/deployment.yaml",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // File reference from the overlay to a sibling file.
        assert_eq!(
            resolve_path(
                "k8s/overlays/prod/kustomization.yaml",
                "../../base/deployment.yaml",
                &ids
            ),
            Some("k8s/base/deployment.yaml".to_string())
        );
        // Base DIRECTORY reference resolves to its kustomization.yaml.
        assert_eq!(
            resolve_path("k8s/overlays/prod/kustomization.yaml", "../../base", &ids),
            Some("k8s/base/kustomization.yaml".to_string())
        );
        assert_eq!(
            resolve_path(
                "k8s/overlays/prod/kustomization.yaml",
                "./missing.yaml",
                &ids
            ),
            None
        );
    }

    #[test]
    fn namespace_defaulting() {
        assert_eq!(norm_ns(""), "default");
        assert_eq!(norm_ns("prod"), "prod");
    }

    #[test]
    fn classify_recognizes_every_naming_variant() {
        use std::path::PathBuf;
        let c = |p: &str| classify(&PathBuf::from(p));
        // Dockerfile: bare, `Dockerfile.<tag>` prefix, and `<name>.dockerfile`
        // suffix — each of the three alternatives, case-insensitive.
        assert_eq!(c("services/Dockerfile"), IacKind::Dockerfile);
        assert_eq!(c("services/Dockerfile.prod"), IacKind::Dockerfile);
        assert_eq!(c("services/web.dockerfile"), IacKind::Dockerfile);
        // Kustomization: .yaml, .yml, and extension-less.
        assert_eq!(c("base/kustomization.yaml"), IacKind::Kustomization);
        assert_eq!(c("base/kustomization.yml"), IacKind::Kustomization);
        assert_eq!(c("base/Kustomization"), IacKind::Kustomization);
        // Generic manifest candidates: BOTH .yaml and .yml.
        assert_eq!(c("k8s/deploy.yaml"), IacKind::MaybeManifest);
        assert_eq!(c("k8s/deploy.yml"), IacKind::MaybeManifest);
        // Non-IaC files are ignored.
        assert_eq!(c("src/main.rs"), IacKind::Other);
        assert_eq!(c("README.md"), IacKind::Other);
    }

    #[test]
    fn read_text_enforces_the_parse_byte_cap_at_the_boundary() {
        use std::io::Write;
        let dir = tempfile::Builder::new()
            .prefix("iac_read_cap_")
            .tempdir()
            .unwrap();
        let cap = super::super::MAX_PARSE_BYTES as usize;

        // Exactly at the cap → accepted (the boundary is inclusive).
        let at = dir.path().join("at.yaml");
        std::fs::File::create(&at)
            .unwrap()
            .write_all(&vec![b'a'; cap])
            .unwrap();
        assert!(
            read_text(&at).is_some(),
            "a file of exactly MAX_PARSE_BYTES must be read"
        );

        // One byte over the cap → rejected (None).
        let over = dir.path().join("over.yaml");
        std::fs::File::create(&over)
            .unwrap()
            .write_all(&vec![b'a'; cap + 1])
            .unwrap();
        assert!(
            read_text(&over).is_none(),
            "a file one byte over MAX_PARSE_BYTES must be skipped"
        );
    }
}
