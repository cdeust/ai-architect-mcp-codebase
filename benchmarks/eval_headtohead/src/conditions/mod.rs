//! The two conditions under test (§Move 2): GRAPH (AP tools) and EXPLORER
//! (Grep/Glob/Read baseline). Both produce a uniform [`Answer`] so the metrics
//! score them identically — no side gets credit the other cannot.

pub mod explorer;
pub mod graph;

use std::collections::BTreeSet;
use std::path::Path;

/// A condition's answer to one question, in the shape the metrics consume.
#[derive(Debug, Clone)]
pub struct Answer {
    /// Files this condition surfaced as the answer (scored against ground truth).
    pub files: BTreeSet<String>,
    /// Characters of context this condition consumed (the token proxy's raw
    /// count; tokens = chars / proxy).
    pub chars: usize,
    /// Number of tool calls issued.
    pub tool_calls: usize,
    /// Human/judge-readable rendering of the answer (fed to the blinded judge).
    pub text: String,
}

impl Answer {
    /// Token estimate under the stated proxy (chars / chars-per-token).
    pub fn tokens(&self, chars_per_token: usize) -> f64 {
        self.chars as f64 / chars_per_token.max(1) as f64
    }
}

/// Map a qualified name (`<file>::<symbol>` or `<file>::Sym#N`) to its file.
/// Every AP node id/qn is file-prefixed, so the prefix before the first `::` is
/// the originating file (verified across python/typescript/go/rust corpora).
pub fn file_of_qn(qn: &str) -> String {
    qn.split("::").next().unwrap_or(qn).to_string()
}

/// Depth-first walk of a corpus directory, invoking `f(relative_path, contents)`
/// for every readable UTF-8 file. Shared by the explorer condition and the
/// corpus hasher. Deterministic order is not required (callers build sets).
pub fn visit_files(root: &Path, f: &mut impl FnMut(&str, &str)) {
    fn walk(base: &Path, dir: &Path, f: &mut impl FnMut(&str, &str)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, f);
            } else if let Ok(content) = std::fs::read_to_string(&path) {
                let rel = path.strip_prefix(base).unwrap_or(&path).to_string_lossy();
                f(&rel, &content);
            }
        }
    }
    walk(root, root, f);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_of_qn_strips_symbol() {
        assert_eq!(file_of_qn("core.py::process_order"), "core.py");
        // Go carries an overload ordinal on the symbol, never the file.
        assert_eq!(file_of_qn("core.go::ProcessOrder#3"), "core.go");
        // A nested qualified name keeps only the file prefix.
        assert_eq!(file_of_qn("api.ts::OrderConfig::constructor"), "api.ts");
        // A bare token (no separator) maps to itself.
        assert_eq!(file_of_qn("main.rs"), "main.rs");
    }
}
