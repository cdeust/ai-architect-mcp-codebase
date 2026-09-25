// graph_store::cfg_twins: what a `#cfg(..)` id suffix means to the graph layer
// (issue #353).
//
// Two items of a name under mutually exclusive `#[cfg]` predicates are two nodes.
// The parser names each by appending `#cfg(<compact gate>)` to its qualified
// name (`src/lib.rs::pick#cfg(not(feature=fast))`); a member of a twin module
// carries the suffix in the middle of its name (`src/lib.rs::m#cfg(unix)::f`).
// This module is the one place that reads that shape back, so the resolver, the
// impact tools and the status counters cannot disagree on it.

/// Opens the suffix of a twin id. Mirrors `parser::spec::rust_cfg_gate::TWIN_MARK`
/// (a test below keeps the two equal): the graph layer does not import the parser.
pub(crate) const TWIN_MARK: &str = "#cfg(";

/// Attribution of a `CallSite` left unresolved on purpose: every candidate for
/// its callee is one of several twins of one item, so no single edge is honest.
/// Written by the static resolver (`resolver::calls`); the impact tools and
/// `index_status` read it in the next change. `''` still means "not attributed".
/// source: issue #353.
pub const CALLSITE_UNRESOLVED_REASON_CFG_TWINS: &str = "cfg_twins";

/// The node tables that carry the `cfg_gate` column, exactly those of the DDL.
///
/// The column is created ONLY by `create_schema` on a graph built by this code:
/// nothing anywhere adds it with an `ALTER` (a test scans the sources for one).
/// That is what makes its presence a proof that the graph was indexed with twin
/// identity: an old graph has no column and is refused for incremental work
/// (`require_cfg_gate_metadata`), and a full reindex removes the graph directory
/// first (`indexing_handlers::do_index_codebase`), so the new tables replace the
/// old ones. A future change that adds the column with an `ALTER` must ALSO write
/// a marker only the new indexer sets, or old collapsed graphs would pass the
/// guard.
pub const CFG_GATE_LABELS: [&str; 10] = [
    "Module",
    "Function",
    "Method",
    "Struct",
    "Enum",
    "Variant",
    "Trait",
    "Field",
    "Constant",
    "TypeAlias",
];

/// True when `qn` carries a twin suffix anywhere in its path.
pub(crate) fn has_cfg_gate(qn: &str) -> bool {
    qn.contains(TWIN_MARK)
}

/// `qn` without its twin suffixes: the name every twin of an item shares. A
/// suffix that never closes is left alone, so an odd id is never mangled.
pub(crate) fn strip_cfg_gates(qn: &str) -> String {
    let mut out = String::with_capacity(qn.len());
    let mut rest = qn;
    while let Some(start) = rest.find(TWIN_MARK) {
        let after = &rest[start + TWIN_MARK.len()..];
        let Some(close) = closing_paren(after) else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The index of the `)` that closes a group already opened, in `text`.
fn closing_paren(text: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_equals_the_parsers() {
        assert_eq!(TWIN_MARK, crate::parser::CFG_TWIN_MARK);
    }

    #[test]
    fn a_leaf_suffix_is_stripped() {
        assert_eq!(
            strip_cfg_gates("src/lib.rs::pick#cfg(not(feature=fast))"),
            "src/lib.rs::pick"
        );
    }

    #[test]
    fn a_suffix_in_the_middle_of_a_path_is_stripped() {
        assert_eq!(
            strip_cfg_gates("src/lib.rs::m#cfg(unix)::f"),
            "src/lib.rs::m::f"
        );
    }

    #[test]
    fn several_suffixes_are_all_stripped() {
        assert_eq!(
            strip_cfg_gates("src/lib.rs::m#cfg(unix)::S#cfg(all(a,b))::f#cfg(kani)"),
            "src/lib.rs::m::S::f"
        );
    }

    #[test]
    fn a_name_without_a_suffix_and_an_unclosed_one_are_unchanged() {
        assert_eq!(strip_cfg_gates("src/lib.rs::pick"), "src/lib.rs::pick");
        assert_eq!(
            strip_cfg_gates("src/lib.rs::pick#cfg(x"),
            "src/lib.rs::pick#cfg(x"
        );
        assert!(!has_cfg_gate("src/lib.rs::pick"));
        assert!(has_cfg_gate("src/lib.rs::m#cfg(unix)::f"));
    }

    #[test]
    fn a_sequence_suffix_is_not_a_twin_suffix() {
        assert_eq!(strip_cfg_gates("src/lib.rs::f#3"), "src/lib.rs::f#3");
    }
}
