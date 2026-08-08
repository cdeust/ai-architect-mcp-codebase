// parser::spec::structural_fallback — the TIER 2/3 generic extensions to
// `structural.rs`'s field-shape engine (issue #224 follow-up: descent for
// nested names, a heritage-on-child-node hop, and a fieldless-grammar
// fallback classifier). Every rule here is verified against real grammar
// output (`cargo test parser::spec::diag_scratch` during development, since
// deleted) on C, C++, ObjC, Swift, kotlin-ng, TypeScript — never guessed
// from documentation. Same zero-per-language-artifact discipline as
// `structural.rs`: every candidate list below is ONE small GLOBAL table,
// checked identically for every grammar, extended only when a real grammar
// in this crate's dependency tree proved an entry necessary.
//
// ## Why a separate module
// `structural.rs` already carries the TIER 1 (pure field-shape) engine at
// close to the 500-line file cap (coding-standards.md §4.1). The fallback
// tiers below are a genuinely separate concern (SRP, §1.1): TIER 1 asks "do
// this node's OWN fields look like a definition/call?"; this module asks
// "when TIER 1 finds nothing, does the SURROUNDING structure (nested
// declarator wrappers, positional non-field children, the node's own kind
// name) still say yes?" Different question, different verification method
// (kind-string tables instead of field tables), same zero-artifact rule.
//
// ## The three mechanisms (mapped to the coverage table's four-capability
// verdict)
//
// 1. **Declarator-chain name descent** (`resolve_name_via_declarator_chain`)
//    — fixes C and C++ function/method names. Verified: `function_definition`
//    has a `declarator` field pointing to `function_declarator`, which has
//    NO `name` field of its OWN but has ANOTHER `declarator` field pointing
//    to the actual `identifier` (free function) or `field_identifier` (C++
//    class method) leaf. One global candidate field name (`"declarator"`),
//    chased recursively until a node with neither `name` nor `declarator`
//    is reached — that terminal node IS the name, regardless of its own
//    kind (no kind filtering needed here: the FIELD NAME `declarator` is
//    exactly the wrapper convention these two grammars use, so trusting it
//    structurally carries no cross-node ambiguity the way a bare kind-string
//    scan would).
// 2. **Bounded identifier-leaf descent** (`resolve_name_via_bounded_scan`) —
//    fixes ObjC, whose `class_interface`/`class_implementation` have no
//    `name` field AND no `declarator` field: the class name is a plain,
//    UNNAMED-FIELD (positional) `identifier` child, appearing before the
//    `superclass` field in document order. Pre-order DFS over ALL named
//    children (skipping the `body` field, since a body's own identifiers are
//    never the definition's name), matching the FIRST leaf whose kind is
//    EXACTLY `"identifier"` — deliberately not a substring match, and
//    deliberately excluding `type_identifier`/`field_identifier`, so this
//    tier can never accidentally consume a C++ return-type identifier that
//    happens to precede the declarator in document order (verified risk,
//    not hypothetical — see the module test `descent_does_not_confuse_a_class_return_type_with_the_function_name`).
// 3. **Kind-substring fallback classifier** (`kind_hints_definition_role`,
//    `has_parameter_like_child`, `kind_hints_positional_call`,
//    `first_named_child`) — fixes Swift and kotlin-ng,
//    whose grammars are close to fully positional: `function_declaration`/
//    `class_declaration` share the SAME field shape (`name` [+ Swift's
//    grammar quirk: a SECOND, later `name` field on the return-type node —
//    `child_by_field_name` returns the first, i.e. the correct one] +
//    `body` as an UNNAMED child, not a field, in kotlin-ng's case), and
//    `call_expression` carries ZERO fields in both grammars. This tier reads
//    the definition node's OWN kind string against a 2-pair GLOBAL
//    vocabulary (`"function"`/`"method"` vs `"class"`/`"struct"`/
//    `"interface"`) — verified: both Swift and kotlin-ng literally use the
//    kind names `function_declaration`/`class_declaration`. The SAME
//    vocabulary gates whether the other fallbacks in this module are even
//    attempted, so a node that doesn't look, by its own grammar-chosen kind
//    name, like a definition never enters the (more expensive, more
//    speculative) descent machinery above.
//
// ## What this module does NOT attempt
// Rust's `impl_item` (no `name` field, and its kind contains neither
// "function"/"method" nor "class"/"struct"/"interface" — it is a distinct
// top-level construct, not a definition-with-a-name in the sense this whole
// engine models) stays unreached, honestly, exactly as documented in
// `structural.rs`'s original module doc and pinned by
// `structural_coverage_tests::rust_finds_top_level_functions_but_not_impl_block_methods`.

use tree_sitter::Node;

/// TIER 1.5 name-fallback #1: chase a chain of fields literally named
/// `"declarator"` until a node with neither `name` nor `declarator` is
/// reached. See the module doc's mechanism 1.
///
/// Precondition: `node` already failed the direct `name`-field check.
/// Postcondition: returns the terminal wrapped node (the name leaf) or
/// `None` if no `declarator` field chain exists at all.
pub(super) fn resolve_name_via_declarator_chain(node: Node) -> Option<Node> {
    resolve_declarator_chain_bounded(node, 8)
}

fn resolve_declarator_chain_bounded(node: Node, depth: u8) -> Option<Node> {
    if depth == 0 {
        return None;
    }
    if let Some(named) = node.child_by_field_name("name") {
        return Some(named);
    }
    let wrapped = node.child_by_field_name("declarator")?;
    if let Some(deeper) = resolve_declarator_chain_bounded(wrapped, depth - 1) {
        return Some(deeper);
    }
    Some(wrapped)
}

/// TIER 1.5 name-fallback #2: bounded pre-order scan over named children for
/// the first EXACT-kind `"identifier"` leaf, skipping the `body` field
/// subtree. See the module doc's mechanism 2.
pub(super) fn resolve_name_via_bounded_scan(node: Node) -> Option<Node> {
    let skip_id = node.child_by_field_name("body").map(|b| b.id());
    scan_for_identifier_leaf(node, 6, skip_id)
}

fn scan_for_identifier_leaf(node: Node, depth: u8, skip_id: Option<usize>) -> Option<Node> {
    if depth == 0 {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if Some(child.id()) == skip_id {
            continue;
        }
        if child.kind() == "identifier" {
            return Some(child);
        }
        if let Some(found) = scan_for_identifier_leaf(child, depth - 1, skip_id) {
            return Some(found);
        }
    }
    None
}

/// The one GLOBAL kind-substring vocabulary this module adds, verified
/// necessary by Swift and kotlin-ng's `function_declaration`/
/// `class_declaration` kind names (module doc mechanism 3). Not a
/// per-language table: every grammar is checked against the SAME two lists.
const FUNCTION_KIND_SUBSTRINGS: [&str; 2] = ["function", "method"];
const TYPE_KIND_SUBSTRINGS: [&str; 3] = ["class", "struct", "interface"];

/// A SECOND, independently-necessary GLOBAL vocabulary: the SUFFIX a
/// definition-site node's own kind ends with, verified across all four
/// target kinds (`function_declaration`/`class_declaration` — Swift,
/// kotlin-ng; `function_definition` — C, C++; `class_interface`/
/// `class_implementation` — ObjC). Required as a second condition (not just
/// a bare substring match) because `FUNCTION_KIND_SUBSTRINGS`/
/// `TYPE_KIND_SUBSTRINGS` alone false-positive on WRAPPER kinds that
/// legitimately contain the same words — verified directly:
/// `function_value_parameters` (kotlin-ng's parameter-list wrapper)
/// contains `"function"`; `class_body`/`class_heritage`
/// (kotlin-ng/TypeScript) contain `"class"`. None of those end with a
/// definition-site suffix, so requiring both conditions excludes them
/// without adding a single per-language exception — the same two rules
/// apply identically to every grammar.
const DEFINITION_SITE_KIND_SUFFIXES: [&str; 4] =
    ["declaration", "definition", "interface", "implementation"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KindRole {
    FunctionLike,
    TypeLike,
}

/// Whether `kind` itself, by its grammar-chosen name, looks like a
/// definition SITE (not a supporting wrapper node) — the gate that bounds
/// every fallback in this module to nodes that already look
/// definition-shaped, never to arbitrary expression, statement, or wrapper
/// nodes. Both the keyword substring AND the definition-site suffix must
/// match; see the two vocabularies' doc comments for the false-positive
/// this jointly rules out (`class_body`/`function_value_parameters`/
/// `class_heritage` all contain a keyword but none end with a
/// definition-site suffix).
pub(super) fn kind_hints_definition_role(kind: &str) -> Option<KindRole> {
    if !DEFINITION_SITE_KIND_SUFFIXES
        .iter()
        .any(|s| kind.ends_with(s))
    {
        return None;
    }
    if FUNCTION_KIND_SUBSTRINGS.iter().any(|s| kind.contains(s)) {
        return Some(KindRole::FunctionLike);
    }
    if TYPE_KIND_SUBSTRINGS.iter().any(|s| kind.contains(s)) {
        return Some(KindRole::TypeLike);
    }
    None
}

/// TIER 2 positional body/parameter detection: kotlin-ng exposes
/// `function_body`/`class_body` and `function_value_parameters` as UNNAMED
/// (positional) children rather than `body`/`parameters` fields. This scans
/// direct children (any) for a kind CONTAINING the given substring — the
/// generalization of a field-name check to a kind-name check, still one
/// global rule per capability, not per language.
fn has_child_kind_containing(node: Node, substring: &str) -> bool {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| c.is_named() && c.kind().contains(substring));
    found
}

pub(super) fn has_parameter_like_child(node: Node) -> bool {
    has_child_kind_containing(node, "parameter")
}

/// TIER 2 heritage-on-child-node hop (fixes TypeScript and C++, see
/// `structural.rs`'s original module doc "What this engine does NOT reach").
/// TypeScript's `class_heritage` and C++'s `base_class_clause` are UNNAMED
/// children of the class node, wrapping the actual base-type reference one
/// or two levels deeper. This is a SEPARATE, verified two-entry GLOBAL
/// substring table from the field-based `HERITAGE_FIELD_CANDIDATES` in
/// `structural.rs` — both are attempted for every TypeLike definition, since
/// a grammar may use either convention (or neither).
const HERITAGE_CHILD_KIND_SUBSTRINGS: [&str; 2] = ["heritage", "base_class"];

/// Returns the text of every EXACT-kind-suffix `*identifier` leaf found
/// inside a heritage-shaped child of `node`, one entry per leaf (supporting
/// `implements A, B`-shaped multi-target clauses). Suffix (not exact) match
/// here — unlike the name-descent tier — because this only runs INSIDE an
/// already-confirmed heritage subtree (gated by
/// `HERITAGE_CHILD_KIND_SUBSTRINGS` first), where there is no sibling
/// return-type field to collide with; `type_identifier` (TypeScript's
/// `identifier` field on `extends_clause` is plain `identifier`, but C++'s
/// `base_class_clause` target is `type_identifier`) must both be reachable.
pub(super) fn heritage_targets_via_child_hop(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    let mut targets = Vec::new();
    for child in node.children(&mut cursor) {
        if child.is_named()
            && HERITAGE_CHILD_KIND_SUBSTRINGS
                .iter()
                .any(|s| child.kind().contains(s))
        {
            collect_identifier_suffix_leaves(child, 6, &mut targets);
        }
    }
    targets
}

fn collect_identifier_suffix_leaves<'t>(node: Node<'t>, depth: u8, out: &mut Vec<Node<'t>>) {
    if depth == 0 {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind().ends_with("identifier") {
            out.push(child);
        } else {
            collect_identifier_suffix_leaves(child, depth - 1, out);
        }
    }
}

/// TIER 2 positional call fallback (fixes Swift and kotlin-ng's
/// `call_expression`, which carries zero fields in both grammars, verified).
/// Gated by `kind` containing `"call"` AND ending with `"expression"` (both
/// conditions required — verified necessary, not speculative: Swift wraps
/// its argument list in an intermediate `call_suffix` node, a CHILD of
/// `call_expression`, which also contains the substring `"call"`; without
/// the suffix condition this fallback double-fires on `call_suffix` too,
/// emitting a phantom call site whose "callee" is the empty argument list
/// text `"()"`). `"call_expression"` ends with `"expression"` in both
/// verified grammars; `"call_suffix"` does not, so the suffix condition
/// excludes it with no per-language exception. The callee is the FIRST
/// named child in document order — verified true for both grammars: the
/// callee expression always precedes the argument list.
const CALL_KIND_SUBSTRING: &str = "call";
const CALL_KIND_REQUIRED_SUFFIX: &str = "expression";

pub(super) fn kind_hints_positional_call(kind: &str) -> bool {
    kind.contains(CALL_KIND_SUBSTRING) && kind.ends_with(CALL_KIND_REQUIRED_SUFFIX)
}

pub(super) fn first_named_child(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    let first = node.named_children(&mut cursor).next();
    first
}

/// TIER 2 visibility (item 4): a definition's direct children scanned for a
/// kind CONTAINING `"modifier"` — verified against Java's `modifiers` child
/// (on `method_declaration`, confirmed absent from that node's FIELD list
/// despite the grammar defining the kind) and matching TypeScript's
/// documented `accessibility_modifier` by the same substring. Returns the
/// raw source text of the first such child (e.g. `"public"`,
/// `"public static"`), or empty when none is found.
pub(super) fn visibility_via_modifier_child(source: &str, node: Node) -> String {
    let mut cursor = node.walk();
    let text = node
        .children(&mut cursor)
        .find(|c| c.is_named() && c.kind().contains("modifier"))
        .map(|c| source[c.byte_range()].trim().to_string())
        .unwrap_or_default();
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lang: tree_sitter::Language, src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        parser.parse(src, None).unwrap()
    }

    #[test]
    fn declarator_chain_finds_c_function_name() {
        let src = "int helper() {\n    return 1;\n}\n";
        let tree = parse(tree_sitter_c::LANGUAGE.into(), src);
        let func = tree.root_node().named_child(0).unwrap();
        assert_eq!(func.kind(), "function_definition");
        let name = resolve_name_via_declarator_chain(func).expect("name found");
        assert_eq!(&src[name.byte_range()], "helper");
    }

    #[test]
    fn declarator_chain_finds_cpp_method_name() {
        let src = "class Greeter {\npublic:\n    int greet() { return 1; }\n};\n";
        let tree = parse(tree_sitter_cpp::LANGUAGE.into(), src);
        let class_node = tree.root_node().named_child(0).unwrap();
        let body = class_node.child_by_field_name("body").unwrap();
        let mut cursor = body.walk();
        let method = body
            .named_children(&mut cursor)
            .find(|n| n.kind() == "function_definition")
            .expect("method node");
        let name = resolve_name_via_declarator_chain(method).expect("name found");
        assert_eq!(&src[name.byte_range()], "greet");
    }

    #[test]
    fn bounded_scan_finds_objc_class_name_before_superclass() {
        let src = "@interface Greeter : NSObject\n@end\n";
        let tree = parse(tree_sitter_objc::LANGUAGE.into(), src);
        let iface = tree.root_node().named_child(0).unwrap();
        let name = resolve_name_via_bounded_scan(iface).expect("name found");
        assert_eq!(&src[name.byte_range()], "Greeter");
    }

    #[test]
    fn descent_does_not_confuse_a_class_return_type_with_the_function_name() {
        // A C++ method returning a class type: the `type` field is
        // `type_identifier`, which must NOT be picked up by the
        // EXACT-kind `"identifier"` bounded scan even if reached first in
        // document order. The declarator-chain tier is what actually
        // resolves this case in production (this node has a `declarator`
        // field, so `resolve_name_via_bounded_scan` is never even reached
        // for it) — this test pins the bounded-scan tier's kind exactness
        // as an independent safety property.
        let src = "class Greeter {\npublic:\n    Greeter make() { return Greeter(); }\n};\n";
        let tree = parse(tree_sitter_cpp::LANGUAGE.into(), src);
        let class_node = tree.root_node().named_child(0).unwrap();
        let body = class_node.child_by_field_name("body").unwrap();
        let mut cursor = body.walk();
        let method = body
            .named_children(&mut cursor)
            .find(|n| n.kind() == "function_definition")
            .expect("method node");
        // The declarator-chain tier (the one actually used in production)
        // must return "make", not the "Greeter" return type.
        let via_chain = resolve_name_via_declarator_chain(method).expect("name found");
        assert_eq!(&src[via_chain.byte_range()], "make");
        // And the bounded-scan tier, if it were run standalone on this same
        // node, must not match the return-type's `type_identifier` either
        // (kind mismatch by design) -- it would instead find "make" via
        // `field_identifier`... which ALSO does not match exact
        // `"identifier"`, so it correctly finds nothing, proving no false
        // positive from the return type.
        assert!(resolve_name_via_bounded_scan(method).is_none());
    }

    #[test]
    fn kind_hints_recognize_swift_and_kotlin_shared_kind_names() {
        assert_eq!(
            kind_hints_definition_role("function_declaration"),
            Some(KindRole::FunctionLike)
        );
        assert_eq!(
            kind_hints_definition_role("class_declaration"),
            Some(KindRole::TypeLike)
        );
        assert_eq!(kind_hints_definition_role("return_statement"), None);
        assert_eq!(
            kind_hints_definition_role("class_interface"),
            Some(KindRole::TypeLike)
        );
        assert_eq!(
            kind_hints_definition_role("class_implementation"),
            Some(KindRole::TypeLike)
        );
        assert_eq!(
            kind_hints_definition_role("function_definition"),
            Some(KindRole::FunctionLike)
        );
    }

    #[test]
    fn kind_hints_reject_wrapper_kinds_that_merely_contain_the_same_keyword() {
        // Verified false-positive risk: kotlin-ng's parameter-list wrapper
        // literally contains "function"; TypeScript/kotlin-ng's body/
        // heritage wrappers literally contain "class". None end with a
        // definition-site suffix, so the joint (substring AND suffix) rule
        // must reject all three.
        assert_eq!(
            kind_hints_definition_role("function_value_parameters"),
            None
        );
        assert_eq!(kind_hints_definition_role("class_body"), None);
        assert_eq!(kind_hints_definition_role("class_heritage"), None);
    }

    #[test]
    fn heritage_child_hop_finds_typescript_extends_target() {
        let src = "class Greeter extends Base {\n  greet(): number { return 1; }\n}\n";
        let tree = parse(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), src);
        let class_node = tree.root_node().named_child(0).unwrap();
        let targets = heritage_targets_via_child_hop(class_node);
        let texts: Vec<&str> = targets.iter().map(|n| &src[n.byte_range()]).collect();
        assert_eq!(texts, vec!["Base"]);
    }

    #[test]
    fn heritage_child_hop_finds_cpp_base_class_target() {
        let src = "class Greeter : public Base {\npublic:\n    int greet() { return 1; }\n};\n";
        let tree = parse(tree_sitter_cpp::LANGUAGE.into(), src);
        let class_node = tree.root_node().named_child(0).unwrap();
        let targets = heritage_targets_via_child_hop(class_node);
        let texts: Vec<&str> = targets.iter().map(|n| &src[n.byte_range()]).collect();
        assert_eq!(texts, vec!["Base"]);
    }

    #[test]
    fn positional_call_gate_rejects_swifts_call_suffix_wrapper() {
        // Verified false-positive risk: Swift's call_expression wraps its
        // argument list in a child `call_suffix` node, which ALSO contains
        // the substring "call". Without the "expression" suffix condition
        // this would double-fire per call site.
        assert!(kind_hints_positional_call("call_expression"));
        assert!(!kind_hints_positional_call("call_suffix"));
    }
}
