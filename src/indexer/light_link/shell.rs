// Shell reference extraction (issue #205): `source`/`.` dot-command
// arguments, direct relative invocations, and `$VAR/`-prefixed paths. Split
// out of light_link.rs — see the parent module doc for the overall contract.

use super::Specifier;
use std::collections::HashSet;

/// Extracts shell reference specifiers (issue #205): `source X` / `. X`
/// dot-command arguments, direct relative invocations (`./tools/foo.sh`,
/// `../lib/bar.sh`) found anywhere on the line, and `$VAR/`-prefixed paths
/// (`$ROOT/rules/y.md`) — the last resolved separately by
/// `resolve_var_prefixed`, which requires the stripped remainder to match a
/// UNIQUE indexed File id. Comment lines (`#`) are skipped.
pub(super) fn extract_shell_specifiers(src: &str) -> Vec<Specifier> {
    let mut out = Vec::new();
    for raw in src.lines() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("source ") {
            if let Some(text) = first_shell_token(rest) {
                out.push(Specifier {
                    text,
                    method: "sh-source",
                });
            }
        } else if let Some(rest) = line.strip_prefix(". ") {
            if let Some(text) = first_shell_token(rest) {
                out.push(Specifier {
                    text,
                    method: "sh-source",
                });
            }
        }
        for raw_tok in line.split_whitespace() {
            let tok = strip_shell_quotes(raw_tok);
            if tok.starts_with("./") || tok.starts_with("../") {
                out.push(Specifier {
                    text: tok.to_string(),
                    method: "sh-invoke",
                });
            } else if let Some(remainder) = strip_var_prefix(tok) {
                out.push(Specifier {
                    text: remainder.to_string(),
                    method: "sh-var-prefix",
                });
            }
        }
    }
    out
}

/// The first whitespace-delimited token of `s`, quote-stripped. Used to read
/// the argument of a `source`/`.` dot-command.
fn first_shell_token(s: &str) -> Option<String> {
    let tok = strip_shell_quotes(s.split_whitespace().next()?);
    if tok.is_empty() {
        None
    } else {
        Some(tok.to_string())
    }
}

/// Strips a single layer of surrounding single or double quotes.
fn strip_shell_quotes(tok: &str) -> &str {
    tok.trim_matches(|c| c == '"' || c == '\'')
}

/// Strips a `$VAR/` or `${VAR}/` prefix from `tok`, returning the remainder,
/// or None when `tok` does not have that shape (not a candidate at all — no
/// guessing about what `$VAR` might resolve to; see `resolve_var_prefixed`
/// for the uniqueness gate on the remainder itself).
fn strip_var_prefix(tok: &str) -> Option<&str> {
    let rest = tok.strip_prefix('$')?;
    let after_name: &str = if let Some(braced) = rest.strip_prefix('{') {
        let end = braced.find('}')?;
        &braced[end + 1..]
    } else {
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
        &rest[end..]
    };
    after_name.strip_prefix('/').filter(|s| !s.is_empty())
}

/// Resolves a `$VAR/`-stripped remainder (e.g. `rules/y.md` from
/// `$ROOT/rules/y.md`) against the indexed File ids: exact match first, else
/// a suffix match at a path-component boundary (`id.ends_with("/" +
/// remainder)`). Returns `None` — never guesses — when zero or more than one
/// File id matches, since a shell `$VAR` could name any repo-root-like
/// directory and only a UNIQUE resolution is safe to assert as a fact. source:
/// issue #205 spec ("resolve $VAR/ prefixes only when the remainder resolves
/// to a unique existing repo file; otherwise skip, do not guess").
pub(super) fn resolve_var_prefixed(remainder: &str, file_ids: &HashSet<String>) -> Option<String> {
    if file_ids.contains(remainder) {
        return Some(remainder.to_string());
    }
    let suffix = format!("/{remainder}");
    let mut matches = file_ids.iter().filter(|id| id.ends_with(&suffix));
    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(first.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_shell_source_and_dot_command() {
        let src = "source tools/lib.sh\n. tools/other.sh\n# source commented.sh\n";
        let specs = extract_shell_specifiers(src);
        assert!(specs
            .iter()
            .any(|s| s.text == "tools/lib.sh" && s.method == "sh-source"));
        assert!(specs
            .iter()
            .any(|s| s.text == "tools/other.sh" && s.method == "sh-source"));
        assert!(!specs.iter().any(|s| s.text == "commented.sh"));
    }

    #[test]
    fn extracts_shell_direct_invocation() {
        let src = "./tools/foo.sh --flag\nbash ../lib/bar.sh\n";
        let specs = extract_shell_specifiers(src);
        assert!(specs
            .iter()
            .any(|s| s.text == "./tools/foo.sh" && s.method == "sh-invoke"));
        assert!(specs
            .iter()
            .any(|s| s.text == "../lib/bar.sh" && s.method == "sh-invoke"));
    }

    #[test]
    fn extracts_shell_var_prefixed_path() {
        let src = "cat $ROOT/rules/y.md\ncat ${BASE}/tools/z.sh\n";
        let specs = extract_shell_specifiers(src);
        assert!(specs
            .iter()
            .any(|s| s.text == "rules/y.md" && s.method == "sh-var-prefix"));
        assert!(specs
            .iter()
            .any(|s| s.text == "tools/z.sh" && s.method == "sh-var-prefix"));
    }

    #[test]
    fn var_prefix_positional_args_are_not_candidates() {
        // `$1`, `$@`, `$HOME` alone (no trailing `/...`) must not be treated
        // as reference specifiers at all.
        let src = "echo $1\necho $@\necho $HOME\n";
        let specs = extract_shell_specifiers(src);
        assert!(specs.is_empty());
    }

    #[test]
    fn resolve_var_prefixed_requires_unique_match() {
        let mut ids: HashSet<String> = HashSet::new();
        ids.insert("rules/y.md".to_string());
        assert_eq!(
            resolve_var_prefixed("rules/y.md", &ids),
            Some("rules/y.md".to_string())
        );
        // Suffix match at a component boundary also works.
        ids.clear();
        ids.insert("project/rules/y.md".to_string());
        assert_eq!(
            resolve_var_prefixed("rules/y.md", &ids),
            Some("project/rules/y.md".to_string())
        );
        // Ambiguous (two files share the suffix) → skip, do not guess.
        ids.insert("other/rules/y.md".to_string());
        assert_eq!(resolve_var_prefixed("rules/y.md", &ids), None);
        // No match at all → skip.
        ids.clear();
        assert_eq!(resolve_var_prefixed("rules/y.md", &ids), None);
    }
}
