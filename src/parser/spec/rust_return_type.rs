// parser::spec::rust_return_type — issues #348 and #349: the receiver type of a
// local that a free function initialised, read off that function's declared
// return type.
//
// source: measured on DYResearch/dy-wcet v4.1.6 (commit 8bb83ad) with the
// 0.13.0 release: 15 of the 17 sites the static run left unresolved bind their
// receiver this way (`let s = generate(..)`, `let Some(s) = build(..) else
// { .. }`, `admitted(t).expect(..)`), and the language-server pass resolves
// them all, so the type is knowable from the signature.
//
// A wrong single edge is worse than none, so every step declines on doubt:
// the callee is a plain identifier naming exactly ONE free function of the
// file, declared in a scope that encloses the call, with no `use`, tuple
// struct, `const` or `static` of the same name in the file and no local of
// that name; its return type is a plain named type that is not one of its own
// generic parameters. An `Option` or `Result` is unwrapped only through a form
// the source spells out: `let Some(s) = .. else`, `let Ok(s) = .. else`,
// `.expect(..)`, `.unwrap()` and `?`. A callee in another file is out of
// reach for this single-file pass and stays for the language server.

use tree_sitter::Node;

use super::rust_scope::{bound_names_in_scope, constructor_call, once_bound_bindings, OnceBound};
use crate::parser::node_text;

/// source: tree-sitter-rust 0.24.2 src/node-types.json.
const FUNCTION_FIELD: &str = "function";
const NAME_FIELD: &str = "name";
const TYPE_FIELD: &str = "type";
const VALUE_FIELD: &str = "value";
const RETURN_TYPE_FIELD: &str = "return_type";
const TYPE_PARAMETERS_FIELD: &str = "type_parameters";
const ALTERNATIVE_FIELD: &str = "alternative";
const TYPE_ARGUMENTS_FIELD: &str = "type_arguments";

/// Item kinds that bind a NAME a bare call could reach besides a function.
/// source: Rust Reference, "Namespaces": tuple structs and consts/statics
/// share the value namespace with functions.
const NAME_CLASHING_KINDS: [&str; 6] = [
    "struct_item",
    "enum_item",
    "union_item",
    "type_item",
    "const_item",
    "static_item",
];

/// The two std wrappers an explicit unwrapping form may open.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wrapper {
    Option,
    Result,
}

/// How the binding takes its value from the initialiser.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// `let s = <init>;`
    Plain,
    /// `let Some(s) = <init> else { .. };` or `let Ok(s) = <init> else { .. };`
    LetElse(Wrapper),
}

/// A function's declared return type.
enum Returned<'t> {
    Plain(Node<'t>),
    Wrapped(Wrapper, Node<'t>),
}

/// precondition: `call_node` is a node inside a parsed Rust function and
/// `receiver` the plain `identifier` that names the receiver of the call.
/// postcondition: `Some(T)` iff the receiver is bound exactly once, by a
/// `let` with no written type whose initialiser is a call of a free function
/// of this file whose return type gives `T` as described in the module
/// header; `None` on every other shape. `T` is the type's last `::` segment
/// with generics stripped, like `receiver_hint`.
pub(super) fn return_type_hint(source: &str, call_node: Node, receiver: Node) -> Option<String> {
    let name = node_text(source, receiver);
    let binding = once_bound_bindings(source, call_node)
        .into_iter()
        .find(|b| b.name == name)?;
    let declaration = binding.declaration;
    if declaration.kind() != "let_declaration"
        || declaration.child_by_field_name(TYPE_FIELD).is_some()
    {
        return None;
    }
    if !super::rust_item_binds::declaration_reaches(declaration, call_node) {
        return None;
    }
    let shape = binding_shape(source, &binding)?;
    let value = declaration.child_by_field_name(VALUE_FIELD)?;
    let (call, unwrapped) = constructor_call(source, value, true)?;
    let callee = call.child_by_field_name(FUNCTION_FIELD)?;
    if callee.kind() != "identifier" {
        return None;
    }
    let callee_name = node_text(source, callee);
    if bound_names_in_scope(source, call_node).contains(&callee_name) {
        return None;
    }
    let function = unique_visible_function(source, call, &callee_name)?;
    let ty = pick_type(source, shape, unwrapped, returned_type(source, function)?)?;
    (!is_own_generic(source, function, &ty) && !file_declares_alias(source, call, &ty))
        .then_some(ty)
}

/// True when the file declares `type <ty> = ..`: the return type then names
/// another type, which the last-segment lookup would not find.
fn file_declares_alias(source: &str, node: Node, ty: &str) -> bool {
    let mut stack = vec![root_of(node)];
    while let Some(current) = stack.pop() {
        if current.kind() == "type_item" && declares(source, current, ty) {
            return true;
        }
        // `use x::Real as Local`: the name says nothing of the type it renames.
        if current.kind() == "use_as_clause"
            && current
                .child_by_field_name("alias")
                .is_some_and(|a| node_text(source, a) == ty)
        {
            return true;
        }
        let mut cursor = current.walk();
        stack.extend(current.named_children(&mut cursor));
    }
    false
}

/// How `binding` takes its value, from its pattern and the `else` of its
/// declaration. `None` for every pattern this pass does not cover.
fn binding_shape(source: &str, binding: &OnceBound) -> Option<Shape> {
    let pattern = binding.pattern;
    if names_only(source, pattern, &binding.name) {
        return Some(Shape::Plain);
    }
    if pattern.kind() != "tuple_struct_pattern"
        || binding
            .declaration
            .child_by_field_name(ALTERNATIVE_FIELD)
            .is_none()
    {
        return None;
    }
    let head = pattern.child_by_field_name(TYPE_FIELD)?;
    let wrapper = match node_text(source, head).as_str() {
        "Some" => Wrapper::Option,
        "Ok" => Wrapper::Result,
        _ => return None,
    };
    let mut cursor = pattern.walk();
    let inner: Vec<Node> = pattern
        .named_children(&mut cursor)
        .filter(|c| c.id() != head.id())
        .collect();
    (inner.len() == 1 && names_only(source, inner[0], &binding.name))
        .then_some(Shape::LetElse(wrapper))
}

/// True when `pattern` is exactly the identifier `name`, optionally `mut`.
fn names_only(source: &str, pattern: Node, name: &str) -> bool {
    match pattern.kind() {
        "identifier" => node_text(source, pattern) == name,
        "mut_pattern" => {
            let mut cursor = pattern.walk();
            let mut children = pattern.named_children(&mut cursor);
            children
                .next()
                .is_some_and(|c| c.kind() == "identifier" && node_text(source, c) == name)
                && children.next().is_none()
        }
        _ => false,
    }
}

/// The type the binding gets, per the shape of its pattern, whether the
/// initialiser was unwrapped in the source, and the function's return type.
fn pick_type(source: &str, shape: Shape, unwrapped: bool, returned: Returned) -> Option<String> {
    match (shape, unwrapped, returned) {
        (Shape::Plain, false, Returned::Plain(ty)) => plain_type(source, ty),
        (Shape::Plain, true, Returned::Wrapped(_, inner)) => plain_type(source, inner),
        (Shape::LetElse(open), false, Returned::Wrapped(wrapper, inner)) if open == wrapper => {
            plain_type(source, inner)
        }
        _ => None,
    }
}

/// The declared return type of `function`, split into an `Option` or `Result`
/// and its first type argument when it is one. `None` for a function with no
/// return type, or when the file defines a type of its own named `Option` or
/// `Result`, which would make the split a guess.
fn returned_type<'t>(source: &str, function: Node<'t>) -> Option<Returned<'t>> {
    let ty = function.child_by_field_name(RETURN_TYPE_FIELD)?;
    let Some(wrapper) = wrapper_of(source, ty) else {
        return Some(Returned::Plain(ty));
    };
    if file_defines_wrapper_name(source, ty) {
        return None;
    }
    let args = ty.child_by_field_name(TYPE_ARGUMENTS_FIELD)?;
    let mut cursor = args.walk();
    let first = args
        .named_children(&mut cursor)
        .find(|c| c.kind() != "lifetime")?;
    Some(Returned::Wrapped(wrapper, first))
}

/// `Some(Option)` for `Option<T>` and `Some(Result)` for `Result<T, E>` or
/// `io::Result<T>`, judged by the last segment of the head of a generic type.
fn wrapper_of(source: &str, ty: Node) -> Option<Wrapper> {
    if ty.kind() != "generic_type" {
        return None;
    }
    let head = ty.child_by_field_name(TYPE_FIELD)?;
    let last = match head.kind() {
        "type_identifier" => node_text(source, head),
        "scoped_type_identifier" => node_text(source, head.child_by_field_name(NAME_FIELD)?),
        _ => return None,
    };
    match last.as_str() {
        "Option" => Some(Wrapper::Option),
        "Result" => Some(Wrapper::Result),
        _ => None,
    }
}

/// A named type with its reference and generics stripped, reduced to its last
/// `::` segment. `None` for `impl Trait`, `dyn Trait`, tuples, arrays and the
/// like: those are not a type a method can be looked up on by name.
fn plain_type(source: &str, node: Node) -> Option<String> {
    match node.kind() {
        "reference_type" | "generic_type" => {
            plain_type(source, node.child_by_field_name(TYPE_FIELD)?)
        }
        "type_identifier" => Some(node_text(source, node)),
        // A path before the name (`other::Set`) says which type is meant; the
        // resolver keeps the last segment only, so it could pick a namesake.
        _ => None,
    }
}

/// True when `ty` is one of the generic parameters `function` declares, so its
/// return type names no concrete type. Judged on the words of the parameter
/// list, which can only over-decline.
fn is_own_generic(source: &str, function: Node, ty: &str) -> bool {
    function
        .child_by_field_name(TYPE_PARAMETERS_FIELD)
        .is_some_and(|params| mentions_word(&node_text(source, params), ty))
}

/// The one free function named `name` declared in this file, when its scope
/// encloses `call` and nothing else in the file could be what `name` denotes.
fn unique_visible_function<'t>(source: &str, call: Node<'t>, name: &str) -> Option<Node<'t>> {
    let mut functions: Vec<Node<'t>> = Vec::new();
    let mut stack = vec![root_of(call)];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "function_item" if declares(source, node, name) && is_free_function(node) => {
                functions.push(node);
            }
            "use_declaration" if mentions_word(&node_text(source, node), name) => return None,
            kind if NAME_CLASHING_KINDS.contains(&kind) && declares(source, node, name) => {
                return None;
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    let [function] = functions.as_slice() else {
        return None;
    };
    let scope = function.parent()?;
    super::rust_item_binds::is_ancestor(scope, call).then_some(*function)
}

/// True when the file declares a struct, enum, union or alias named `Option`
/// or `Result`.
fn file_defines_wrapper_name(source: &str, ty: Node) -> bool {
    let mut stack = vec![root_of(ty)];
    while let Some(node) = stack.pop() {
        if NAME_CLASHING_KINDS.contains(&node.kind())
            && ["Option", "Result"]
                .iter()
                .any(|w| declares(source, node, w))
        {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

/// A `function_item` that is not a method: at file level, in a `mod`, or
/// nested in a body, never in an `impl` or `trait`.
fn is_free_function(function: Node) -> bool {
    match function.parent().map(|p| (p.kind(), p.parent())) {
        Some(("source_file", _)) | Some(("block", _)) => true,
        Some(("declaration_list", Some(owner))) => owner.kind() == "mod_item",
        _ => false,
    }
}

fn declares(source: &str, node: Node, name: &str) -> bool {
    node.child_by_field_name(NAME_FIELD)
        .is_some_and(|n| node_text(source, n) == name)
}

fn root_of(node: Node) -> Node {
    let mut current = node;
    while let Some(parent) = current.parent() {
        current = parent;
    }
    current
}

/// True when `text` holds `word` as a whole identifier.
fn mentions_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| w == word)
}
