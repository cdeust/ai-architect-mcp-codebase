// parser::spec::c_family — the shared C-family convention helpers (ADR-0055
// §4; §3.3 rule-of-three extraction, phase 8).
//
// The C family (C phase 6, C++ phase 7, Objective-C phase 8) shares a handful
// of behavioral predicates that reduce to identical code: every member is
// `public` (no access keyword the hand-written walkers honored), a definition
// QN is `{scope}::{name}#{seq}` (the per-file `seq` makes it unique, so no
// dedup is needed), a call site is keyed `{caller}::call@{line}:{col}#{seq}`
// with a single `callee_name` property, and a `#include`/`#import` directive is
// shaped into one `Import` by stripping the directive and the `<>`/`""`
// delimiters. These were duplicated VERBATIM in `CConventions` (c.rs) and
// `CppConventions` (cpp.rs); Objective-C is the third use, which crosses §3.3
// ("three concrete uses before extracting"), so the shared parts move here.
//
// This module is behavior-preserving for C and C++ — their parity suites
// (`c_parity_tests`, `cpp_parity_tests`) prove each helper produces byte-for-
// byte the same records the inline code did. What does NOT move: C++'s `using`
// import branch and Objective-C's `@import`/message-send handling are genuinely
// language-specific and stay in their own conventions (ADR-0055 §4 — the escape
// hatch is per-language behavior, not shared structure).
//
// source: tree-sitter-c / tree-sitter-cpp / tree-sitter-objc node-types.json
// (the include directive and call-expression shapes these helpers read).

use tree_sitter::Node;

use super::conventions::{CallEntry, ImportEntry};
use crate::parser::{node_field_text, node_text, qual};

/// The uniform C-family visibility: `public` for every declared name. C has no
/// access keyword; C++ has `private:`/`protected:` but the hand-written walker
/// ignored them and hardcoded `public`; Objective-C hardcoded `public` too.
/// Preserved for parity across all three.
pub(super) fn public_visibility() -> String {
    "public".to_string()
}

/// The C-family definition QN: `{scope}::{name}#{seq}`. The per-file `seq`
/// counter makes every function/method/prototype QN unique, so the walker's
/// collision dedup is never needed. Shared verbatim by C, C++, and Objective-C.
pub(super) fn def_qn(scope: &str, name: &str, seq: u64) -> String {
    format!("{scope}::{name}#{seq}")
}

/// The last identifier segment of a call expression's callee, after member and
/// scope access: `printf` → `printf`, `obj.method` → `method`,
/// `ptr->call` → `call`, `geometry::identity` → `identity`. A non-identifier
/// callee (`(fp)()`) yields `None` (the call is dropped). Splits on
/// `['.', '>', ':']`, matching the hand-written C / C++ `extract_calls`.
///
/// `function_field` is the grammar field naming the callee (`function` in both
/// grammars). Objective-C does NOT use this helper for `call_expression` (it
/// splits on `.` only) — that divergence is why the callee helper is opt-in per
/// language, not a shared default.
pub(super) fn member_access_callee(
    source: &str,
    call_node: Node,
    function_field: &str,
) -> Option<String> {
    let callee = node_field_text(source, call_node, function_field);
    let tail = callee
        .rsplit(['.', '>', ':'])
        .next()
        .unwrap_or("")
        .trim_end_matches('(')
        .trim()
        .to_string();
    identifier_callee(tail)
}

/// Accepts a callee string iff it is non-empty and begins with an identifier
/// character (`[A-Za-z_]`), else `None`. The single decision point both the
/// member-access split (C/C++) and Objective-C's `.`-split call callee funnel
/// through, so the "drop a non-identifier callee" rule lives in one place.
pub(super) fn identifier_callee(tail: String) -> Option<String> {
    if !tail.is_empty()
        && tail
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
    {
        Some(tail)
    } else {
        None
    }
}

/// Shapes a call node whose callee is already accepted into the shared
/// C-family `CallEntry`: a `CallSite` named `callee`, keyed
/// `{caller}::call@{line}:{col}#{seq}`, `public`, with one `callee_name`
/// property and a `Calls` edge to `callee`. Identical across C, C++, and
/// Objective-C.
pub(super) fn call_entry(call_node: Node, caller_qn: &str, callee: &str, seq: u64) -> CallEntry {
    let line = call_node.start_position().row + 1;
    let col = call_node.start_position().column + 1;
    CallEntry {
        name: callee.to_string(),
        qualified_name: format!("{caller_qn}::call@{line}:{col}#{seq}"),
        visibility: public_visibility(),
        properties: vec![("callee_name".to_string(), callee.to_string())],
        start_line: call_node.start_position().row as u64 + 1,
        end_line: call_node.end_position().row as u64 + 1,
        ref_kind: "Calls",
        ref_to: callee.to_string(),
    }
}

/// One `#include` / `#import` directive → one `Import`. Strips the directive
/// keyword (`directives`, tried in order) and the `<>`/`""` delimiters; the
/// display name is the path's last `/`-segment, the QN is
/// `{scope}::include:{path}` (prefix supplied by the caller), and the edge
/// target is the full cleaned path. Reproduces the hand-written C / C++
/// `extract_include`.
///
/// `qn_prefix` is the QN discriminator each language uses (`include:` for
/// C/C++, `import:` for Objective-C's `#import`), so the shared shaping does not
/// force a single QN scheme on the family.
pub(super) fn include_entry(
    source: &str,
    node: Node,
    scope: &str,
    directives: &[&str],
    qn_prefix: &str,
) -> Vec<ImportEntry> {
    let mut text = node_text(source, node);
    text = text.trim().to_string();
    for d in directives {
        text = text.trim_start_matches(d).to_string();
    }
    let cleaned = text
        .trim()
        .trim_matches('<')
        .trim_matches('>')
        .trim_matches('"')
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return Vec::new();
    }
    let display_name = cleaned.rsplit('/').next().unwrap_or(&cleaned).to_string();
    vec![ImportEntry {
        display_name,
        qualified_name: qual(scope, &format!("{qn_prefix}{cleaned}")),
        ref_to: cleaned.clone(),
        properties: vec![("path".to_string(), cleaned)],
        visibility: public_visibility(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
    }]
}
