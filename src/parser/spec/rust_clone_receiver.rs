// parser::spec::rust_clone_receiver, issue #390: the receiver type of a binding
// initialised by `x.clone()`.
//
//   let t = self.clone();   t.is_schedulable()   // in `impl TaskSet`
//   let c = s.clone();      c.is_schedulable()   // `s` typed by an earlier rule
//
// `(&T).clone()` is `T` when `T: Clone` (Rust Reference, "Method-call
// expressions": the receiver type `&T` is probed by value first and
// `<T as Clone>::clone` takes `&T`; inherent methods of `T` come before trait
// methods). So the hint is sound only when this file PROVES that `T: Clone`, that
// no other `clone` method of `T` competes, and that `Clone` is the std trait.
//
// Evidence taken, and declined when missing:
//   - `T` is the one plain, non-generic struct/enum/union of this file that the
//     call sees (`visible_type`), with no `#[cfg]` on it;
//   - `#[derive(Clone)]` on it (no `cfg_attr` among its attributes), or a
//     top-level `impl Clone for T` with no `#[cfg]`;
//   - no `impl T { fn clone }`, no other impl for `T` that declares `clone`, no
//     trait of this file that declares `clone`, no item named `Clone` and no
//     `use` naming `Clone` in the file;
//   - for `self`: the nearest `fn` is a method of an `impl` (not a trait default)
//     with a plain `self` / `&self` / `&mut self` parameter and a plain `T`;
//   - for an identifier: its own receiver hint (any earlier rule), whose type,
//     evidence and origin are kept as they are.
//
// source: The Rust Reference, "Method-call expressions" and "Derive"
// (https://doc.rust-lang.org/reference/expressions/method-call-expr.html);
// measured on DYResearch/dy-wcet v4.1.6 (commit 8bb83ad): lib.rs:953, 1482, 1485.

use tree_sitter::Node;

use super::rust_constructed_receiver::{declares, mentions_word, root_of, visible_type};
use super::rust_receiver::{receiver_hint_with_origin, receiver_identifier, DerivedHint};
use super::rust_scope::once_bound_bindings;
use crate::parser::node_text;

const VALUE_FIELD: &str = "value";
const TYPE_FIELD: &str = "type";
const TRAIT_FIELD: &str = "trait";
const FUNCTION_FIELD: &str = "function";
const FIELD_FIELD: &str = "field";
const ARGUMENTS_FIELD: &str = "arguments";
const BODY_FIELD: &str = "body";
const PARAMETERS_FIELD: &str = "parameters";
const TYPE_PARAMETERS_FIELD: &str = "type_parameters";
const CLONE: &str = "clone";
const CLONE_TRAIT: &str = "Clone";

/// precondition: `node` is a `call_expression` (`recv.m(..)`) or the receiver
/// `identifier`, inside a parsed Rust function.
/// postcondition: `Some` iff the receiver is a plain identifier bound once by an
/// untyped `let` that reaches the call and whose initialiser is `X.clone()` with
/// no argument, `X` being `self` or a plain identifier, and the module header's
/// evidence holds for the type; every other shape is `None`.
pub(super) fn cloned_hint(source: &str, node: Node) -> Option<DerivedHint> {
    let receiver = receiver_identifier(node)?;
    let name = node_text(source, receiver);
    let binding = once_bound_bindings(source, node)
        .into_iter()
        .find(|b| b.name == name)?;
    let declaration = binding.declaration;
    if declaration.kind() != "let_declaration"
        || declaration.child_by_field_name(TYPE_FIELD).is_some()
        || !super::rust_item_binds::declaration_reaches(declaration, node)
        || !super::rust_return_type::names_only(source, binding.pattern, &name)
    {
        return None;
    }
    let value = declaration.child_by_field_name(VALUE_FIELD)?;
    let cloned = cloned_operand(source, value)?;
    let hint = if cloned.kind() == "self" {
        self_hint(source, cloned)?
    } else {
        let inner = receiver_hint_with_origin(source, cloned)?;
        if inner.import_root.is_some() || inner.local_import.is_some() {
            return None;
        }
        inner
    };
    is_std_clone(source, node, &hint.ty).then_some(hint)
}

/// The `X` of `X.clone()` (no argument) when `X` is `self` or an identifier.
fn cloned_operand<'t>(source: &str, value: Node<'t>) -> Option<Node<'t>> {
    if value.kind() != "call_expression" {
        return None;
    }
    let function = value.child_by_field_name(FUNCTION_FIELD)?;
    let no_args = value
        .child_by_field_name(ARGUMENTS_FIELD)
        .is_some_and(|a| a.named_child_count() == 0);
    if function.kind() != "field_expression" || !no_args {
        return None;
    }
    let method = function.child_by_field_name(FIELD_FIELD)?;
    let operand = function.child_by_field_name(VALUE_FIELD)?;
    let plain = matches!(operand.kind(), "self" | "identifier");
    (node_text(source, method) == CLONE && plain).then_some(operand)
}

/// The hint of `self.clone()`: the type of the `impl` whose method holds it.
fn self_hint(source: &str, operand: Node) -> Option<DerivedHint> {
    let function = std::iter::successors(operand.parent(), |n| n.parent())
        .find(|n| n.kind() == "function_item")?;
    let params = function.child_by_field_name(PARAMETERS_FIELD)?;
    let mut cursor = params.walk();
    let plain_self = params
        .named_children(&mut cursor)
        .any(|p| p.kind() == "self_parameter" && !node_text(source, p).contains(':'));
    let list = function
        .parent()
        .filter(|p| p.kind() == "declaration_list")?;
    let implementation = list.parent().filter(|p| p.kind() == "impl_item")?;
    let ty = implementation.child_by_field_name(TYPE_FIELD)?;
    if !plain_self
        || ty.kind() != "type_identifier"
        || implementation
            .child_by_field_name(TYPE_PARAMETERS_FIELD)
            .is_some()
    {
        return None;
    }
    Some(DerivedHint {
        ty: node_text(source, ty),
        via_return_type: false,
        import_root: None,
        local_import: None,
        constructed: true,
        assoc: None,
    })
}

/// True when `ty` is a type of this file that provably has the std `Clone` and
/// whose `.clone()` no other method can take (module header).
fn is_std_clone(source: &str, at: Node, ty: &str) -> bool {
    let Some(item) = visible_type(source, at, ty) else {
        return false;
    };
    if item.child_by_field_name(TYPE_PARAMETERS_FIELD).is_some()
        || !super::rust_cfg_gate::effective_gate(source, item).is_empty()
    {
        return false;
    }
    let root = root_of(at);
    let mut derived = derives_clone(source, item);
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "use_declaration" if mentions_word(&node_text(source, node), CLONE_TRAIT) => {
                return false
            }
            "trait_item" if declares_clone_method(source, node) => return false,
            // A macro may expand to an `impl` giving `ty` another `clone`, or to
            // the `Clone` impl itself; the expansion is not in this file (#418).
            "macro_definition" if mentions_clone(&node_text(source, node)) => return false,
            "macro_invocation"
                if is_item_position(node) && mentions_word(&node_text(source, node), ty) =>
            {
                return false
            }
            "impl_item" => match clone_role(source, node, ty) {
                Role::Competes => return false,
                Role::CloneImpl => derived = true,
                Role::Unrelated => {}
            },
            _ if declares(source, node, CLONE_TRAIT) => return false,
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    derived
}

fn mentions_clone(text: &str) -> bool {
    mentions_word(text, CLONE_TRAIT) || mentions_word(text, CLONE)
}

/// True for a macro call that stands where an item can: at file level or in a
/// `mod`, `impl` or `trait` body.
fn is_item_position(call: Node) -> bool {
    call.parent()
        .is_some_and(|p| matches!(p.kind(), "source_file" | "declaration_list"))
}

/// What an `impl` means for `ty.clone()`.
enum Role {
    /// The top-level, ungated `impl Clone for ty`.
    CloneImpl,
    /// Another `clone` method of `ty` (inherent, or of another trait), or a
    /// gated `impl Clone for ty`, which may or may not exist in the build.
    Competes,
    Unrelated,
}

fn clone_role(source: &str, implementation: Node, ty: &str) -> Role {
    let of_ty = implementation
        .child_by_field_name(TYPE_FIELD)
        .is_some_and(|t| node_text(source, t) == ty);
    if !of_ty {
        return Role::Unrelated;
    }
    let is_clone_trait = implementation
        .child_by_field_name(TRAIT_FIELD)
        .is_some_and(|t| node_text(source, t) == CLONE_TRAIT);
    let gated = !super::rust_cfg_gate::effective_gate(source, implementation).is_empty();
    let top_level = implementation
        .parent()
        .is_some_and(|p| p.kind() == "source_file");
    match (is_clone_trait, gated || !top_level) {
        (true, false) => Role::CloneImpl,
        (true, true) => Role::Competes,
        (false, _) if declares_clone_method(source, implementation) => Role::Competes,
        (false, _) => Role::Unrelated,
    }
}

/// True when `container` (an `impl` or a `trait`) has a method named `clone`.
fn declares_clone_method(source: &str, container: Node) -> bool {
    let Some(body) = container.child_by_field_name(BODY_FIELD) else {
        return false;
    };
    let mut cursor = body.walk();
    let found = body.named_children(&mut cursor).any(|c| {
        matches!(c.kind(), "function_item" | "function_signature_item")
            && declares(source, c, CLONE)
    });
    found
}

/// True when `#[derive(.., Clone, ..)]` is among the attributes of `item` and no
/// attribute of it is a `cfg_attr` (which may add or remove the derive).
fn derives_clone(source: &str, item: Node) -> bool {
    let mut derives = false;
    let mut previous = item.prev_sibling();
    while let Some(attribute) = previous.filter(|p| {
        matches!(
            p.kind(),
            "attribute_item" | "line_comment" | "block_comment"
        )
    }) {
        let text = node_text(source, attribute);
        if mentions_word(&text, "cfg_attr") {
            return false;
        }
        derives |= attribute.kind() == "attribute_item" && derive_list_has_clone(&text);
        previous = attribute.prev_sibling();
    }
    derives
}

/// True when `attribute` (`#[derive(A, B)]`) is a derive and lists `Clone`. A
/// doc attribute or a comment that says the words does not count (#418).
fn derive_list_has_clone(attribute: &str) -> bool {
    let inner = attribute
        .trim_start_matches('#')
        .trim_start_matches('!')
        .trim_start()
        .trim_start_matches('[')
        .trim_start();
    inner
        .strip_prefix("derive")
        .map(str::trim_start)
        .is_some_and(|rest| rest.starts_with('(') && mentions_word(rest, CLONE_TRAIT))
}

#[cfg(test)]
mod derive_tests {
    use super::derive_list_has_clone;

    // source: issue #418 item 3: the match must read the derive list, not any
    // attribute holding the words `derive` and `Clone`.
    #[test]
    fn only_a_derive_list_names_clone() {
        assert!(derive_list_has_clone("#[derive(Clone)]"));
        assert!(derive_list_has_clone("#[derive(Debug, Clone, PartialEq)]"));
        assert!(derive_list_has_clone("#[derive (std::clone::Clone)]"));
        assert!(!derive_list_has_clone("#[derive(Debug)]"));
        assert!(!derive_list_has_clone(
            "#[doc = \"never derive Clone here\"]"
        ));
        assert!(!derive_list_has_clone("#[allow(derive_clone)]"));
    }
}
