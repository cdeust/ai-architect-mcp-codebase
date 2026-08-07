// Markdown reference extraction (issue #205): `[text](path)` links, inline
// backtick-quoted paths, and bare relative-path mentions in prose. Split out
// of light_link.rs — see the parent module doc for the overall contract.

use super::Specifier;

/// Combines the three Markdown reference-extraction methods (issue #205):
/// `[text](path)` links, backtick-quoted paths, and bare relative-path
/// mentions in prose. Each specifier keeps the method that found it so the
/// emitted edge's `resolution_method` names its provenance. Overlap between
/// the three (the same path found via link AND backtick) is harmless — the
/// caller dedups by `(from, target)` before insertion.
pub(super) fn extract_markdown_specifiers(src: &str) -> Vec<Specifier> {
    let mut out: Vec<Specifier> = extract_markdown_targets(src)
        .into_iter()
        .map(|text| Specifier {
            text,
            method: "md-link",
        })
        .collect();
    out.extend(
        extract_backtick_paths(src)
            .into_iter()
            .map(|text| Specifier {
                text,
                method: "md-backtick",
            }),
    );
    out.extend(extract_bare_targets(src).into_iter().map(|text| Specifier {
        text,
        method: "md-bare",
    }));
    out
}

/// Extracts the URL targets of inline Markdown links `[text](url)` that point
/// at other files in the repo. External URLs (`http`, `mailto:`, protocol-
/// relative `//`), in-page anchors (`#…`), and absolute paths (`/…`) are
/// dropped — only repo-local references remain. A trailing `#anchor` or
/// `"title"` on the URL is stripped.
fn extract_markdown_targets(src: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    // Scan for the `](` sequence that opens a Markdown link destination.
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b'(' {
            let start = i + 2;
            let mut j = start;
            let mut depth = 1; // balance nested parens in the URL
            while j < bytes.len() {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            if j <= bytes.len() && j > start {
                let raw = &src[start..j];
                if let Some(url) = clean_md_url(raw) {
                    targets.push(url);
                }
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    targets
}

/// Normalizes a Markdown link destination to a repo-local path, or None if it
/// is external / an anchor / absolute.
fn clean_md_url(raw: &str) -> Option<String> {
    let mut url = raw.trim();
    // Drop an optional title: `(path "Title")`.
    if let Some(sp) = url.find(char::is_whitespace) {
        url = &url[..sp];
    }
    // Drop a fragment / query.
    url = url.split('#').next().unwrap_or(url);
    url = url.split('?').next().unwrap_or(url);
    if url.is_empty() {
        return None;
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || url.starts_with("//")
        || url.starts_with('/')
    {
        return None;
    }
    Some(url.to_string())
}

/// Extracts inline single-backtick spans (`` `path` ``) that look path-like
/// (see `is_path_like`). Fenced code blocks (paragraphs delimited by a
/// ` ``` ` line) are skipped entirely — their contents are code, not prose
/// references — so this only sees genuine inline-code path mentions.
fn extract_backtick_paths(src: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut in_fence = false;
    for line in src.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut open: Option<usize> = None;
        for (i, c) in line.char_indices() {
            if c != '`' {
                continue;
            }
            match open {
                None => open = Some(i + 1),
                Some(start) => {
                    let inner = &line[start..i];
                    if is_path_like(inner) {
                        targets.push(inner.to_string());
                    }
                    open = None;
                }
            }
        }
    }
    targets
}

/// Extracts bare (unmarked) relative-path mentions from prose — a whitespace-
/// delimited token, stripped of common enclosing/trailing punctuation, that
/// contains a `/` (the directory component is the signal that distinguishes
/// "this looks like a path" from an ordinary word — backtick spans get the
/// weaker `is_path_like` test because backticks are already a strong "this is
/// code/a path" signal). Skipped inside fenced code blocks (already covered,
/// more precisely, by the AST/backtick paths). The real precision backstop is
/// downstream: a bare token only becomes an edge if it resolves EXACTLY to an
/// indexed File id (`resolve_exact`) — garbage tokens (e.g. a stray `](` left
/// over from a Markdown link syntax this pass does not parse) simply fail to
/// resolve and are dropped, never guessed at.
fn extract_bare_targets(src: &str) -> Vec<String> {
    const TRIM: &[char] = &[
        '(', ')', '[', ']', '`', ',', '.', ';', ':', '"', '\'', '*', '!', '?',
    ];
    let mut targets = Vec::new();
    let mut in_fence = false;
    for line in src.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        for raw_tok in line.split_whitespace() {
            let tok = raw_tok.trim_matches(|c: char| TRIM.contains(&c));
            if tok.is_empty() || !tok.contains('/') {
                continue;
            }
            if tok.starts_with("http://") || tok.starts_with("https://") || tok.starts_with('$') {
                continue;
            }
            targets.push(tok.to_string());
        }
    }
    targets
}

/// True when `s` plausibly names a file: no internal whitespace, not a URL or
/// shell variable, and either has a directory component (`/`) or a dotted
/// extension (`name.ext`). This is noise reduction ONLY — the binding
/// correctness gate is the downstream exact-match resolution against the
/// indexed File id set (coding-standards.md §8: no fuzzy matching).
fn is_path_like(s: &str) -> bool {
    if s.is_empty() || s.len() > 200 || s.chars().any(char::is_whitespace) {
        return false;
    }
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with('$') {
        return false;
    }
    if s.contains('/') {
        return true;
    }
    match s.rfind('.') {
        Some(i) => i > 0 && i < s.len() - 1,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_markdown_link_targets() {
        let src = "See [the standard](rules/coding-standards.md) for details.";
        let targets = extract_markdown_targets(src);
        assert_eq!(targets, vec!["rules/coding-standards.md".to_string()]);
    }

    #[test]
    fn extracts_backtick_paths_and_skips_fenced_code() {
        let src = r#"
Run `tools/memory-tool.sh` to check state.
Also see `rules/coding-standards.md` §4.

```
this is `not/a/reference` — inside a fenced code block, must be skipped
```

A plain word like `foo` (no dot, no slash) is not path-like.
"#;
        let targets = extract_backtick_paths(src);
        assert!(targets.contains(&"tools/memory-tool.sh".to_string()));
        assert!(targets.contains(&"rules/coding-standards.md".to_string()));
        assert!(!targets.iter().any(|t| t == "not/a/reference"));
        assert!(!targets.iter().any(|t| t == "foo"));
    }

    #[test]
    fn extracts_bare_relative_paths_requiring_a_slash() {
        let src = "The engineer agent enforces rules/coding-standards.md at all stakes, \
                    see tools/memory-tool.sh, but plain-word.md alone has no slash.";
        let targets = extract_bare_targets(src);
        assert!(targets.contains(&"rules/coding-standards.md".to_string()));
        assert!(targets.contains(&"tools/memory-tool.sh".to_string()));
        assert!(!targets.iter().any(|t| t == "plain-word.md"));
    }

    #[test]
    fn bare_targets_strip_trailing_sentence_punctuation() {
        let src = "Follow rules/coding-standards.md, then read tools/memory-tool.sh.";
        let targets = extract_bare_targets(src);
        assert!(targets.contains(&"rules/coding-standards.md".to_string()));
        assert!(targets.contains(&"tools/memory-tool.sh".to_string()));
    }

    #[test]
    fn is_path_like_rejects_non_paths() {
        assert!(is_path_like("rules/coding-standards.md"));
        assert!(is_path_like("memory-tool.sh"));
        assert!(!is_path_like("foo")); // no dot, no slash
        assert!(!is_path_like("has whitespace/x.md"));
        assert!(!is_path_like("https://example.com/x.md"));
        assert!(!is_path_like("$VAR/x.md"));
        assert!(!is_path_like(""));
    }
}
