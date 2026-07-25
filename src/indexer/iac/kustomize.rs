// Kustomize overlay parsing — the resource/base/patch reference lists of a
// `kustomization.yaml`.
//
// Mirrors DeusData/codebase-memory-mcp pass_k8s.c `handle_kustomize`: a
// kustomization declares the set of manifests it composes (`resources:`),
// the overlays it extends (`bases:` / `components:`), and the manifests it
// patches (`patches:` / `patchesStrategicMerge:`). Each entry is a repo-relative
// path to a file or an overlay directory. We collect exactly those paths; the
// emitter resolves each to an indexed File and draws the reference edge.
//
// This is a targeted line scan, not a YAML parser: kustomization files are a
// small, flat, well-known subset (a handful of top-level list keys), so a
// dependency-free scan is both sufficient and precise about which lines it
// could not interpret. source: issue #63 criterion 3; NO new heavyweight YAML
// dependency (design constraint 2) — pass_k8s.c uses the same hand scan.

/// Top-level keys whose list items are path references to other manifests or
/// overlays. source: kustomize.io reference — the composition keys.
const REF_LIST_KEYS: &[&str] = &[
    "resources",
    "bases",
    "components",
    "patchesstrategicmerge",
    "patchesjson6902",
    "patches",
];

/// Upper bound on collected references — a kustomization composing thousands of
/// resources is adversarial; the cap bounds work (issue #63 criterion 6).
const MAX_REFS: usize = 512;

/// The path references declared by a kustomization overlay.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Kustomization {
    /// Repo-relative paths (files or overlay directories) this overlay composes.
    pub refs: Vec<String>,
}

/// Parses a `kustomization.yaml` for its resource/base/patch path references.
///
/// Precondition: `source` is the UTF-8 text of a kustomization file.
/// Postcondition: `refs` holds every path scalar found under a recognized
/// composition key (`resources`/`bases`/`components`/`patches*`), in file
/// order, deduplicated, capped at `MAX_REFS`. Non-path constructs (inline
/// generators, literal values) are ignored — never an error.
pub fn parse(source: &str) -> Kustomization {
    let mut out = Kustomization::default();
    // The indentation of the currently-active composition list key, if we are
    // inside one. `None` once a shallower-or-equal key of a different kind
    // begins. A recognized key with an inline value (`resources: []`) does not
    // open a list.
    let mut active_list_indent: Option<usize> = None;

    for raw in source.lines() {
        let indent = indent_of(raw);
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // A `key:` line at this indent either opens/closes a composition list.
        if let Some((key, value)) = split_key(line) {
            // A list item (`- x`) is handled below, not here — `split_key`
            // returns None for those, so reaching here means a real mapping key.
            let is_ref_key = REF_LIST_KEYS.contains(&key.to_ascii_lowercase().as_str());
            if is_ref_key && value.is_empty() {
                active_list_indent = Some(indent);
                continue;
            }
            // Any other mapping key at or above the active list's indent closes
            // the active list (we have left its block).
            if let Some(active) = active_list_indent {
                if indent <= active {
                    active_list_indent = None;
                }
            }
            // `patches:` object form nests `- path: <file>`; the `path:` key is
            // captured by the list-item branch below, so nothing to do here.
            continue;
        }

        // A list item under an active composition list.
        if let Some(active) = active_list_indent {
            if indent <= active {
                // Dedented out of the list without a new key (rare) — close it.
                active_list_indent = None;
                continue;
            }
            if let Some(item) = line.strip_prefix("- ") {
                let item = item.trim();
                // Object item `- path: overlays/x.yaml` (patches form) or a bare
                // scalar `- deployment.yaml` (resources form).
                let path = if let Some((k, v)) = split_key(item) {
                    if k.eq_ignore_ascii_case("path") {
                        Some(unquote(v))
                    } else {
                        None
                    }
                } else {
                    Some(unquote(item))
                };
                if let Some(p) = path {
                    push_ref(&mut out.refs, p);
                }
            } else if let Some((k, v)) = split_key(line) {
                // Continuation line of an object item, e.g. a bare `path: x`.
                if k.eq_ignore_ascii_case("path") {
                    push_ref(&mut out.refs, unquote(v));
                }
            }
        }
    }

    out
}

/// Number of leading spaces (YAML forbids tabs for indentation).
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// Splits `key: value` into (key, value), or `None` if the trimmed line is not
/// a mapping entry (e.g. a `- item`). A key with no value returns an empty
/// value string. Only splits on the FIRST `: ` / trailing `:` so values
/// containing colons survive.
fn split_key(trimmed: &str) -> Option<(&str, &str)> {
    if trimmed.starts_with('-') {
        return None;
    }
    if let Some(rest) = trimmed.strip_suffix(':') {
        return Some((rest.trim(), ""));
    }
    let idx = trimmed.find(": ")?;
    Some((trimmed[..idx].trim(), trimmed[idx + 2..].trim()))
}

/// Strips surrounding single/double quotes from a scalar.
fn unquote(s: &str) -> String {
    let t = s.trim();
    if (t.starts_with('"') && t.ends_with('"') && t.len() >= 2)
        || (t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2)
    {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

/// Appends `p` if non-empty, not already present, and under the cap.
fn push_ref(refs: &mut Vec<String>, p: String) {
    if p.is_empty() || refs.len() >= MAX_REFS || refs.contains(&p) {
        return;
    }
    refs.push(p);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_resources_and_bases() {
        let k = "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nresources:\n  - deployment.yaml\n  - service.yaml\nbases:\n  - ../base\n";
        let out = parse(k);
        assert_eq!(out.refs, vec!["deployment.yaml", "service.yaml", "../base"]);
    }

    #[test]
    fn collects_patch_object_paths() {
        let k = "resources:\n  - ../base\npatches:\n  - path: patch-replicas.yaml\n    target:\n      kind: Deployment\n  - path: patch-image.yaml\n";
        let out = parse(k);
        assert_eq!(
            out.refs,
            vec!["../base", "patch-replicas.yaml", "patch-image.yaml"]
        );
    }

    #[test]
    fn inline_empty_list_opens_nothing() {
        let k = "resources: []\ncomponents:\n  - ../comp\n";
        let out = parse(k);
        assert_eq!(out.refs, vec!["../comp"]);
    }

    #[test]
    fn ignores_non_ref_keys() {
        let k = "namePrefix: prod-\ncommonLabels:\n  env: prod\nresources:\n  - app.yaml\n";
        let out = parse(k);
        assert_eq!(out.refs, vec!["app.yaml"]);
    }

    #[test]
    fn dedups_and_unquotes() {
        let k = "resources:\n  - \"a.yaml\"\n  - a.yaml\n  - 'b.yaml'\n";
        let out = parse(k);
        assert_eq!(out.refs, vec!["a.yaml", "b.yaml"]);
    }

    #[test]
    fn blank_and_comment_lines_inside_a_list_do_not_close_it() {
        // A blank line or a comment between list items must be SKIPPED, not
        // treated as a dedent that closes the resources block (which would drop
        // every ref after it). Guards the `is_empty() || starts_with('#')` skip.
        let k = "resources:\n  - a.yaml\n\n  # a comment\n  - b.yaml\n";
        let out = parse(k);
        assert_eq!(out.refs, vec!["a.yaml", "b.yaml"]);
    }

    #[test]
    fn half_quoted_scalars_are_not_stripped() {
        // `unquote` must strip a quote ONLY when the scalar is wrapped on BOTH
        // sides. A trailing-only quote is content and must survive verbatim —
        // guards the two `&&` in the quote-pair predicate.
        let k = "resources:\n  - value\"\n  - value'\n";
        let out = parse(k);
        assert_eq!(out.refs, vec!["value\"", "value'"]);
    }

    #[test]
    fn empty_scalar_is_not_pushed_as_a_reference() {
        // An empty list item (`- ""`) must not become an empty reference —
        // guards the `p.is_empty() ||` short-circuit in push_ref.
        let k = "resources:\n  - \"\"\n  - a.yaml\n";
        let out = parse(k);
        assert_eq!(out.refs, vec!["a.yaml"]);
    }
}
