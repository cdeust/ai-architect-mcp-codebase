// parser::spec::rust_closure_receiver, issue #390: the receiver type of a call
// that reads a closure. Two forms:
//
//   - the result of a local closure, `let build = |c| { let mut t = T::new(); t };
//     build(1).m()`: the type is what the closure returns, read off its written
//     return type or off its tail expression when that is a constructor of this
//     file or a binding whose own type is known;
//   - an untyped closure parameter, `build(x).is_some_and(|s| s.m())`: the
//     parameter of a closure that is the only argument of an `Option` or `Result`
//     method takes the payload type of the free function's declared return type
//     (`rust_return_type::payload_hint` lists the methods and the source).
//
// source: measured on DYResearch/dy-wcet v4.1.6 (commit 8bb83ad): `tests/
// properties.rs:241,246` (result of a local closure) and `tests/exhaustive.rs:196`
// (`build(&trial).is_some_and(|s| s.is_schedulable())`) are two of the seven calls
// of `TaskSet::is_schedulable` rust-analyzer resolves and 0.14.0 leaves open.
//
// A wrong single edge is worse than none, so every step declines on doubt. The
// closure must be the one `let` binding of its name in the function, untyped,
// not `async`; a closure with `return` or `?` needs no special case (every exit
// of a closure has the type of its tail), but a tail that is not a plain
// constructor or identifier declines. The type must be one `visible_type`
// accepts at the call: unique in this file, without type parameters, with no
// namesake in the way.

use tree_sitter::Node;

use super::rust_constructed_receiver::{hint_of_expression, visible_type};
use super::rust_receiver::{receiver_hint_with_origin, DerivedHint};
use super::rust_scope::once_bound_bindings;
use crate::parser::node_text;

/// source: tree-sitter-rust 0.24.2 src/node-types.json.
const VALUE_FIELD: &str = "value";
const TYPE_FIELD: &str = "type";
const BODY_FIELD: &str = "body";
const RETURN_TYPE_FIELD: &str = "return_type";
const FUNCTION_FIELD: &str = "function";
const ARGUMENTS_FIELD: &str = "arguments";

/// Node kinds a block's last child may be without being the block's value.
const NON_TAIL_KINDS: [&str; 2] = ["line_comment", "block_comment"];

/// precondition: `call` is a `call_expression` (`recv.m(..)`) inside a parsed
/// Rust function.
/// postcondition: `Some` iff `recv` is `f(..)` where `f` is the plain name of a
/// local bound once by an untyped `let f = |..| ..;` that reaches `call`, is not
/// `async`, and the closure returns a known type: its written return type or a
/// tail that is a struct literal, a tuple constructor or `Type::assoc(..)`
/// returning `Self` (all of this file, `constructed`), or an identifier, whose own
/// receiver hint is then the answer as it stands (a closure has one return type,
/// so every exit has the type of its tail). Every other shape is `None`.
pub(super) fn closure_result_hint(source: &str, call: Node) -> Option<DerivedHint> {
    let function = call.child_by_field_name(FUNCTION_FIELD)?;
    let receiver = function
        .child_by_field_name(VALUE_FIELD)
        .filter(|_| function.kind() == "field_expression")?;
    if receiver.kind() != "call_expression" {
        return None;
    }
    let callee = receiver.child_by_field_name(FUNCTION_FIELD)?;
    closure_result_of(source, callee)
}

/// precondition: `callee` is the `identifier` token at the start of `f(..)`, the
/// receiver of a method call, inside a parsed Rust function (a call expression's
/// callee, or a name in a macro's argument tokens).
/// postcondition: as `closure_result_hint` for the closure `f` names.
pub(super) fn closure_result_of(source: &str, callee: Node) -> Option<DerivedHint> {
    if callee.kind() != "identifier" {
        return None;
    }
    let name = node_text(source, callee);
    let binding = once_bound_bindings(source, callee)
        .into_iter()
        .find(|b| b.name == name)?;
    let declaration = binding.declaration;
    if declaration.kind() != "let_declaration"
        || declaration.child_by_field_name(TYPE_FIELD).is_some()
        || !super::rust_item_binds::declaration_reaches(declaration, callee)
        || !super::rust_return_type::names_only(source, binding.pattern, &name)
    {
        return None;
    }
    let closure = declaration.child_by_field_name(VALUE_FIELD)?;
    if closure.kind() != "closure_expression" || node_text(source, closure).starts_with("async") {
        return None;
    }
    if let Some(written) = closure.child_by_field_name(RETURN_TYPE_FIELD) {
        let ty = super::rust_return_type::plain_type(source, written)?;
        visible_type(source, callee, &ty)?;
        return Some(spelled(ty, false));
    }
    tail_hint(source, callee, closure.child_by_field_name(BODY_FIELD)?)
}

/// A hint whose type the closure itself spells: only this file's candidates.
fn spelled(ty: String, via_return_type: bool) -> DerivedHint {
    DerivedHint {
        ty,
        via_return_type,
        import_root: None,
        local_import: None,
        constructed: true,
        assoc: None,
    }
}

/// The hint the value of `body` gives, seen from the call at `at`.
fn tail_hint(source: &str, at: Node, body: Node) -> Option<DerivedHint> {
    let tail = if body.kind() == "block" {
        let mut cursor = body.walk();
        let last = body
            .named_children(&mut cursor)
            .filter(|c| !NON_TAIL_KINDS.contains(&c.kind()))
            .last();
        last?
    } else {
        body
    };
    match tail.kind() {
        "identifier" => receiver_hint_with_origin(source, tail),
        "struct_expression" | "call_expression" => {
            let built = hint_of_expression(source, at, tail)?;
            Some(spelled(built.ty, built.via_return_type))
        }
        _ => None,
    }
}

/// precondition: `call_node` is a node inside a parsed Rust function and
/// `receiver` the plain `identifier` that names the receiver of the call.
/// postcondition: `Some(T)` iff the receiver is bound once, as the only
/// untyped parameter of a closure that is the only argument of `x.method(..)`
/// where `x` is a call of a free function of this file returning an `Option` or
/// `Result` of `T` that `method` opens (`rust_return_type::payload_hint`), and
/// the call is in the closure's body; every other shape is `None`.
pub(super) fn closure_parameter_hint(
    source: &str,
    call_node: Node,
    receiver: Node,
) -> Option<super::rust_return_type::ReturnTypeHint> {
    let name = node_text(source, receiver);
    let binding = once_bound_bindings(source, call_node)
        .into_iter()
        .find(|b| b.name == name)?;
    let parameter = binding.declaration;
    let list = parameter
        .parent()
        .filter(|p| p.kind() == "closure_parameters")?;
    let closure = list.parent().filter(|p| p.kind() == "closure_expression")?;
    let names_it = super::rust_return_type::names_only(source, binding.pattern, &name);
    let alone = list.named_child_count() == 1 && parameter.id() == binding.pattern.id();
    let in_body = closure
        .child_by_field_name(BODY_FIELD)
        .is_some_and(|b| super::rust_item_binds::is_ancestor(b, call_node));
    if !(names_it && alone && in_body) {
        return None;
    }
    let arguments = closure.parent().filter(|p| p.kind() == "arguments")?;
    let outer = arguments
        .parent()
        .filter(|p| p.kind() == "call_expression")?;
    let only = arguments.named_child_count() == 1;
    let opened = outer.child_by_field_name(FUNCTION_FIELD)?;
    if !only
        || opened.kind() != "field_expression"
        || outer.child_by_field_name(ARGUMENTS_FIELD)?.id() != arguments.id()
    {
        return None;
    }
    let method = node_text(source, opened.child_by_field_name("field")?);
    let producer = opened.child_by_field_name(VALUE_FIELD)?;
    if producer.kind() != "call_expression" {
        return None;
    }
    super::rust_return_type::payload_hint(source, call_node, producer, &method)
}
