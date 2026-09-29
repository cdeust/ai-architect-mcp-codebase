// parser::spec::c_family — the shared C-family convention helpers (ADR-0055
// §4; §3.3 rule-of-three extraction, phase 8).
//
// The C family (C phase 6, C++ phase 7, Objective-C phase 8) shares a handful
// of behavioral predicates that reduce to identical code: every member is
// `public` (no access keyword the hand-written walkers honored), a definition
// QN is `{scope}::{name}#{seq}` (the per-file `seq` makes it unique, so no
// dedup is needed), a call site is keyed `{caller}::call@{line}:{col}#{seq}`
// with a `callee_name` property (C and C++ add `callee_shape`, issue #401), and
// a `#include`/`#import` directive is
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
use crate::graph_store::{CALLEE_SHAPE_DIRECT, CALLEE_SHAPE_INDIRECT, CALLEE_SHAPE_MEMBER};
use crate::parser::{node_field_text, node_text, qual};

/// The uniform C-family visibility: `public` for every declared name. C has no
/// access keyword; C++ has `private:`/`protected:` but the hand-written walker
/// ignored them and hardcoded `public`; Objective-C hardcoded `public` too.
/// Preserved for parity across all three.
pub(super) fn public_visibility() -> String {
    "public".to_string()
}

/// The property naming what a callable node stands for (issue #400): `prototype`
/// for a declaration with no body, `macro` for a function-like macro. Absent
/// means a body; the indexer stores that as `body`.
const BODY_KIND: &str = "body_kind";
/// The property marking a callable with internal linkage (issue #400).
const LINKAGE: &str = "linkage";
const LINKAGE_INTERNAL: &str = "internal";

/// `linkage=internal` for a function definition or prototype written with the
/// `static` storage class, which only its own translation unit can name; no
/// property otherwise.
/// source: ISO/IEC 9899:2018 §6.2.2p3 ("If the declaration of a file scope
/// identifier for an object or a function contains the storage-class specifier
/// static, the identifier has internal linkage"); C++ [basic.link]/3.
pub(super) fn linkage_props(source: &str, node: Node) -> Vec<(String, String)> {
    let mut cursor = node.walk();
    let is_static = node
        .children(&mut cursor)
        .any(|c| c.kind() == "storage_class_specifier" && node_text(source, c) == "static");
    if is_static {
        vec![(LINKAGE.to_string(), LINKAGE_INTERNAL.to_string())]
    } else {
        Vec::new()
    }
}

/// Gives `linkage=internal` to every function of a file that shares its name
/// with one written `static` there.
///
/// Linkage belongs to the identifier within the translation unit, not to one
/// declaration: once a file-scope function is declared `static`, a later
/// declaration or definition without a storage class keeps internal linkage,
/// so `static void f(void);` then `void f(void) { … }` defines a file-local
/// `f`. `linkage_props` reads one node, so without this pass the definition
/// looked external and another file's call bound to it. The reverse order
/// (`static` only on a later declaration) is undefined behaviour and rejected
/// by compilers; it is read as file-local too, the reading that can only
/// withhold an edge. Macros have no linkage and are left alone.
/// source: ISO/IEC 9899:2018 §6.2.2p4 (a later declaration takes the linkage
/// of the prior one) and §6.2.2p7 (both linkages in one unit: undefined).
pub(super) fn propagate_internal_linkage(nodes: &mut [crate::parser::ExtractedNode]) {
    let is_function = |n: &crate::parser::ExtractedNode| {
        n.label == crate::parser::LABEL_FUNCTION && !n.properties.iter().any(|(k, _)| k == "macro")
    };
    let internal = |n: &crate::parser::ExtractedNode| {
        n.properties
            .iter()
            .any(|(k, v)| k == LINKAGE && v == LINKAGE_INTERNAL)
    };
    let names: std::collections::HashSet<String> = nodes
        .iter()
        .filter(|n| is_function(n) && internal(n))
        .map(|n| n.name.clone())
        .collect();
    for node in nodes.iter_mut() {
        if is_function(node) && !internal(node) && names.contains(&node.name) {
            node.properties
                .push((LINKAGE.to_string(), LINKAGE_INTERNAL.to_string()));
        }
    }
}

/// The properties of a C-family prototype: `is_prototype`, `body_kind=prototype`
/// and its linkage.
pub(super) fn prototype_props(source: &str, node: Node) -> Vec<(String, String)> {
    let mut props = vec![
        ("is_prototype".to_string(), "true".to_string()),
        (BODY_KIND.to_string(), "prototype".to_string()),
    ];
    props.extend(linkage_props(source, node));
    props
}

/// The properties of a `#define`: `macro=true`, and `body_kind=macro` when the
/// macro takes arguments and is emitted as a `Function`.
pub(super) fn macro_props(label: &str) -> Vec<(String, String)> {
    let mut props = vec![("macro".to_string(), "true".to_string())];
    if label == crate::parser::LABEL_FUNCTION {
        props.push((BODY_KIND.to_string(), "macro".to_string()));
    }
    props
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
/// tail yields `None`; C and C++ reach this only for a name or a member access
/// (`shaped_callee` keeps indirect callees). Splits on `['.', '>', ':']`,
/// matching the hand-written C / C++ `extract_calls`.
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

/// Callee kinds that name a function directly. `primitive_type` is a C++
/// functional cast (`int(x)`), kept as a name as before.
/// source: tree-sitter-c 0.24.2 and tree-sitter-cpp 0.23.4 node-types.json,
/// the `function` field of `call_expression`.
const DIRECT_CALLEE_KINDS: [&str; 5] = [
    "identifier",
    "qualified_identifier",
    "template_function",
    "primitive_type",
    "destructor_name",
];
/// source: same node-types.json; `field_expression` is `s.f` and `p->f`.
const MEMBER_CALLEE_KIND: &str = "field_expression";
/// The longest indirect callee text kept as the site's name; the text only
/// labels the site, nothing resolves it.
/// source: structural, keeps one-line labels for `(*handlers[i].fn)` shapes.
const INDIRECT_CALLEE_MAX_CHARS: usize = 80;

/// Callee kinds that are a pointer-valued expression: what a call through a
/// function pointer, a table of them or a call's result looks like once the
/// parentheses are stripped. `field_expression` only reaches this list
/// parenthesized (`(s->cb)(x)`); bare it is a member call.
/// source: tree-sitter-c 0.24.2 and tree-sitter-cpp 0.23.4 node-types.json,
/// the `function` field of `call_expression` (an `_expression`, so a callee can
/// also be a cast operand, a condition, a string operand of `asm`, ...).
const INDIRECT_CALLEE_KINDS: [&str; 8] = [
    "pointer_expression",
    "subscript_expression",
    "call_expression",
    "conditional_expression",
    "cast_expression",
    "generic_expression",
    "lambda_expression",
    "field_expression",
];

/// The shape of a call's callee (issue #401), read from the kind of its
/// `function` node; `None` when the node is not a call at all.
///
/// tree-sitter-c reads `(T)(x)` with `T` not known as a typedef in the file as
/// a call whose callee is `(T)`, and reads asm operands (`: "i" (x)`) and
/// mis-parsed macro conditions the same way; none of those is a call, so a
/// callee that is a literal, an operator expression or a lone parenthesized
/// name yields `None`, unless the name cannot be a cast: `(fp)()` and
/// `(fp)(a, b)` have no single operand, and `(fp)(x)` is a call when the file
/// declares `fp` as a variable, parameter or function. Only `(name)(x)` with
/// `name` declared elsewhere (a header) stays indistinguishable from a cast
/// without type information; it is dropped, as it was before #401.
/// source: ISO/IEC 9899:2018 §6.5.4 (a cast has exactly one operand),
/// §6.5.2.2 (a call through an expression of pointer-to-function type).
pub(super) fn callee_shape(
    source: &str,
    call_node: Node,
    function_field: &str,
) -> Option<&'static str> {
    let callee = call_node.child_by_field_name(function_field)?;
    if DIRECT_CALLEE_KINDS.contains(&callee.kind()) {
        return Some(CALLEE_SHAPE_DIRECT);
    }
    if callee.kind() == MEMBER_CALLEE_KIND {
        return Some(CALLEE_SHAPE_MEMBER);
    }
    let mut inner = callee;
    while inner.kind() == "parenthesized_expression" {
        inner = inner.named_child(0)?;
    }
    if inner.kind() == "identifier" && inner.id() != callee.id() {
        let one_operand = call_node
            .child_by_field_name("arguments")
            .is_some_and(|args| args.named_child_count() == 1);
        let is_call = !one_operand || is_declared_in_file(source, inner);
        return is_call.then_some(CALLEE_SHAPE_INDIRECT);
    }
    INDIRECT_CALLEE_KINDS
        .contains(&inner.kind())
        .then_some(CALLEE_SHAPE_INDIRECT)
}

/// Whether the file declares the name `name_node` spells: an `identifier` that
/// is the `declarator` of a declaration, a parameter or a function (a typedef
/// name is a `type_identifier`, so a type never matches). A declaration the
/// grammar could not parse cleanly is ignored: a storage macro before it makes
/// the grammar read a typedef name as the declared variable.
fn is_declared_in_file(source: &str, name_node: Node) -> bool {
    let name = node_text(source, name_node);
    let mut root = name_node;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    let mut cursor = root.walk();
    loop {
        let n = cursor.node();
        if n.kind() == "identifier"
            && n.id() != name_node.id()
            && n.parent().is_some_and(|p| {
                !p.has_error()
                    && p.child_by_field_name("declarator")
                        .is_some_and(|d| d.id() == n.id())
            })
            && node_text(source, n) == name
        {
            return true;
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return false;
            }
        }
    }
}

/// The callee of a C or C++ call. A name or a member access yields its last
/// identifier, as `member_access_callee` always did; an indirect callee
/// yields its own text, whitespace collapsed, so the call keeps a site
/// instead of being dropped (issue #401); a callee that is not a call yields
/// `None`.
pub(super) fn shaped_callee(source: &str, call_node: Node, function_field: &str) -> Option<String> {
    if callee_shape(source, call_node, function_field)? != CALLEE_SHAPE_INDIRECT {
        return member_access_callee(source, call_node, function_field);
    }
    let text = node_field_text(source, call_node, function_field);
    let label: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(INDIRECT_CALLEE_MAX_CHARS)
        .collect();
    (!label.is_empty()).then_some(label)
}

/// `call_entry` plus the `callee_shape` property the resolver reads to leave
/// pointer calls open (issue #401). Used by C and C++; Objective-C keeps
/// `call_entry`.
pub(super) fn shaped_call_entry(
    source: &str,
    call_node: Node,
    function_field: &str,
    caller_qn: &str,
    call: (&str, u64),
) -> CallEntry {
    let (callee, seq) = call;
    let mut entry = call_entry(call_node, caller_qn, callee, seq);
    if let Some(shape) = callee_shape(source, call_node, function_field) {
        entry
            .properties
            .push(("callee_shape".to_string(), shape.to_string()));
    }
    entry
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
        properties: vec![
            ("callee_name".to_string(), callee.to_string()),
            // source: LSP 3.17 Base Protocol — positions are 0-based; `col`
            // above is 1-based, so `lsp_col` carries the raw tree-sitter
            // 0-based column separately. Read by
            // indexer::persist::nodes::append_label_properties.
            (
                "lsp_col".to_string(),
                call_node.start_position().column.to_string(),
            ),
        ],
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
