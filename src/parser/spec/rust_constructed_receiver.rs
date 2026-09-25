// parser::spec::rust_constructed_receiver, issue #355: the receiver type of a
// call whose receiver spells its own type, read off the expression that builds
// it: a tuple-struct constructor `Tier(1)`, a struct literal `Named { n: 3 }`
// or, in place only, `Type::assoc(..)` whose declared return type is `Self` or
// the type itself.
//
// source: measured on the issue's fixture with the 0.13.0 release: five of six
// method calls left unresolved statically, all resolved by the language server.
//
// A wrong single edge is worse than none, so every step declines on doubt. The
// type must be defined exactly once in this file, in a scope the call sees,
// with nothing else in the file that could be what the name denotes (a
// function, a constant, a static, an alias, a `use`, a local binding, a macro
// that may bind it), and it must carry no type parameters. A path before the
// name, `Self`, an enum variant, a tuple struct built through a `use` and a
// type whose impl lives in another file all decline.

use tree_sitter::Node;

use super::rust_scope::{bound_names_in_scope, once_bound_bindings};
use crate::parser::node_text;

/// source: tree-sitter-rust 0.24.2 src/node-types.json.
const FUNCTION_FIELD: &str = "function";
const VALUE_FIELD: &str = "value";
const NAME_FIELD: &str = "name";
const PATH_FIELD: &str = "path";
const TYPE_FIELD: &str = "type";
const BODY_FIELD: &str = "body";
const TRAIT_FIELD: &str = "trait";
const RETURN_TYPE_FIELD: &str = "return_type";
const TYPE_PARAMETERS_FIELD: &str = "type_parameters";

/// Item kinds that declare a type a constructor or literal can name.
const TYPE_KINDS: [&str; 3] = ["struct_item", "enum_item", "union_item"];
/// Item kinds that take the name into another namespace or rename the type.
/// source: Rust Reference, "Namespaces": functions, consts and statics share
/// the value namespace with tuple structs; an alias names another type.
const CLASHING_KINDS: [&str; 4] = ["function_item", "const_item", "static_item", "type_item"];

/// A receiver type read off the expression that builds the receiver.
pub(super) struct ConstructedHint {
    pub(super) ty: String,
    /// True when the type comes from a declared return type (`Type::assoc(..)`)
    /// and not from the constructor or literal itself.
    pub(super) via_return_type: bool,
}

/// What a constructor-shaped expression names.
enum Built {
    /// `Tier(1)`: an unqualified callee.
    Tuple(String),
    /// `Named { .. }`: an unqualified struct literal.
    Literal(String),
    /// `Type::assoc(..)`: the type, then the associated function.
    Assoc(String, String),
}

/// precondition: `node` is a `call_expression` (`recv.m(..)`), or the receiver
/// `identifier` a macro-argument scan isolated; it sits inside a parsed Rust
/// function or closure.
/// postcondition: `Some` iff the receiver is a plain identifier bound once by
/// an untyped `let` whose initialiser is a tuple constructor or a struct
/// literal, or an expression of those shapes written in place, or (in place
/// only) `Type::assoc(..)` declared to return `Self` or the type; every other
/// shape is `None`.
pub(super) fn constructed_hint(source: &str, node: Node) -> Option<ConstructedHint> {
    if node.kind() == "identifier" {
        return bound_receiver(source, node, node);
    }
    if node.kind() != "call_expression" {
        return None;
    }
    let function = node.child_by_field_name(FUNCTION_FIELD)?;
    if function.kind() != "field_expression" {
        return None;
    }
    let value = function.child_by_field_name(VALUE_FIELD)?;
    if value.kind() == "identifier" {
        return bound_receiver(source, node, value);
    }
    let built = built_by(source, value)?;
    hint_for(source, node, built)
}

/// A receiver `identifier` bound once by an untyped `let` of a tuple
/// constructor or a struct literal. `Type::assoc(..)` is left to the binding
/// rule that already types it.
fn bound_receiver(source: &str, call: Node, receiver: Node) -> Option<ConstructedHint> {
    let name = node_text(source, receiver);
    let binding = once_bound_bindings(source, call)
        .into_iter()
        .find(|b| b.name == name)?;
    let declaration = binding.declaration;
    if declaration.kind() != "let_declaration"
        || declaration.child_by_field_name(TYPE_FIELD).is_some()
        || !super::rust_item_binds::declaration_reaches(declaration, call)
        || !super::rust_return_type::names_only(source, binding.pattern, &name)
    {
        return None;
    }
    let built = built_by(source, declaration.child_by_field_name(VALUE_FIELD)?)?;
    if matches!(built, Built::Assoc(..)) {
        return None;
    }
    hint_for(source, call, built)
}

/// The constructor-shaped expression `expr` is, when it is one.
fn built_by(source: &str, expr: Node) -> Option<Built> {
    match expr.kind() {
        "struct_expression" => {
            let name = expr.child_by_field_name(NAME_FIELD)?;
            (name.kind() == "type_identifier").then(|| Built::Literal(node_text(source, name)))
        }
        "call_expression" => {
            let callee = expr.child_by_field_name(FUNCTION_FIELD)?;
            match callee.kind() {
                "identifier" => Some(Built::Tuple(node_text(source, callee))),
                "scoped_identifier" => {
                    let path = callee.child_by_field_name(PATH_FIELD)?;
                    let assoc = callee.child_by_field_name(NAME_FIELD)?;
                    (path.kind() == "identifier")
                        .then(|| Built::Assoc(node_text(source, path), node_text(source, assoc)))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The hint `built` gives for the call at `at`, after the visibility and
/// uniqueness checks of the module header.
fn hint_for(source: &str, at: Node, built: Built) -> Option<ConstructedHint> {
    match built {
        Built::Tuple(name) => {
            let item = visible_type(source, at, &name)?;
            let is_tuple = item.kind() == "struct_item"
                && item
                    .child_by_field_name(BODY_FIELD)
                    .is_some_and(|b| b.kind() == "ordered_field_declaration_list");
            is_tuple.then_some(ConstructedHint {
                ty: name,
                via_return_type: false,
            })
        }
        Built::Literal(name) => {
            let item = visible_type(source, at, &name)?;
            let is_braced = item.kind() == "struct_item"
                && item
                    .child_by_field_name(BODY_FIELD)
                    .is_some_and(|b| b.kind() == "field_declaration_list");
            is_braced.then_some(ConstructedHint {
                ty: name,
                via_return_type: false,
            })
        }
        Built::Assoc(ty, assoc) => {
            visible_type(source, at, &ty)?;
            returns_self(source, at, &ty, &assoc).then_some(ConstructedHint {
                ty,
                via_return_type: true,
            })
        }
    }
}

/// The one type item named `name` in this file, when the call at `at` sees it
/// and nothing else could be what the name denotes.
fn visible_type<'t>(source: &str, at: Node<'t>, name: &str) -> Option<Node<'t>> {
    if name == "Self" || name == "self" || name.is_empty() {
        return None;
    }
    let mut types: Vec<Node<'t>> = Vec::new();
    let mut stack = vec![root_of(at)];
    while let Some(node) = stack.pop() {
        if node.kind() == "use_declaration" {
            if mentions_word(&node_text(source, node), name) {
                return None;
            }
            continue;
        }
        if declares(source, node, name) {
            if TYPE_KINDS.contains(&node.kind()) {
                types.push(node);
            } else if CLASHING_KINDS.contains(&node.kind()) {
                return None;
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    let [item] = types.as_slice() else {
        return None;
    };
    if item.child_by_field_name(TYPE_PARAMETERS_FIELD).is_some()
        || !scope_sees(source, at, *item)
        || bound_names_in_scope(source, at).contains(name)
        || macro_may_bind(source, at, name)
    {
        return None;
    }
    Some(*item)
}

/// True when a macro of the enclosing function may bind `name`, or an item of
/// that function (`const`, `static`, `use`) does.
fn macro_may_bind(source: &str, at: Node, name: &str) -> bool {
    let scope = enclosing_function(at).unwrap_or_else(|| root_of(at));
    super::rust_macro_binds::names_macros_may_rebind(source, scope).contains(name)
        || super::rust_item_binds::names_items_bind(source, scope).contains(name)
}

fn enclosing_function(node: Node) -> Option<Node> {
    let mut current = Some(node);
    while let Some(n) = current {
        if n.kind() == "function_item" {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// True when `item` is in scope at `at`: declared in a block that encloses the
/// call, or in the module the call is in, or in the parent of that module when
/// the module glob-imports it with `use super::*;`. Modules do not inherit the
/// names of their parent, so no other module qualifies.
fn scope_sees(source: &str, at: Node, item: Node) -> bool {
    let Some(defined_in) = item.parent() else {
        return false;
    };
    let mut current = at.parent();
    while let Some(scope) = current {
        if scope.kind() == "block" && scope.id() == defined_in.id() {
            return true;
        }
        let is_module = scope.kind() == "source_file"
            || (scope.kind() == "declaration_list"
                && scope.parent().is_some_and(|p| p.kind() == "mod_item"));
        if is_module {
            if scope.id() == defined_in.id() {
                return true;
            }
            return has_super_glob(source, scope)
                && scope
                    .parent()
                    .and_then(|m| m.parent())
                    .is_some_and(|parent| parent.id() == defined_in.id());
        }
        current = scope.parent();
    }
    false
}

/// True when the module body `scope` holds `use super::*;`.
fn has_super_glob(source: &str, scope: Node) -> bool {
    let mut cursor = scope.walk();
    let found = scope.named_children(&mut cursor).any(|c| {
        c.kind() == "use_declaration"
            && node_text(source, c)
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>()
                == "usesuper::*;"
    });
    found
}

/// True when the type `ty` has exactly one function `assoc`, in one inherent
/// impl of this file, that is not `async` and returns `Self` or `ty` plainly.
fn returns_self(source: &str, at: Node, ty: &str, assoc: &str) -> bool {
    let mut functions: Vec<(Node, bool)> = Vec::new();
    let mut stack = vec![root_of(at)];
    while let Some(node) = stack.pop() {
        if node.kind() == "impl_item"
            && node
                .child_by_field_name(TYPE_FIELD)
                .is_some_and(|t| node_text(source, t) == ty)
        {
            let inherent = node.child_by_field_name(TRAIT_FIELD).is_none()
                && node.child_by_field_name(TYPE_PARAMETERS_FIELD).is_none();
            if let Some(body) = node.child_by_field_name(BODY_FIELD) {
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    if child.kind() == "function_item" && declares(source, child, assoc) {
                        functions.push((child, inherent));
                    }
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    let [(function, true)] = functions.as_slice() else {
        return false;
    };
    if is_async(source, *function) {
        return false;
    }
    function
        .child_by_field_name(RETURN_TYPE_FIELD)
        .is_some_and(|r| {
            r.kind() == "type_identifier" && {
                let text = node_text(source, r);
                text == "Self" || text == ty
            }
        })
}

fn is_async(source: &str, function: Node) -> bool {
    let mut cursor = function.walk();
    let found = function
        .named_children(&mut cursor)
        .any(|c| c.kind() == "function_modifiers" && mentions_word(&node_text(source, c), "async"));
    found
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
