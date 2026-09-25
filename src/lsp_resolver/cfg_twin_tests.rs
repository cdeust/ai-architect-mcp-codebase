// lsp_resolver::cfg_twin_tests: issue #353. A language server resolves `pick()`
// to the twin its own cfg set compiles, whose id ends in `#cfg(..)`. The name
// check compares the stripped name, or every twin target is rejected as a
// same-line collision.

use super::edges::resolved_target_matches;
use super::sites::{NodePosition, UnresolvedCallSite};

fn site(callee: &str) -> UnresolvedCallSite {
    UnresolvedCallSite {
        id: "src/a.rs::caller::call@3:4".into(),
        caller_qn: "src/a.rs::caller".into(),
        caller_label: "Function".into(),
        callee_name: callee.into(),
        file_path: "src/a.rs".into(),
        line: 3,
        col: 4,
    }
}

fn at(id: &str) -> NodePosition {
    NodePosition {
        id: id.into(),
        label: "Function".into(),
    }
}

#[test]
fn a_twin_id_matches_the_call_that_spells_its_plain_name() {
    let twin = at("src/lib.rs::pick#cfg(not(feature=fast))");
    assert!(resolved_target_matches(&twin, &site("pick")));
    assert!(resolved_target_matches(
        &at("src/lib.rs::m#cfg(unix)::pick"),
        &site("m.pick")
    ));
}

#[test]
fn a_twin_id_of_another_name_is_still_rejected() {
    let twin = at("src/lib.rs::pick#cfg(not(feature=fast))");
    assert!(!resolved_target_matches(&twin, &site("other")));
}
