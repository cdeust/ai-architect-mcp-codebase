// resolver::cfg_verdict: whether the build compiles one `#[cfg]` twin, shared by
// the static pass (`cfg_select`) and the language-server pass (issue #366).
//
// Two facts decide it, in this order:
//
// 1. The caller's own gate. A caller whose id carries `#cfg(unix)` exists only
//    when `unix` holds, so it reaches `helper#cfg(unix)` and never
//    `helper#cfg(not(unix))`, whatever the default build says. This is
//    syntactic: the twin is compiled when every conjunct of its gate is a
//    conjunct of the caller's, and compiled out when the caller holds the
//    negation of one of its conjuncts. Nothing is solved; a gate that needs
//    reasoning stays undecided.
// 2. The default profile: the `cfg_active` column the indexer wrote from the
//    package's default features (`indexer::cfg_active`).
//
// A node that is not a twin has no gate and no `cfg_active`, so its verdict is
// `Undecided` and neither pass treats it differently.
//
// Part B of #366 adds the file: the gate a file inherits from the `mod`
// declarations that lead to it joins the item's own gates in the syntactic
// check (a caller in the `cfg(unix)` file reaches the `cfg(unix)` twin file), and
// the file's default-build verdict decides a twin whose id carries no gate. It is
// never folded into the item's `cfg_active`: an undecided file gate says nothing
// about the same-file twins inside it.

use std::collections::HashMap;

use crate::graph_store::{cfg_gates_in, FileCfg, GraphStore, CFG_ACTIVE, CFG_INACTIVE};
use crate::parser::cfg_compact::parse_compact;
use crate::parser::cfg_expr::CfgPredicate;

/// `cfg_active` of every twin of the graph, and the cfg facts of every file
/// under a gate or compiled out, read once per pass.
#[derive(Default)]
pub(crate) struct TwinView {
    active: HashMap<String, String>,
    files: HashMap<String, FileCfg>,
}

impl TwinView {
    pub(crate) fn load(store: &GraphStore) -> Self {
        TwinView {
            active: store.cfg_active_by_id(),
            files: store.file_cfg_by_id(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with(entries: &[(&str, &str)]) -> Self {
        TwinView {
            active: entries
                .iter()
                .map(|(id, v)| (id.to_string(), v.to_string()))
                .collect(),
            files: HashMap::new(),
        }
    }

    /// Adds the facts of `file` (`(module_path, gate, active)`).
    #[cfg(test)]
    pub(crate) fn with_file(mut self, file: &str, facts: (&str, &str, &str)) -> Self {
        let (module_path, gate, active) = facts;
        self.files.insert(
            file.to_string(),
            FileCfg {
                module_path: module_path.to_string(),
                gate: gate.to_string(),
                active: active.to_string(),
            },
        );
        self
    }

    fn file(&self, id: &str) -> Option<&FileCfg> {
        let file = crate::language_provider::extract_file_prefix(id)?;
        self.files.get(&file)
    }

    /// Whether the file of `id` sits under a gate it inherits from `mod`
    /// declarations.
    pub(crate) fn file_is_gated(&self, id: &str) -> bool {
        self.file(id).is_some_and(|f| !f.gate.is_empty())
    }

    /// `id` as its module path spells it (`src/lib.rs::imp::pick` for
    /// `src/unix.rs::pick` in `mod imp`), `None` when its file has none.
    pub(crate) fn logical_id(&self, id: &str) -> Option<String> {
        let file = crate::language_provider::extract_file_prefix(id)?;
        let module_path = &self.files.get(&file)?.module_path;
        if module_path.is_empty() {
            return None;
        }
        let rest = id.strip_prefix(&file)?.strip_prefix("::")?;
        Some(format!("{module_path}::{rest}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Compiled,
    NotCompiled,
    Undecided,
}

/// The conjuncts of the gates written in `qn`, or `None` when one gate does not
/// parse (nothing is then known about the item).
fn conjuncts(qn: &str) -> Option<Vec<CfgPredicate>> {
    let mut out: Vec<CfgPredicate> = Vec::new();
    for gate in cfg_gates_in(qn) {
        let parts = match parse_compact(gate)? {
            CfgPredicate::All(items) => items,
            single => vec![single],
        };
        for part in parts {
            if !out.contains(&part) {
                out.push(part);
            }
        }
    }
    Some(out)
}

fn negation(predicate: &CfgPredicate) -> CfgPredicate {
    match predicate {
        CfgPredicate::Not(inner) => (**inner).clone(),
        other => CfgPredicate::Not(Box::new(other.clone())),
    }
}

/// The gates written in `qn` joined with the gate its file inherits, or `None`
/// when one of them does not parse.
fn conjuncts_in_file(view: &TwinView, qn: &str) -> Option<Vec<CfgPredicate>> {
    let mut out = conjuncts(qn)?;
    let file_gate = view.file(qn).map(|f| f.gate.as_str()).unwrap_or("");
    if !file_gate.is_empty() {
        let parts = match parse_compact(file_gate)? {
            CfgPredicate::All(items) => items,
            single => vec![single],
        };
        for part in parts {
            if !out.contains(&part) {
                out.push(part);
            }
        }
    }
    Some(out)
}

/// The gates a caller holds, read from its qualified name or from the id of a
/// call site inside it (the site id starts with the caller's id), and from the
/// gate its file inherits. An unparsable gate holds nothing.
pub(crate) fn caller_gates(view: &TwinView, caller: &str) -> Vec<CfgPredicate> {
    conjuncts_in_file(view, caller).unwrap_or_default()
}

/// Whether the build compiles the node `id` (qualified name `qn`, which carries
/// its gates) for a caller holding `caller`.
pub(crate) fn verdict(view: &TwinView, caller: &[CfgPredicate], id: &str, qn: &str) -> Verdict {
    if let Some(gate) = conjuncts_in_file(view, qn) {
        if !gate.is_empty() && gate.iter().all(|c| caller.contains(c)) {
            return Verdict::Compiled;
        }
        if gate.iter().any(|c| caller.contains(&negation(c))) {
            return Verdict::NotCompiled;
        }
    }
    let file_active = view.file(id).map(|f| f.active.as_str());
    if file_active == Some(CFG_INACTIVE) {
        return Verdict::NotCompiled;
    }
    match view.active.get(id).map(String::as_str) {
        Some(CFG_ACTIVE) => Verdict::Compiled,
        Some(CFG_INACTIVE) => Verdict::NotCompiled,
        // An item without a gate of its own exists when its file does.
        _ if file_active == Some(CFG_ACTIVE) && cfg_gates_in(qn).is_empty() => Verdict::Compiled,
        _ => Verdict::Undecided,
    }
}

/// Whether a caller (or a call site inside it) holding the gates of `caller`
/// reaches a node the build compiles out. Used by the language-server pass,
/// which must not record a definition in a twin the build does not compile.
pub(crate) fn is_compiled_out(view: &TwinView, caller: &str, id: &str) -> bool {
    verdict(view, &caller_gates(view, caller), id, id) == Verdict::NotCompiled
}

/// Deletes the language-server rows to a twin `is_compiled_out` rules out for
/// their caller, before the resolve pass decides those sites again.
pub(crate) fn reset_compiled_out_lsp_rows(store: &GraphStore) -> Result<usize, String> {
    let view = TwinView::load(store);
    if view.active.is_empty() && view.files.is_empty() {
        return Ok(0);
    }
    // The gated and the compiled-out files: their items are twins whose ids
    // carry no gate, so the store finds their rows through the file.
    let twin_files: Vec<String> = view.files.keys().cloned().collect();
    store.reset_lsp_twin_rows(&twin_files, |caller, target| {
        is_compiled_out(&view, caller, target)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: &str = "src/lib.rs::pick#cfg(feature=fast)";
    const SLOW: &str = "src/lib.rs::pick#cfg(not(feature=fast))";

    fn of(view: &TwinView, caller: &str, target: &str) -> Verdict {
        verdict(view, &caller_gates(view, caller), target, target)
    }

    #[test]
    fn the_profile_decides_a_twin_for_an_ungated_caller() {
        let view = TwinView::with(&[(FAST, "inactive"), (SLOW, "active")]);
        let site = "src/lib.rs::caller::call@3:4";
        assert_eq!(of(&view, site, FAST), Verdict::NotCompiled);
        assert_eq!(of(&view, site, SLOW), Verdict::Compiled);
        assert!(is_compiled_out(&view, site, FAST));
        assert!(!is_compiled_out(&view, site, SLOW));
    }

    #[test]
    fn a_callers_gate_outranks_the_profile() {
        let view = TwinView::with(&[(FAST, "inactive"), (SLOW, "active")]);
        let site = "src/lib.rs::run#cfg(feature=fast)::call@3:4";
        assert_eq!(of(&view, site, FAST), Verdict::Compiled);
        assert_eq!(of(&view, site, SLOW), Verdict::NotCompiled);
        assert!(!is_compiled_out(&view, site, FAST));
    }

    #[test]
    fn an_unknown_profile_and_a_plain_node_are_undecided() {
        let view = TwinView::with(&[(FAST, "unknown")]);
        let site = "src/lib.rs::caller::call@3:4";
        assert_eq!(of(&view, site, FAST), Verdict::Undecided);
        assert_eq!(of(&view, site, "src/lib.rs::plain"), Verdict::Undecided);
        assert!(!is_compiled_out(&view, site, FAST));
        assert!(!is_compiled_out(&view, site, "src/lib.rs::plain"));
    }

    /// The trap of part B: same-file twins inside a `cfg(unix)` file are still
    /// decided by their own gates, because the file's undecided gate is never
    /// folded into them.
    #[test]
    fn twins_inside_an_undecided_file_are_decided_by_their_own_gates() {
        const IN_UNIX_FAST: &str = "src/unix.rs::pick#cfg(feature=fast)";
        const IN_UNIX_SLOW: &str = "src/unix.rs::pick#cfg(not(feature=fast))";
        let view = TwinView::with(&[(IN_UNIX_FAST, "inactive"), (IN_UNIX_SLOW, "active")])
            .with_file("src/unix.rs", ("src/lib.rs::imp", "unix", "unknown"));
        let site = "src/lib.rs::caller::call@3:4";
        assert_eq!(of(&view, site, IN_UNIX_FAST), Verdict::NotCompiled);
        assert_eq!(of(&view, site, IN_UNIX_SLOW), Verdict::Compiled);
    }

    #[test]
    fn a_twin_file_is_decided_by_its_file_and_by_the_callers_file() {
        let view = TwinView::default()
            .with_file(
                "src/fast.rs",
                ("src/lib.rs::imp", "feature=fast", "inactive"),
            )
            .with_file(
                "src/slow.rs",
                ("src/lib.rs::imp", "not(feature=fast)", "active"),
            )
            .with_file("src/unix.rs", ("src/lib.rs::os", "unix", "unknown"))
            .with_file("src/other.rs", ("src/lib.rs::os", "not(unix)", "unknown"));
        let site = "src/lib.rs::caller::call@3:4";
        assert_eq!(of(&view, site, "src/fast.rs::pick"), Verdict::NotCompiled);
        assert_eq!(of(&view, site, "src/slow.rs::pick"), Verdict::Compiled);
        assert_eq!(of(&view, site, "src/unix.rs::h"), Verdict::Undecided);
        let in_unix = "src/unix.rs::run::call@2:4";
        assert_eq!(of(&view, in_unix, "src/unix.rs::h"), Verdict::Compiled);
        assert_eq!(of(&view, in_unix, "src/other.rs::h"), Verdict::NotCompiled);
        assert_eq!(
            view.logical_id("src/unix.rs::h").as_deref(),
            Some("src/lib.rs::os::h")
        );
        assert_eq!(view.logical_id("src/lib.rs::caller"), None);
    }
}
