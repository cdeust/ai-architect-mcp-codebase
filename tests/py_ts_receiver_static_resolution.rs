// py_ts_receiver_static_resolution — regression for issue #290: Python
// `self.m()` / TypeScript `this.m()` inside a class method had zero static
// callers (the #283 defect outside the Rust-only fix). Real indexer +
// resolver, no LSP. source: issue #290; tests/rust_receiver_static_resolution.rs.

use ai_architect_mcp::clustering;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;
mod common;
use common::TempDirExt;

/// source: src/ambiguity_policy.rs (`Evidence::ReceiverBound => 0.93`).
const RECEIVER_BOUND_CONFIDENCE: f64 = 0.93;

/// Writes `files` under a fresh tempdir's `src/`, indexes and resolves it.
fn index_and_resolve(
    tag: &str,
    files: &[(&str, &str)],
) -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    let root = tempfile::Builder::new()
        .prefix(&format!("py_ts_receiver_{tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = root.join("src");
    for (rel, body) in files {
        let p = src.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create fixture dir");
        }
        fs::write(&p, body).expect("write fixture file");
    }
    let graph_dir = root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    let res = resolver::resolve_graph(&store).expect("resolve_graph");
    (store, res, root)
}

/// Every `Calls_Method_Method` edge out of `caller_qn`, as
/// (target_qn, confidence, resolution_method).
fn calls_from(store: &GraphStore, caller_qn: &str) -> Vec<Vec<String>> {
    store
        .execute_query(&format!(
            "MATCH (a:Method)-[r:Calls_Method_Method]->(b:Method) \
             WHERE a.qualified_name = '{caller_qn}' \
             RETURN b.qualified_name, r.confidence, r.resolution_method"
        ))
        .expect("query Calls_Method_Method")
        .rows
}

/// The unresolved `Calls` reason for the one CallSite spelled `callee`.
fn unresolved_reason(res: &resolver::ResolutionResult, callee: &str) -> Option<String> {
    res.unresolved
        .iter()
        .find(|u| u.kind == "Calls" && u.target_text == callee)
        .map(|u| u.reason.clone())
}

/// `caller` has exactly one receiver-bound edge, into `target`, and
/// `get_impact(target)` reports it (the issue's measurement; pre-fix it
/// printed `static callers=0`).
fn assert_receiver_bound(
    store: &GraphStore,
    res: &resolver::ResolutionResult,
    caller: &str,
    target: &str,
) {
    let edges = calls_from(store, caller);
    let impact = clustering::get_impact(store, target).expect("get_impact");
    let callers: Vec<&str> = impact
        .callers
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();
    eprintln!(
        "[#290 measurement] {target}: static callers={} {callers:?}, \
         unresolved callsites naming target={}, epistemic={:?} {:?}",
        callers.len(),
        impact.unresolved_callsites_naming_target,
        impact.epistemic,
        impact.epistemic_reasons
    );
    assert_eq!(
        edges.len(),
        1,
        "{caller} must bind to exactly one method; edges={edges:?} unresolved={:?}",
        res.unresolved
            .iter()
            .map(|u| (&u.target_text, &u.reason))
            .collect::<Vec<_>>()
    );
    assert_eq!(edges[0][0], target);
    let confidence: f64 = edges[0][1].parse().unwrap_or(0.0);
    assert!(
        (confidence - RECEIVER_BOUND_CONFIDENCE).abs() < 1e-9,
        "got {confidence}"
    );
    assert_eq!(edges[0][2], "receiver-type");
    assert_eq!(callers, vec![caller], "get_impact must report the caller");
    assert_eq!(impact.unresolved_callsites_naming_target, 0);
}

const PY_SAME_CLASS: &str = "class TaskSet:\n\
    \x20\x20\x20\x20def response_of(self, i):\n\
    \x20\x20\x20\x20\x20\x20\x20\x20return i\n\
    \n\
    \x20\x20\x20\x20def total(self):\n\
    \x20\x20\x20\x20\x20\x20\x20\x20return self.response_of(0)\n";

const TS_SAME_CLASS: &str = "export class TaskSet {\n\
    \x20\x20responseOf(i: number): number { return i; }\n\
    \x20\x20total(): number { return this.responseOf(0); }\n\
    }\n";

#[test]
fn python_self_receiver_call_has_zero_static_callers() {
    let (store, res, _root) = index_and_resolve("py_bound", &[("task_set.py", PY_SAME_CLASS)]);
    assert_receiver_bound(
        &store,
        &res,
        "task_set.py::TaskSet::total",
        "task_set.py::TaskSet::response_of",
    );
}

#[test]
fn typescript_this_receiver_call_has_zero_static_callers() {
    let (store, res, _root) = index_and_resolve("ts_bound", &[("task_set.ts", TS_SAME_CLASS)]);
    assert_receiver_bound(
        &store,
        &res,
        "task_set.ts::TaskSet::total",
        "task_set.ts::TaskSet::responseOf",
    );
}

#[test]
fn receiver_call_to_missing_method_never_fabricates_an_edge() {
    let py = "class TaskSet:\n\
              \x20\x20\x20\x20def total(self):\n\
              \x20\x20\x20\x20\x20\x20\x20\x20return self.missing()\n";
    let ts = "export class TaskSet {\n\
              \x20\x20total(): number { return this.missing(); }\n\
              }\n";
    let (store, res, _root) =
        index_and_resolve("missing", &[("task_set.py", py), ("task_set.ts", ts)]);
    for (caller, callee) in [
        ("task_set.py::TaskSet::total", "self.missing"),
        ("task_set.ts::TaskSet::total", "this.missing"),
    ] {
        assert!(calls_from(&store, caller).is_empty(), "{caller}");
        assert_eq!(
            unresolved_reason(&res, callee).as_deref(),
            Some("no target found"),
            "{callee}"
        );
    }
}

/// `helper` exists only as a MODULE function; binding `self.helper()` to it
/// would be the name-only guess the #283 arbitration rejected.
#[test]
fn self_receiver_never_binds_a_same_named_module_function() {
    let py = "def helper():\n\
              \x20\x20\x20\x20return 1\n\
              \n\
              class TaskSet:\n\
              \x20\x20\x20\x20def total(self):\n\
              \x20\x20\x20\x20\x20\x20\x20\x20return self.helper()\n";
    let ts = "export function helper(): number { return 1; }\n\
              export class TaskSet {\n\
              \x20\x20total(): number { return this.helper(); }\n\
              }\n";
    let (store, res, _root) =
        index_and_resolve("module_fn", &[("task_set.py", py), ("task_set.ts", ts)]);
    let into_helper = store
        .execute_query("MATCH (a:Method)-[r]->(b:Function) WHERE b.name = 'helper' RETURN count(*)")
        .expect("query edges into helper");
    assert_eq!(into_helper.rows[0][0], "0");
    for callee in ["self.helper", "this.helper"] {
        assert_eq!(
            unresolved_reason(&res, callee).as_deref(),
            Some("no target found"),
            "{callee}"
        );
    }
}

/// Homonymous classes: the caller's own file binds (palier 1); a third file
/// whose class lacks `m` stays `Ambiguous` (palier 2), never guessed.
#[test]
fn homonymous_classes_bind_own_file_and_stay_ambiguous_elsewhere() {
    let with_m = |v: u8| {
        format!(
            "class TaskSet:\n\
             \x20\x20\x20\x20def m(self):\n\
             \x20\x20\x20\x20\x20\x20\x20\x20return {v}\n\
             \n\
             \x20\x20\x20\x20def caller(self):\n\
             \x20\x20\x20\x20\x20\x20\x20\x20return self.m()\n"
        )
    };
    let without_m = "class TaskSet:\n\
                     \x20\x20\x20\x20def caller(self):\n\
                     \x20\x20\x20\x20\x20\x20\x20\x20return self.m()\n";
    let (a, b) = (with_m(1), with_m(2));
    let (store, res, _root) = index_and_resolve(
        "homonyms",
        &[
            ("a.py", a.as_str()),
            ("b.py", b.as_str()),
            ("c.py", without_m),
        ],
    );
    let own = calls_from(&store, "a.py::TaskSet::caller");
    assert_eq!(own.len(), 1, "{own:?}");
    assert_eq!(own[0][0], "a.py::TaskSet::m");

    assert!(calls_from(&store, "c.py::TaskSet::caller").is_empty());
    let reason = res
        .unresolved
        .iter()
        .find(|u| u.from_id.starts_with("c.py::TaskSet::caller") && u.target_text == "self.m")
        .map(|u| u.reason.clone());
    assert!(
        reason
            .as_deref()
            .is_some_and(|r| r.starts_with("ambiguous (")),
        "c.py's caller must stay ambiguous, got {reason:?}"
    );
}

/// A field-chained receiver is not the class: unchanged, unresolved.
#[test]
fn field_chained_receivers_stay_not_found() {
    let py = "class TaskSet:\n\
              \x20\x20\x20\x20def get(self):\n\
              \x20\x20\x20\x20\x20\x20\x20\x20return 0\n\
              \n\
              \x20\x20\x20\x20def lookup(self):\n\
              \x20\x20\x20\x20\x20\x20\x20\x20return self.tasks.get()\n";
    let ts = "export class Button {\n\
              \x20\x20onClick(): void {}\n\
              \x20\x20fire(): void { this.props.onClick(); }\n\
              }\n";
    let (store, res, _root) =
        index_and_resolve("chained", &[("task_set.py", py), ("button.ts", ts)]);
    assert!(calls_from(&store, "task_set.py::TaskSet::lookup").is_empty());
    assert!(calls_from(&store, "button.ts::Button::fire").is_empty());
    for callee in ["self.tasks.get", "this.props.onClick"] {
        assert_eq!(
            unresolved_reason(&res, callee).as_deref(),
            Some("no target found"),
            "{callee}"
        );
    }
}
