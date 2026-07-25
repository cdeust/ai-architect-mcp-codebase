// Kubernetes manifest scanning — a dependency-free, indent-aware scan of the
// fields issue #63 criteria 2 names: apiVersion, kind, metadata.name/namespace,
// container images, and the ConfigMap/Secret/Service references a workload
// injects.
//
// Mirrors DeusData/codebase-memory-mcp pass_k8s.c: an indent stack tracks the
// mapping path so `metadata.name` is distinguished from a `name:` deeper in the
// pod spec. Multi-document files (`---`-separated) yield one document each —
// criterion 4 requires them handled, not silently truncated to the first doc as
// the C reference does.
//
// Why a hand scan and not serde_yaml: k8s manifests are a constrained YAML
// subset, and a hand scan gives PRECISE control over the parse-gap signal
// (issue #63 criterion 4) — a Helm-templated (`{{ }}`) or malformed document is
// REPORTED as a gap, not silently mis-parsed. serde_yaml is archived/unmaintained
// and serde_yml pulls a heavyweight transitive tree for a subset we scan
// directly (design constraint 2: no new heavyweight dependency without
// justification, and the justification bar is not met here).

/// Upper bounds per document (issue #63 criterion 6: bounded extraction).
const MAX_IMAGES: usize = 32;
const MAX_REFS: usize = 64;

/// A referenced ConfigMap/Secret/Service, by kind and name. Namespace is the
/// owning document's namespace (k8s name references are namespace-scoped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRef {
    /// "ConfigMap" | "Secret" | "Service".
    pub kind: String,
    pub name: String,
}

/// One Kubernetes manifest document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct K8sDoc {
    pub kind: String,
    pub api_version: String,
    pub name: String,
    /// Empty when the manifest declares none — the caller treats "" as the
    /// implicit `default` namespace for reference matching.
    pub namespace: String,
    /// 1-based line where this document starts (for the IacResource node span).
    pub start_line: u32,
    /// Container `image:` references found in the pod spec.
    pub images: Vec<String>,
    /// ConfigMap/Secret/Service references injected by this workload.
    pub config_refs: Vec<ConfigRef>,
    /// The document contained Go-template markers (`{{ … }}`) — Helm/templated.
    /// The caller records a parse gap: templated regions are not resolvable.
    pub templated: bool,
}

/// The result of scanning a YAML file for k8s manifests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanResult {
    pub docs: Vec<K8sDoc>,
    /// 1-based (start,end) line ranges the scan could not interpret as a
    /// well-formed manifest — a templated document, or a `kind`-bearing document
    /// with no resolvable `metadata.name`. Fed verbatim to the coverage sidecar
    /// (issue #57 / #63 criterion 4). Absence of a range is NOT a correctness
    /// guarantee, only "no gap detected".
    pub gap_ranges: Vec<(u32, u32)>,
}

/// Returns true if `content` looks like a Kubernetes manifest: it declares both
/// a top-level `apiVersion:` and `kind:`. Used to decide whether a `.yaml`/
/// `.yml` file is IaC the pass should attempt (and therefore whose parse gaps
/// are worth reporting) versus arbitrary YAML the pass ignores entirely.
/// source: pass_infrascan.c cbm_is_k8s_manifest.
pub fn looks_like_manifest(content: &str) -> bool {
    let mut has_api = false;
    let mut has_kind = false;
    for raw in content.lines() {
        let indent = raw.len() - raw.trim_start_matches(' ').len();
        if indent != 0 {
            continue;
        }
        let line = raw.trim();
        if line.starts_with("apiVersion:") {
            has_api = true;
        } else if line.starts_with("kind:") {
            has_kind = true;
        }
        if has_api && has_kind {
            return true;
        }
    }
    false
}

/// Scans a multi-document YAML file into its k8s manifest documents plus any
/// parse-gap line ranges.
///
/// Precondition: `content` is UTF-8 YAML that `looks_like_manifest` accepted.
/// Postcondition: one `K8sDoc` per `---`-separated document that yields a
/// `kind`; each templated or nameless `kind`-bearing document contributes a
/// `gap_range` AND (for templated docs that still expose a kind) a best-effort
/// `K8sDoc`, so the file is never silently dropped (issue #63 criterion 4).
pub fn scan(content: &str) -> ScanResult {
    let mut result = ScanResult::default();
    for (start_line, chunk) in split_documents(content) {
        if chunk.trim().is_empty() {
            continue;
        }
        let templated = chunk.contains("{{");
        let doc = scan_document(&chunk, start_line, templated);
        let end_line = start_line + chunk.lines().count().saturating_sub(1) as u32;
        match doc {
            Some(d) => {
                // A document with a kind but no name, or a templated document,
                // is a gap: constructs inside may be missing or unresolved.
                if templated || d.name.is_empty() {
                    result.gap_ranges.push((start_line, end_line));
                }
                result.docs.push(d);
            }
            None => {
                // A non-empty chunk that yielded no `kind` inside a file that
                // looked like a manifest is a genuine parse gap.
                if !templated || chunk.contains("kind:") {
                    result.gap_ranges.push((start_line, end_line));
                }
            }
        }
    }
    result
}

/// Splits `content` on YAML document separators (a line that is exactly `---`,
/// optionally trailing-spaced). Returns `(start_line_1based, chunk_text)` for
/// each document. A leading `---` produces no empty leading document.
fn split_documents(content: &str) -> Vec<(u32, String)> {
    let mut docs = Vec::new();
    let mut current = String::new();
    let mut current_start: u32 = 1;
    let mut line_no: u32 = 0;
    for raw in content.lines() {
        line_no += 1;
        if raw.trim_end() == "---" {
            if !current.is_empty() {
                docs.push((current_start, std::mem::take(&mut current)));
            }
            current_start = line_no + 1;
            continue;
        }
        current.push_str(raw);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        docs.push((current_start, current));
    }
    docs
}

/// Scans one document chunk. Returns `None` if no top-level `kind:` is present.
fn scan_document(chunk: &str, start_line: u32, templated: bool) -> Option<K8sDoc> {
    let mut kind = String::new();
    let mut api_version = String::new();
    let mut name = String::new();
    let mut namespace = String::new();
    let mut images: Vec<String> = Vec::new();
    let mut config_refs: Vec<ConfigRef> = Vec::new();

    // Indent of the top-level `metadata:` block, so `name`/`namespace` are read
    // ONLY from metadata (not from a `name:` deep in the pod template).
    let mut metadata_indent: Option<usize> = None;
    // A pending reference marker (e.g. `configMapRef:`) awaiting its `name:`.
    let mut ref_marker: Option<(usize, String)> = None;

    for raw in chunk.lines() {
        let indent = indent_of(raw);
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let entry = line.strip_prefix("- ").map(str::trim).unwrap_or(line);
        let (key, value) = match split_key(entry) {
            Some(kv) => kv,
            None => continue,
        };

        // Close a pending ref marker once we dedent out of its block.
        if let Some((mi, _)) = &ref_marker {
            if indent <= *mi && !key.eq_ignore_ascii_case("name") {
                ref_marker = None;
            }
        }
        // Close the metadata block on dedent to top level.
        if let Some(mi) = metadata_indent {
            if indent <= mi && key != "metadata" {
                metadata_indent = None;
            }
        }

        match key {
            "apiVersion" if indent == 0 => api_version = unquote(value),
            "kind" if indent == 0 && kind.is_empty() => kind = unquote(value),
            "metadata" if indent == 0 => metadata_indent = Some(indent),
            "name" => {
                // metadata.name (deeper than metadata:) OR the name of a pending
                // reference marker.
                if let Some((_, ref_kind)) = ref_marker.take() {
                    push_ref(&mut config_refs, ref_kind, unquote(value));
                } else if let Some(mi) = metadata_indent {
                    if indent > mi && name.is_empty() {
                        name = unquote(value);
                    }
                }
            }
            "namespace" => {
                if let Some(mi) = metadata_indent {
                    if indent > mi && namespace.is_empty() {
                        namespace = unquote(value);
                    }
                }
            }
            "image" => push_image(&mut images, unquote(value)),
            "secretName" => push_ref(&mut config_refs, "Secret".into(), unquote(value)),
            "serviceName" => push_ref(&mut config_refs, "Service".into(), unquote(value)),
            k => {
                if let Some(rk) = ref_kind_of(k) {
                    // Marker line; its `name:` follows at a deeper indent.
                    ref_marker = Some((indent, rk.to_string()));
                }
            }
        }
    }

    if kind.is_empty() {
        return None;
    }
    Some(K8sDoc {
        kind,
        api_version,
        name,
        namespace,
        start_line,
        images,
        config_refs,
        templated,
    })
}

/// Maps a reference-marker key to the resource kind it points at, or `None`.
/// source: k8s API — envFrom/valueFrom reference shapes + volume sources.
fn ref_kind_of(key: &str) -> Option<&'static str> {
    match key {
        "configMapRef" | "configMapKeyRef" | "configMap" => Some("ConfigMap"),
        "secretRef" | "secretKeyRef" | "secret" => Some("Secret"),
        "service" => Some("Service"),
        _ => None,
    }
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// Splits `key: value` (or trailing-colon `key:`) into (key, value). Returns
/// `None` for non-mapping lines. Values keep internal colons (split on first
/// `: ` only).
fn split_key(trimmed: &str) -> Option<(&str, &str)> {
    if let Some(rest) = trimmed.strip_suffix(':') {
        return Some((rest.trim(), ""));
    }
    let idx = trimmed.find(": ")?;
    Some((trimmed[..idx].trim(), trimmed[idx + 2..].trim()))
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
    {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

fn push_image(images: &mut Vec<String>, img: String) {
    if !img.is_empty() && images.len() < MAX_IMAGES && !images.contains(&img) {
        images.push(img);
    }
}

fn push_ref(refs: &mut Vec<ConfigRef>, kind: String, name: String) {
    if name.is_empty() || refs.len() >= MAX_REFS {
        return;
    }
    let r = ConfigRef { kind, name };
    if !refs.contains(&r) {
        refs.push(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_deployment_fields() {
        let m = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n  namespace: prod\nspec:\n  template:\n    spec:\n      containers:\n        - name: web\n          image: web:1.2.3\n          envFrom:\n            - configMapRef:\n                name: app-config\n            - secretRef:\n                name: app-secret\n";
        let r = scan(m);
        assert_eq!(r.docs.len(), 1);
        let d = &r.docs[0];
        assert_eq!(d.kind, "Deployment");
        assert_eq!(d.api_version, "apps/v1");
        assert_eq!(d.name, "web");
        assert_eq!(d.namespace, "prod");
        assert_eq!(d.images, vec!["web:1.2.3"]);
        assert_eq!(
            d.config_refs,
            vec![
                ConfigRef {
                    kind: "ConfigMap".into(),
                    name: "app-config".into()
                },
                ConfigRef {
                    kind: "Secret".into(),
                    name: "app-secret".into()
                },
            ]
        );
        assert!(r.gap_ranges.is_empty());
    }

    #[test]
    fn pod_spec_name_does_not_shadow_metadata_name() {
        // The container `- name: web` must NOT be read as the manifest name.
        let m = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: real-name\nspec:\n  containers:\n    - name: sidecar\n      image: busybox\n";
        let d = &scan(m).docs[0];
        assert_eq!(d.name, "real-name");
    }

    #[test]
    fn multi_document_yields_each() {
        let m = "apiVersion: v1\nkind: Service\nmetadata:\n  name: svc\n---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg\n";
        let r = scan(m);
        assert_eq!(r.docs.len(), 2);
        assert_eq!(r.docs[0].kind, "Service");
        assert_eq!(r.docs[1].kind, "ConfigMap");
        // Doc 1 is lines 1-4, the `---` separator is line 5, so doc 2 starts at 6.
        assert_eq!(r.docs[1].start_line, 6);
    }

    #[test]
    fn templated_document_is_a_gap_but_still_scanned() {
        let m = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: {{ .Values.name }}\nspec:\n  replicas: {{ .Values.replicas }}\n";
        let r = scan(m);
        assert_eq!(r.docs.len(), 1);
        assert!(r.docs[0].templated);
        assert_eq!(r.gap_ranges, vec![(1, 6)]);
    }

    #[test]
    fn kindless_yaml_is_reported_not_dropped() {
        let m = "foo: bar\nbaz:\n  - 1\n  - 2\n";
        let r = scan(m);
        assert!(r.docs.is_empty());
        assert_eq!(r.gap_ranges, vec![(1, 4)]);
    }

    #[test]
    fn detects_manifest_shape() {
        assert!(looks_like_manifest("apiVersion: v1\nkind: Pod\n"));
        assert!(!looks_like_manifest("name: x\nversion: 1\n"));
    }

    #[test]
    fn manifest_shape_requires_both_apiversion_and_kind() {
        // BOTH markers are required — a document with only one is not treated as
        // IaC (guards the `has_api && has_kind`).
        assert!(
            !looks_like_manifest("kind: Pod\n"),
            "kind alone is not enough"
        );
        assert!(
            !looks_like_manifest("apiVersion: v1\n"),
            "apiVersion alone is not enough"
        );
    }

    #[test]
    fn top_level_fields_are_read_only_at_indent_zero() {
        // apiVersion/kind/metadata are top-level keys; a nested key of the same
        // name inside the pod spec must NOT overwrite them (guards the three
        // `indent == 0` match guards).
        let m = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n  namespace: prod\nspec:\n  template:\n    apiVersion: nested/v9\n    kind: Nested\n    metadata:\n      name: pod-name\n";
        let d = &scan(m).docs[0];
        assert_eq!(d.kind, "Deployment", "nested kind must not overwrite");
        assert_eq!(
            d.api_version, "apps/v1",
            "nested apiVersion must not overwrite"
        );
        assert_eq!(d.name, "web", "metadata.name wins over a nested name");
        assert_eq!(d.namespace, "prod");
    }

    #[test]
    fn first_top_level_kind_wins() {
        // A document's `kind` is the FIRST top-level `kind:`; a later one must
        // not overwrite it (guards the `indent == 0 && kind.is_empty()` guard's
        // conjunction).
        let m = "apiVersion: v1\nkind: First\nkind: Second\nmetadata:\n  name: x\n";
        assert_eq!(scan(m).docs[0].kind, "First");
    }

    #[test]
    fn first_metadata_name_and_namespace_win() {
        // A duplicate name/namespace inside metadata must NOT overwrite the first
        // (guards the `name.is_empty()` / `namespace.is_empty()` `&&` conjuncts).
        let m = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: first\n  name: second\n  namespace: nsone\n  namespace: nstwo\n";
        let d = &scan(m).docs[0];
        assert_eq!(d.name, "first");
        assert_eq!(d.namespace, "nsone");
    }

    #[test]
    fn a_new_top_level_key_closes_the_metadata_block() {
        // When metadata has no name and a sibling top-level key (`spec`) begins,
        // the metadata block closes, so a `name:` deep in the spec is NOT taken
        // as the manifest name (guards the `key != "metadata"` close condition).
        let m = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  labels:\n    app: x\nspec:\n  template:\n    name: not-the-name\n";
        let d = &scan(m).docs[0];
        assert_eq!(
            d.name, "",
            "a spec-level name must not fill an empty metadata name"
        );
    }

    #[test]
    fn a_dedented_comment_does_not_drop_the_metadata_name() {
        // A comment dedented to the metadata key's own column must be SKIPPED,
        // not treated as a sibling key that closes metadata early (which would
        // lose the name). Guards the comment/blank-line skip.
        let m = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n# a dedented comment: with a colon\n  name: real-name\n";
        let d = &scan(m).docs[0];
        assert_eq!(d.name, "real-name");
    }

    #[test]
    fn scalar_quoting_dedup_and_empty_guards() {
        // Pins the helper contracts that only surface through scan():
        //  * unquote strips a fully-quoted scalar but leaves a half-quoted one
        //    verbatim (guards unquote's len/`&&`/slice logic);
        //  * push_image dedups repeats and drops empties;
        //  * push_ref drops an empty-named reference.
        let m = "apiVersion: v1\nkind: Deployment\nmetadata:\n  name: \"quoted-name\"\n  namespace: 'half\nspec:\n  template:\n    spec:\n      containers:\n        - name: a\n          image: app:1\n        - name: b\n          image: app:1\n        - name: c\n          image: \"\"\n        - name: d\n          image: other:2\n          envFrom:\n            - configMapRef:\n                name: \"\"\n            - secretRef:\n                name: real-secret\n";
        let d = &scan(m).docs[0];
        assert_eq!(d.name, "quoted-name", "a fully-quoted name is unwrapped");
        assert_eq!(
            d.namespace, "'half",
            "a half-quoted scalar is left verbatim"
        );
        assert_eq!(
            d.images,
            vec!["app:1", "other:2"],
            "repeated images dedup and an empty image is dropped"
        );
        assert_eq!(
            d.config_refs,
            vec![ConfigRef {
                kind: "Secret".into(),
                name: "real-secret".into()
            }],
            "an empty-named configMapRef is not a reference"
        );
    }

    #[test]
    fn service_backend_reference_is_captured() {
        // An Ingress-style `service:\n  name: X` block is a Service reference
        // (guards the `service` arm of ref_kind_of).
        let m = "apiVersion: networking.k8s.io/v1\nkind: Ingress\nmetadata:\n  name: ing\nspec:\n  rules:\n    - http:\n        paths:\n          - backend:\n              service:\n                name: web-svc\n";
        let d = &scan(m).docs[0];
        assert!(
            d.config_refs.contains(&ConfigRef {
                kind: "Service".into(),
                name: "web-svc".into()
            }),
            "the Ingress backend service must be a Service ref, got {:?}",
            d.config_refs
        );
    }
}
