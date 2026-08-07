// JS-family import/require extraction and Node-style resolution.
// Split out of light_link.rs (issue #205) — see the parent module doc for the
// overall light-linking contract.

use super::{normalize, JS_RESOLVE_SUFFIXES};
use std::collections::HashSet;
use std::path::Path;

/// Extracts the quoted specifiers of relative imports/requires on each line.
/// Only relative specifiers (starting with `.`) are returned — bare package
/// names ("react") have no File node to link to. Comment lines are skipped.
pub(super) fn extract_relative_specifiers(src: &str) -> Vec<String> {
    let mut specs = Vec::new();
    for raw in src.lines() {
        let line = raw.trim_start();
        if line.starts_with("//") || line.starts_with('*') || line.starts_with("/*") {
            continue;
        }
        for anchor in [" from ", "from\"", "from'", "require(", "import("] {
            let mut search_from = 0usize;
            while let Some(rel_pos) = raw[search_from..].find(anchor) {
                let after = search_from + rel_pos + anchor.len();
                if let Some(spec) = first_quoted(&raw[after..]) {
                    if spec.starts_with('.') {
                        specs.push(spec);
                    }
                }
                search_from = after;
            }
        }
    }
    specs
}

/// Reads the first single/double/back-quoted string at the start of `s`,
/// skipping leading whitespace and a single optional `(`. Returns the inner
/// text, or None if `s` does not begin with a quoted token.
fn first_quoted(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'(') {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let quote = bytes[i];
    if quote != b'"' && quote != b'\'' && quote != b'`' {
        return None;
    }
    i += 1;
    let begin = i;
    while i < bytes.len() && bytes[i] != quote {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    Some(s[begin..i].to_string())
}

/// Resolves a relative specifier (`./x`, `../y/z`) against the set of indexed
/// File ids using Node-style suffix resolution. Returns the matched File id.
pub(super) fn resolve_specifier(
    from_id: &str,
    spec: &str,
    file_ids: &HashSet<String>,
) -> Option<String> {
    let from_dir = Path::new(from_id).parent().unwrap_or_else(|| Path::new(""));
    // Try the specifier relative to the referrer's directory first, then as a
    // repo-root path (Markdown links are often written root-relative). For each
    // base, try Node-style suffixes so bare JS specifiers (`./util`) resolve.
    for base_path in [normalize(&from_dir.join(spec)), normalize(Path::new(spec))] {
        let base = base_path.to_string_lossy().replace('\\', "/");
        if base.is_empty() {
            continue;
        }
        for suffix in JS_RESOLVE_SUFFIXES {
            let candidate = format!("{base}{suffix}");
            if file_ids.contains(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_relative_import_and_require() {
        let src = r#"
            import { a } from './util.js';
            import b from "../core/b";
            const c = require('./c');
            const dyn = import("./d.mjs");
            import react from "react";   // bare — must be ignored
            // import x from './commented';
        "#;
        let specs = extract_relative_specifiers(src);
        assert!(specs.contains(&"./util.js".to_string()));
        assert!(specs.contains(&"../core/b".to_string()));
        assert!(specs.contains(&"./c".to_string()));
        assert!(specs.contains(&"./d.mjs".to_string()));
        assert!(!specs.iter().any(|s| s == "react"));
    }

    #[test]
    fn resolves_with_node_suffixes() {
        let ids: HashSet<String> = ["ui/js/app.js", "ui/js/util.js", "ui/core/index.js"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            resolve_specifier("ui/js/app.js", "./util", &ids),
            Some("ui/js/util.js".to_string())
        );
        assert_eq!(
            resolve_specifier("ui/js/app.js", "../core", &ids),
            Some("ui/core/index.js".to_string())
        );
        assert_eq!(resolve_specifier("ui/js/app.js", "./missing", &ids), None);
    }
}
