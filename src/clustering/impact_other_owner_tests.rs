// Tests for the open call sites `get_impact` leaves out of its count because
// their spelling names another owner (issue #392). Every case drives the public
// `get_impact` over a real in-memory graph, so the count, the exclusion counter
// and the prose reason are read where a caller reads them.

use super::*;
use crate::clustering::{get_impact, ImpactResult};
use crate::graph_store::{cypher_str, GraphStore, NODE_CALL_SITE};

const TARGET: &str = "src/lib.rs::TaskSet::new";
const EXTERNAL: &str = "external_callee";

fn store() -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix("impact_other_owner")
        .tempdir()
        .expect("create temp dir");
    let store = GraphStore::open_or_create(&dir.path().join("testdb")).expect("open_or_create");
    store.create_schema().expect("create_schema");
    (dir, store)
}

fn insert(store: &GraphStore, label: &str, props: &[(&str, String)]) {
    let owned: Vec<(&str, &str)> = props.iter().map(|(k, v)| (*k, v.as_str())).collect();
    store.insert_node(label, &owned).expect("insert node");
}

fn type_node(store: &GraphStore, label: &str, name: &str) {
    let qn = format!("src/lib.rs::{name}");
    insert(
        store,
        label,
        &[
            ("id", cypher_str(&qn)),
            ("name", cypher_str(name)),
            ("qualified_name", cypher_str(&qn)),
            ("start_line", "1".into()),
            ("end_line", "1".into()),
            ("visibility", cypher_str("pub")),
            ("language", cypher_str("rust")),
        ],
    );
}

fn method(store: &GraphStore, owner: &str, name: &str, trait_name: &str) -> String {
    let qn = format!("src/lib.rs::{owner}::{name}");
    insert(
        store,
        "Method",
        &[
            ("id", cypher_str(&qn)),
            ("name", cypher_str(name)),
            ("qualified_name", cypher_str(&qn)),
            ("start_line", "1".into()),
            ("end_line", "1".into()),
            ("visibility", cypher_str("pub")),
            ("is_async", "false".into()),
            ("receiver_type", cypher_str(&format!("src/lib.rs::{owner}"))),
            ("trait_name", cypher_str(trait_name)),
            ("language", cypher_str("rust")),
        ],
    );
    qn
}

/// How an open call site spells its callee.
struct Open<'a> {
    callee: &'a str,
    reason: &'a str,
    hint: &'a str,
    language: &'a str,
}

fn rust<'a>(callee: &'a str, reason: &'a str, hint: &'a str) -> Open<'a> {
    Open {
        callee,
        reason,
        hint,
        language: "rust",
    }
}

fn site(store: &GraphStore, id: &str, open: Open) {
    insert(
        store,
        NODE_CALL_SITE,
        &[
            ("id", cypher_str(id)),
            ("callee_name", cypher_str(open.callee)),
            ("line", "1".into()),
            ("col", "1".into()),
            ("is_resolved", "false".into()),
            ("language", cypher_str(open.language)),
            ("unresolved_reason", cypher_str(open.reason)),
            ("receiver_hint", cypher_str(open.hint)),
        ],
    );
}

/// `TaskSet` with an inherent `new`, the shape of dy-wcet's target.
fn task_set_store() -> (tempfile::TempDir, GraphStore) {
    let (dir, s) = store();
    type_node(&s, "Struct", "TaskSet");
    method(&s, "TaskSet", "new", "");
    (dir, s)
}

fn impact(s: &GraphStore) -> ImpactResult {
    get_impact(s, TARGET).expect("get_impact")
}

/// The two sites of issue #392 leave the count and are counted apart; a site
/// that spells the target's own type, `Self`, or nothing about its receiver stays.
#[test]
fn a_path_to_another_type_leaves_the_count_and_is_counted_apart() {
    let (_d, s) = task_set_store();
    site(&s, "a", rust("io::BufWriter::new", EXTERNAL, ""));
    site(&s, "b", rust("Vec::new", EXTERNAL, ""));
    site(&s, "c", rust("TaskSet::new", "not_found", ""));
    site(&s, "d", rust("Self::new", "not_found", ""));
    site(&s, "e", rust("x.new", "no_receiver_type", ""));

    let r = impact(&s);

    assert_eq!(r.unresolved_callsites_excluded_other_owner, 2);
    assert_eq!(r.unresolved_callsites_naming_target, 3);
    assert_eq!(r.unresolved_callsites_by_reason.values().sum::<u64>(), 3);
    assert!(
        r.epistemic_reasons
            .iter()
            .any(|m| m.contains("3 unresolved call site(s)") && m.contains("2 more open")),
        "{:?}",
        r.epistemic_reasons
    );
}

/// When every open site names another type, nothing could be the target: the
/// answer is no longer a lower bound for that reason.
#[test]
fn a_target_whose_only_open_sites_name_another_type_reports_no_carrier() {
    let (_d, s) = task_set_store();
    site(&s, "a", rust("Vec::new", EXTERNAL, ""));

    let r = impact(&s);

    assert_eq!(r.unresolved_callsites_naming_target, 0);
    assert_eq!(r.unresolved_callsites_excluded_other_owner, 1);
    assert_eq!(r.epistemic, crate::epistemic::Boundary::Exact);
}

/// A receiver hint of another type excludes; an unknown receiver, the owner's
/// own type, a borrowed owner, a smart pointer (it dereferences) and a type of
/// the repository that implements `Deref` all stay.
#[test]
fn a_receiver_hint_of_another_type_excludes_only_when_it_cannot_reach_the_owner() {
    let (_d, s) = task_set_store();
    type_node(&s, "Struct", "Other");
    type_node(&s, "Struct", "Wrapper");
    method(&s, "Wrapper", "deref", "Deref");
    site(&s, "vec", rust("v.new", "no_receiver_type", "Vec"));
    site(&s, "other", rust("o.new", "not_found", "Other"));
    site(&s, "unknown", rust("u.new", "no_receiver_type", ""));
    site(&s, "owner", rust("t.new", "not_found", "TaskSet"));
    site(&s, "borrowed", rust("b.new", "not_found", "&mut TaskSet"));
    site(&s, "boxed", rust("x.new", "external_callee", "Box"));
    site(&s, "wrapper", rust("w.new", "not_found", "Wrapper"));
    site(&s, "dyn", rust("d.new", "not_found", "dyn Tr"));

    let r = impact(&s);

    assert_eq!(r.unresolved_callsites_excluded_other_owner, 2, "Vec, Other");
    assert_eq!(r.unresolved_callsites_naming_target, 6);
}

/// A trait impl is reached through the trait's own path, so `Default::default`
/// and `fmt::Display::fmt` stay for the impl; a closed std type or a type of the
/// repository still excludes.
#[test]
fn a_trait_impl_keeps_the_paths_that_name_its_trait() {
    let (_d, s) = store();
    type_node(&s, "Struct", "TaskSet");
    type_node(&s, "Struct", "Other");
    let target = method(&s, "TaskSet", "default", "Default");
    site(&s, "t", rust("Default::default", EXTERNAL, ""));
    site(&s, "vec", rust("Vec::default", EXTERNAL, ""));
    site(&s, "u64", rust("u64::default", EXTERNAL, ""));
    site(&s, "other", rust("Other::default", "not_found", ""));
    site(&s, "unknown_trait", rust("Zeroable::default", EXTERNAL, ""));

    let r = get_impact(&s, &target).expect("get_impact");

    assert_eq!(r.unresolved_callsites_excluded_other_owner, 3);
    assert_eq!(r.unresolved_callsites_naming_target, 2, "Default, Zeroable");
}

/// A name a `type` alias or a `use .. as` gives to another type is not read as
/// that type: `Set::new` may be `TaskSet::new` under an alias.
#[test]
fn an_alias_of_the_owner_is_kept() {
    let (_d, s) = task_set_store();
    type_node(&s, "Struct", "Set");
    insert(
        &s,
        "Import",
        &[
            ("id", cypher_str("src/lib.rs::TS")),
            ("path", cypher_str("crate::TaskSet")),
            ("alias", cypher_str("Set")),
            ("is_glob", "false".into()),
            ("start_line", "1".into()),
            ("end_line", "1".into()),
            ("is_resolved", "true".into()),
            ("language", cypher_str("rust")),
        ],
    );
    site(&s, "a", rust("Set::new", "not_found", ""));
    site(&s, "b", rust("Vec::new", EXTERNAL, ""));

    let r = impact(&s);

    assert_eq!(r.unresolved_callsites_naming_target, 1);
    assert_eq!(r.unresolved_callsites_excluded_other_owner, 1);
}

/// C and C++ callees are recorded as their last segment and a receiver of another
/// type may inherit the method: no site is dropped for a non-Rust language, nor
/// for a target that is not a method of a repository type.
#[test]
fn other_languages_and_other_targets_keep_every_site() {
    let (_d, s) = task_set_store();
    site(
        &s,
        "cpp",
        Open {
            callee: "new",
            reason: "no_receiver_type",
            hint: "Base",
            language: "cpp",
        },
    );
    site(
        &s,
        "cpp2",
        Open {
            callee: "Vec::new",
            reason: EXTERNAL,
            hint: "Base",
            language: "cpp",
        },
    );
    let r = impact(&s);
    assert_eq!(r.unresolved_callsites_naming_target, 2);
    assert_eq!(r.unresolved_callsites_excluded_other_owner, 0);

    // A free function `new`: the spelling of a path does not decide its owner.
    let (_d2, s2) = store();
    let f = "src/lib.rs::new";
    insert(
        &s2,
        "Function",
        &[
            ("id", cypher_str(f)),
            ("name", cypher_str("new")),
            ("qualified_name", cypher_str(f)),
            ("start_line", "1".into()),
            ("end_line", "1".into()),
            ("visibility", cypher_str("pub")),
            ("is_async", "false".into()),
            ("language", cypher_str("rust")),
        ],
    );
    site(&s2, "a", rust("Vec::new", EXTERNAL, ""));
    let r2 = get_impact(&s2, f).expect("get_impact");
    assert_eq!(r2.unresolved_callsites_naming_target, 1);
    assert_eq!(r2.unresolved_callsites_excluded_other_owner, 0);
}

#[test]
fn a_hint_reads_as_its_plain_type_name_or_not_at_all() {
    assert_eq!(hint_type_name("Vec"), Some("Vec"));
    assert_eq!(hint_type_name("&mut b::Set<T>"), Some("Set"));
    assert_eq!(hint_type_name("& TaskSet"), Some("TaskSet"));
    for unreadable in ["", "dyn Tr", "[u8; 4]", "(A, B)", "impl Tr"] {
        assert_eq!(hint_type_name(unreadable), None, "{unreadable:?}");
    }
}
