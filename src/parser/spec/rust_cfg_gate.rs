// parser::spec::rust_cfg_gate: the effective `#[cfg]` gate of a Rust item, and
// the id suffix that keeps two twins apart (issue #353).
//
// Two items of one name under mutually exclusive predicates
// (`#[cfg(feature = "fast")] fn pick` and `#[cfg(not(feature = "fast"))] fn
// pick`) share one qualified name. The graph keeps one node per (label,
// qualified name), so the second twin used to vanish and every call to the name
// resolved to the first at confidence 0.95, whichever twin the build compiles.
//
// The fix keeps the qualified name of every item that does not collide, and
// appends `#cfg(<compact gate>)` to the qualified name of each member of a
// collision: `src/lib.rs::pick#cfg(not(feature=fast))`. The suffix has no `::`,
// `.`, `:` or `#`, so the id parsers that split on `::` or strip a `#<digits>`
// tail read it as part of the last segment.
//
// The gate of an item is the `all` of every `#[cfg]` that reaches it: its own
// outer attributes, those of every enclosing `mod`, `impl`, `trait` and `fn`, and
// the inner `#![cfg]` attributes of the file or of an enclosing inline module.
// `cfg_attr` is NOT expanded: `#[cfg_attr(feature = "x", cfg(unix))]` gates
// nothing here, so an item under it has no gate from it (a `cfg_attr` that adds a
// `cfg` is a rare spelling; two twins told apart only by it stay one node, as
// before this change). Comments inside `cfg(..)` do not count: they are removed
// before the predicate is read, so adding one never renames an id. A predicate
// that does not parse is kept as its comment-free text, so two different unparsed gates still give two different suffixes.
// source: The Rust Reference, "Conditional compilation" (the `cfg` attribute
// applies to the item it precedes; a `cfg` on a container gates its contents).

use tree_sitter::Node;

use super::walkers::WalkCtx;
use crate::parser::cfg_expr::{parse_cfg_arguments, without_comments, CfgPredicate};
use crate::parser::node_text;

/// Opens the suffix of a twin id. The suffix closes with `)`.
pub(crate) const TWIN_MARK: &str = "#cfg(";

/// The kinds of enclosing item whose own `#[cfg]` gates everything inside them.
const GATING_ANCESTORS: &[&str] = &["mod_item", "impl_item", "trait_item", "function_item"];

/// The compact, canonical gate of `item`, or `""` when nothing gates it.
pub(crate) fn effective_gate(source: &str, item: Node) -> String {
    let predicates = gate_predicates(source, item);
    if predicates.is_empty() {
        return String::new();
    }
    CfgPredicate::All(predicates).canonical().compact()
}

/// True when `item` exists only in a build that sets `cfg(test)`: some `#[cfg]`
/// that reaches it (see `effective_gate`) requires the bare option `test`.
/// `cfg(any(test, ..))`, `cfg(not(test))` and `cfg_attr(test, ..)` do not.
/// source: issue #354.
pub(crate) fn gate_requires_test(source: &str, item: Node) -> bool {
    CfgPredicate::All(gate_predicates(source, item)).requires_option("test")
}

/// Every `#[cfg]` predicate that reaches `item`, own attributes first.
fn gate_predicates(source: &str, item: Node) -> Vec<CfgPredicate> {
    let mut predicates = outer_cfgs(source, item);
    let mut ancestor = item.parent();
    while let Some(node) = ancestor {
        if GATING_ANCESTORS.contains(&node.kind()) {
            predicates.extend(outer_cfgs(source, node));
        }
        if node.kind() == "declaration_list" || node.kind() == "source_file" {
            predicates.extend(inner_cfgs(source, node));
        }
        ancestor = node.parent();
    }
    predicates
}

/// The `#[cfg(..)]` outer attributes that precede `item`, comments skipped.
/// They stop at the first sibling that is neither an attribute nor a comment,
/// so an attribute never leaks to the next item.
fn outer_cfgs(source: &str, item: Node) -> Vec<CfgPredicate> {
    let mut found = Vec::new();
    let mut sibling = item.prev_named_sibling();
    while let Some(previous) = sibling {
        match previous.kind() {
            "attribute_item" => found.extend(cfg_of(source, previous)),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        sibling = previous.prev_named_sibling();
    }
    found
}

/// The `#![cfg(..)]` inner attributes among the direct children of `container`.
fn inner_cfgs(source: &str, container: Node) -> Vec<CfgPredicate> {
    let mut cursor = container.walk();
    container
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "inner_attribute_item")
        .filter_map(|child| cfg_of(source, child))
        .collect()
}

/// The predicate of one `attribute_item` when it is a `cfg` attribute.
fn cfg_of(source: &str, attribute_item: Node) -> Option<CfgPredicate> {
    let attribute = attribute_item.named_child(0)?;
    if attribute.kind() != "attribute" {
        return None;
    }
    let name = attribute.named_child(0)?;
    if node_text(source, name).as_str() != "cfg" {
        return None;
    }
    let raw = node_text(source, attribute.child_by_field_name("arguments")?);
    let raw = without_comments(raw.as_str());
    Some(
        parse_cfg_arguments(&raw).unwrap_or_else(|| CfgPredicate::Option {
            key: "unparsed".to_string(),
            value: Some(raw.split_whitespace().collect()),
        }),
    )
}

/// The suffix appended to the qualified name of a twin whose gate is `gate`.
pub(crate) fn twin_suffix(gate: &str) -> String {
    format!("{TWIN_MARK}{gate})")
}

/// The gate a qualified name carries in its own suffix, or `None` for a name
/// that is not a twin. Only a suffix at the very end counts: the children of a
/// twin module carry the suffix in the middle of their name, not as a gate of
/// their own.
pub(crate) fn gate_of_qualified_name(qn: &str) -> Option<&str> {
    let body = qn.strip_suffix(')')?;
    let (_, gate) = body.rsplit_once(TWIN_MARK)?;
    Some(gate)
}

/// The qualified name of an item that may have a twin: `plain` when no other
/// item of this label collides with it, else `plain` plus the suffix of the
/// item's own gate. `ctx.twins` is empty on the first walk of a file, so the
/// first walk never changes a name.
pub(crate) fn twin_qn(ctx: &WalkCtx, node: Node, label: &str, plain: String) -> String {
    if ctx.twins.is_empty() || !ctx.twins.contains_key(&(label.to_string(), plain.clone())) {
        return plain;
    }
    let gate = effective_gate(ctx.source, node);
    format!("{plain}{}", twin_suffix(&gate))
}

/// The types an `impl` block can be written for and that can have twins.
const IMPL_OWNER_LABELS: [&str; 3] = ["Struct", "Enum", "Trait"];

/// The id an `impl` block's methods attach to when its type is `plain`.
///
/// Not a twin: `plain`. A twin type has no node named `plain` any more, so an
/// edge from it would dangle; the impl belongs to a twin only when its own gate
/// is exactly the gate of one member, and then to that member's id. Otherwise
/// (an ungated impl, a gate that names none of them, or one that names several)
/// the block is not tied to a twin and the owner is `None`: no edge, the safe
/// answer, since which twin the impl serves is the build's to say.
pub(crate) fn impl_owner(ctx: &WalkCtx, impl_node: Node, plain: &str) -> Option<String> {
    let member_gates = IMPL_OWNER_LABELS
        .iter()
        .find_map(|label| ctx.twins.get(&(label.to_string(), plain.to_string())));
    let Some(member_gates) = member_gates else {
        return Some(plain.to_string());
    };
    let gate = effective_gate(ctx.source, impl_node);
    (!gate.is_empty() && member_gates.contains(&gate))
        .then(|| format!("{plain}{}", twin_suffix(&gate)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    /// The gate of the first item of `kind` in `source`.
    fn gate_of_first(source: &str, kind: &str) -> String {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("rust grammar");
        let tree = parser.parse(source, None).expect("parses");
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == kind {
                return effective_gate(source, node);
            }
            let mut cursor = node.walk();
            let children: Vec<Node> = node.named_children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
        panic!("no {kind} in {source}");
    }

    #[test]
    fn an_ungated_item_has_the_empty_gate() {
        assert_eq!(gate_of_first("fn f() {}", "function_item"), "");
    }

    #[test]
    fn an_outer_cfg_is_the_gate_and_its_spelling_does_not_matter() {
        let a = gate_of_first(
            "#[cfg(not(feature = \"fast\"))]\nfn f() {}",
            "function_item",
        );
        let b = gate_of_first(
            "#[cfg( not( feature=\"fast\" ) )] fn f() {}",
            "function_item",
        );
        assert_eq!(a, "not(feature=fast)");
        assert_eq!(a, b);
    }

    #[test]
    fn several_cfgs_combine_as_all_in_a_stable_order() {
        let source = "#[cfg(unix)]\n#[cfg(feature = \"x\")]\nfn f() {}";
        assert_eq!(
            gate_of_first(source, "function_item"),
            "all(feature=x,unix)"
        );
    }

    #[test]
    fn the_gate_of_an_enclosing_module_impl_and_file_reaches_the_item() {
        let source = "#![cfg(target_os = \"linux\")]\n\
                      #[cfg(feature = \"a\")]\nmod m {\n#[cfg(kani)]\nimpl S {\nfn f() {}\n}\n}";
        assert_eq!(
            gate_of_first(source, "function_item"),
            "all(feature=a,kani,target_os=linux)"
        );
    }

    #[test]
    fn a_cfg_does_not_leak_to_the_next_item() {
        let source = "#[cfg(unix)]\nfn a() {}\nfn b() {}";
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("rust grammar");
        let tree = parser.parse(source, None).expect("parses");
        let root = tree.root_node();
        let mut cursor = root.walk();
        let functions: Vec<Node> = root
            .named_children(&mut cursor)
            .filter(|n| n.kind() == "function_item")
            .collect();
        assert_eq!(effective_gate(source, functions[0]), "unix");
        assert_eq!(effective_gate(source, functions[1]), "");
    }

    #[test]
    fn an_unparsed_predicate_is_kept_as_its_text() {
        let a = gate_of_first("#[cfg(a::b)]\nfn f() {}", "function_item");
        let b = gate_of_first("#[cfg(c::d)]\nfn f() {}", "function_item");
        assert_ne!(a, b);
        assert!(!a.contains("::"), "{a}");
    }

    #[test]
    fn a_suffix_round_trips_and_only_the_last_one_is_a_gate() {
        let suffix = twin_suffix("not(feature=fast)");
        let qn = format!("src/lib.rs::pick{suffix}");
        assert_eq!(gate_of_qualified_name(&qn), Some("not(feature=fast)"));
        assert_eq!(gate_of_qualified_name("src/lib.rs::pick"), None);
        let child = format!("src/lib.rs::m{suffix}::f");
        assert_eq!(gate_of_qualified_name(&child), None);
    }
}
