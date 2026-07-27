// parser::spec::walkers::cpp — the hybrid C-family class-model definition
// walker (ADR-0055 phase 7): dispatch plus the SCOPE CONTAINERS (namespace,
// class/struct/union, function definition). The member emitters — enum members,
// method prototypes, data fields, nested types, typedefs, aliases — live in the
// sibling `cpp_members` module; the split is the concern boundary between "what
// opens a scope" and "what a scope contains" (§1.1, and it keeps both files
// under the §4.1 size cap).
//
// C++ is neither flat C (`clike`, which has no namespace/class/method model) nor
// the pure class-model (`walk_defs`, with `class_inheritance`/dedup): it is a
// class-recursive DFS whose ONE per-file `seq` counter keys functions, methods,
// prototypes, AND call sites in a single order. That ordering is part of the
// emitted contract, so C++ keeps this dedicated walker driven by the
// `CppFamilySpec` sub-table, and `walk_defs` delegates here whenever a `LangSpec`
// carries `cpp_family: Some(_)`. The six languages riding `walk_defs`/`clike`
// stay untouched. Calls and imports still route through the SHARED generic
// walkers (`calls::walk_calls`, `imports::walk_imports`) via the conventions —
// only the definition shapes are C++-specific.
//
// What IS shared with the flat family is the declarator name search
// (`walkers/declarator`), driven by the `DeclaratorNaming` row C++ carries: the
// #106 fix ("the name is what the declarator binds, never a parameter") is
// inherited as data, which is what closed #123.
//
// `enclosing_type` is `Some(class_qn)` while walking a class/struct/union body
// and `None` at file or namespace scope. It drives the class-scope divergences: a
// `function_definition` becomes a `Method`/`HasMethod` (not a `Function`/
// `Defines`) inside a class, and a member declaration is only emitted there.
//
// Every node kind and field: tree-sitter-cpp 0.23.4 src/node-types.json,
// validated by the spec guard.

use tree_sitter::Node;

use super::super::lang_spec::{CppFamilySpec, LangSpec};
use super::cpp_members;
use super::declarator::{declarator_name, named_or_first_identifier};
use super::{calls, end_line_of, imports, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_text, qual, ExtractedNode, ExtractedRef, LABEL_FUNCTION, LABEL_METHOD, LABEL_STRUCT,
};

/// Hybrid C++ definition walker: dispatches each child of `parent` to the
/// concern its node kind names in `cf`, recursing into class/namespace bodies
/// and transparently through templates and any unmatched wrapper with named
/// children. All dispatched kinds are disjoint EXCEPT `member_decl_kinds`, whose
/// `declaration` also occurs at file/namespace scope — its arm is therefore
/// guarded on being inside a class scope, and falls through to the recursion arm
/// otherwise (which is how `struct S { int x; } v;` at file scope is still
/// reached).
pub(super) fn walk_cpp_defs(
    spec: &LangSpec,
    cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    parent: Node,
    scope: &str,
    enclosing_type: Option<&str>,
) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        let k = child.kind();
        if kind_in(cf.namespace_kinds, k) {
            emit_namespace(spec, cf, ctx, child, scope);
        } else if kind_in(cf.class_kinds, k) {
            emit_class_like(spec, cf, ctx, child, scope, /*is_class=*/ true);
        } else if kind_in(cf.struct_kinds, k) {
            emit_class_like(spec, cf, ctx, child, scope, /*is_class=*/ false);
        } else if kind_in(cf.enum_kinds, k) {
            cpp_members::emit_enum(spec, cf, ctx, child, scope);
        } else if kind_in(cf.template_kinds, k) {
            // Templates wrap a class/function; descend transparently, same scope
            // and enclosing type, emitting no node of their own.
            walk_cpp_defs(spec, cf, ctx, child, scope, enclosing_type);
        } else if kind_in(cf.func_def_kinds, k) {
            emit_function(spec, cf, ctx, child, scope, enclosing_type);
        } else if kind_in(cf.field_decl_kinds, k) {
            // A member declaration is only emitted inside a class/struct body: a
            // prototype `Method` when it carries a function declarator, else its
            // data members plus any nested type it declares inline. At
            // file/namespace scope `field_declaration` does not occur, so this
            // arm is a no-op there.
            if enclosing_type.is_some() {
                cpp_members::emit_member(spec, cf, ctx, child, scope);
            }
        } else if kind_in(cf.member_decl_kinds, k) && enclosing_type.is_some() {
            // A constructor/destructor declaration: the grammar spells it as a
            // bare `declaration` (no return type), which is why the pre-#124
            // walker emitted nothing for `Point(int,int);` / `~Point();`. It goes
            // through the same per-declarator member path as a `field_declaration`
            // — the shapes differ only in whether the grammar supplies a `type`.
            //
            // No "does it carry a function declarator?" pre-filter: `emit_member`
            // already decides per declared name and emits nothing for a declarator
            // that binds no name, so a second, coarser test upstream would be a
            // branch no input can distinguish (§9 — mutation testing found exactly
            // that: every mutant of the filter survived).
            cpp_members::emit_member(spec, cf, ctx, child, scope);
        } else if kind_in(cf.typedef_kinds, k) {
            cpp_members::emit_typedef(spec, cf, ctx, child, scope);
        } else if kind_in(cf.alias_kinds, k) {
            cpp_members::emit_alias(spec, cf, ctx, child, scope);
        } else if kind_in(spec.import_node_kinds, k) {
            // `preproc_include` and `using_declaration`; the conventions shape
            // each (include vs using) into an `Import`.
            imports::walk_imports(spec, ctx, child, scope);
        } else if child.named_child_count() > 0 {
            // Transparent recursion into an unmatched wrapper with named children
            // (access specifiers, linkage specs, preprocessor conditionals, a
            // file-scope `declaration` wrapping an inline type, …), same scope and
            // enclosing type.
            // mutation note (§12): the `> 0` → `>= 0` mutant is EQUIVALENT —
            // recursing into a childless node emits nothing (the child loop
            // iterates zero times), so no test can observe a difference. Kept as
            // a faithful copy of the old walker's guard and a cheap leaf skip.
            walk_cpp_defs(spec, cf, ctx, child, scope, enclosing_type);
        }
    }
}

/// What a declarator DECLARES, read the way C++ declarator syntax is read:
/// inside-out from the name.
#[derive(PartialEq, Eq, Debug)]
pub(super) enum Declares {
    /// A function: `area()`, `Point(int,int)`, `int *f()` (a function returning a
    /// pointer), `int (*table(int))(int)` (a function returning a function
    /// pointer). ⇒ a method prototype when it is a class member.
    Function,
    /// Data: `int *p`, `int& r`, and `void (*cb)(int)` — a POINTER to a function
    /// is data, not a function (issue #135). ⇒ a `Field`.
    Data,
    /// Only the bare name, with nothing said about it yet.
    Name,
}

/// Reads a declarator and reports whether the name it binds is a function or data.
///
/// Preconditions: `declarator` is a declaration's `declarator_field` child (or a
/// nested declarator). Postconditions: `Function` iff the outermost construct
/// applied to the name is a call signature; `Data` iff it is a pointer or
/// reference; `Name` for a bare identifier. Terminates because every recursion
/// descends to a strictly smaller subtree.
///
/// This replaces a whole-subtree "is there a `function_declarator` anywhere?" scan,
/// which could not tell `void cb(int z);` (a method) from `void (*cb)(int z);` (a
/// data member of function-pointer type) — both have a `function_declarator` as the
/// OUTERMOST declarator, and only what sits between it and the name differs
/// (issue #135):
///
///   `area()`              function_declarator{field_identifier}          → Function
///   `(*cb)(int)`          function_declarator{paren{pointer{name}}}      → Data
///   `int *f()`            pointer{function_declarator{name}}             → Function
///   `int (*table(int))()` function_declarator{paren{pointer{fn{name}}}}  → Function
///
/// The rule that produces all four: a function declarator asks what its OWN
/// declarator field declares — if that is a pointer, the name is a pointer TO a
/// function (data); otherwise the name IS the function.
/// source: tree-sitter-cpp 0.23.4 node-types.json (function_declarator,
/// pointer_declarator and array_declarator declare a required `declarator` field;
/// parenthesized_declarator and reference_declarator are fieldless).
pub(super) fn declarator_declares(cf: &CppFamilySpec, declarator: Node) -> Declares {
    let kind = declarator.kind();
    let inner = || {
        declarator
            .child_by_field_name(cf.naming.declarator_field)
            .or_else(|| first_declarator_child(cf, declarator))
    };
    if kind == cf.func_declarator_kind {
        return match inner().map(|d| declarator_declares(cf, d)) {
            // `(*cb)(int)`: the inner declarator makes the name a pointer, so the
            // whole thing is a pointer to a function — data.
            Some(Declares::Data) => Declares::Data,
            // `area()` (bare name) or `(*table(int))(int)` (inner is itself a
            // function): the name is a function.
            _ => Declares::Function,
        };
    }
    if kind_in(cf.pointer_declarator_kinds, kind) {
        return match inner().map(|d| declarator_declares(cf, d)) {
            // `int *f()` — a function returning a pointer is still a function.
            Some(Declares::Function) => Declares::Function,
            _ => Declares::Data,
        };
    }
    if kind_in(cf.grouping_declarator_kinds, kind) {
        return inner()
            .map(|d| declarator_declares(cf, d))
            .unwrap_or(Declares::Name);
    }
    Declares::Name
}

/// The first named child of a FIELDLESS declarator wrapper
/// (`parenthesized_declarator`, `reference_declarator`), skipping the parameter
/// list. Those two kinds carry no `declarator` field at all, so the chain is
/// followed through children instead.
fn first_declarator_child<'t>(cf: &CppFamilySpec, node: Node<'t>) -> Option<Node<'t>> {
    let params_id = node
        .child_by_field_name(cf.naming.parameters_field)
        .map(|p| p.id());
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|c| Some(c.id()) != params_id);
    found
}

/// Emits a namespace (`Struct`, `is_namespace=true`) + `Defines`, then recurses
/// its body as a NON-class scope (`enclosing_type = None`, so inner functions
/// stay `Function`s). An anonymous namespace emits no node and recurses its body
/// under the unchanged scope.
fn emit_namespace(spec: &LangSpec, cf: &CppFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = named_or_first_identifier(cf.naming, spec, ctx.source, node);
    if name.is_empty() {
        if let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) {
            walk_cpp_defs(spec, cf, ctx, body, scope, None);
        }
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_STRUCT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: vec![("is_namespace".to_string(), "true".to_string())],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) {
        walk_cpp_defs(spec, cf, ctx, body, &qn, None);
    }
}

/// Emits a class (`is_class=true`) or struct/union (no `is_class`) as a
/// `Struct` (with a `Defines` edge), its base classes as `Extends` edges, then
/// recurses its body as a CLASS scope (`enclosing_type = Some(qn)`, so members
/// become methods / data fields).
///
/// Preconditions: `node`'s kind is in `cf.class_kinds` or `cf.struct_kinds`.
/// Postconditions: one `Struct` + one `Defines` (plus one `Extends` per base and
/// whatever the body contains), or NOTHING when the specifier has no body or no
/// resolvable name.
///
/// The body-presence guard is the #107 lesson applied to C++ (boy-scout, §14):
/// `class_specifier` is the SAME node kind for a definition (`class Shape { … }`,
/// which has a `body`) and for a forward declaration (`class Shape;`, which does
/// not). Without it, `class Shape;` followed by the real definition emitted TWO
/// `Struct` nodes on the identical QN — a duplicated primary key whose span was
/// the one-line declaration. A forward declaration declares no type, so it
/// contributes no node; the definition (here or in a header) carries the truth.
/// Pinned by `cpp_forward_declaration_at_file_scope_emits_nothing`.
pub(super) fn emit_class_like(
    spec: &LangSpec,
    cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    is_class: bool,
) {
    let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) else {
        return;
    };
    let name = named_or_first_identifier(cf.naming, spec, ctx.source, node);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let mut props = Vec::new();
    if is_class {
        props.push(("is_class".to_string(), "true".to_string()));
    }
    ctx.nodes.push(ExtractedNode {
        label: LABEL_STRUCT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: props,
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    emit_bases(cf, ctx, node, &qn);
    walk_cpp_defs(spec, cf, ctx, body, &qn, Some(&qn));
}

/// Emits one `Extends` edge per base type in the node's `base_clause_kind`
/// child. The base clause is a DIRECT child of the class/struct node (it has no
/// `bases` field), and each base is a `base_type_kinds` child of that clause
/// (access specifiers and virtual/attribute tokens are skipped).
/// source: tree-sitter-cpp 0.23.4 (class_specifier children include
/// base_class_clause; base_class_clause has no `bases` field).
fn emit_bases(cf: &CppFamilySpec, ctx: &mut WalkCtx, node: Node, class_qn: &str) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != cf.base_clause_kind {
            continue;
        }
        let mut inner = child.walk();
        for base in child.children(&mut inner) {
            if !kind_in(cf.base_type_kinds, base.kind()) {
                continue;
            }
            let t = node_text(ctx.source, base).trim().to_string();
            if t.is_empty() {
                continue;
            }
            ctx.refs.push(ExtractedRef {
                kind: "Extends".to_string(),
                from_qualified_name: class_qn.to_string(),
                to_qualified_name: t,
            });
        }
    }
}

/// The owner an out-of-body definition belongs to: the `::`-joined
/// `qualified_scope_field` segments of a `qualified_declarator_kind` declarator
/// (`geometry::Circle::area` → `geometry::Circle`), or `None` when the declarator
/// is unqualified (issue #124 item 5).
///
/// Preconditions: `declarator` is a `func_def_kinds` node's `declarator_field`
/// child. Postconditions: `Some(path)` with `path` non-empty, or `None`; the
/// path never includes the member's own name — the recursion stops at the
/// innermost `name_field`, which is the member, not an owner.
///
/// The grammar nests one `qualified_identifier` per `::`, each carrying its own
/// `scope` (verified against node-types.json: `qualified_identifier` has exactly
/// `scope` + `name`, and `name` may itself be a `qualified_identifier`). The loop
/// terminates because each iteration descends to a strictly deeper `name_field`
/// child and the tree is finite.
fn owner_path(
    cf: &CppFamilySpec,
    spec: &LangSpec,
    source: &str,
    declarator: Node,
) -> Option<String> {
    let mut node = declarator.child_by_field_name(cf.naming.declarator_field)?;
    let mut segments: Vec<String> = Vec::new();
    while node.kind() == cf.qualified_declarator_kind {
        let scope_node = node.child_by_field_name(cf.qualified_scope_field)?;
        segments.push(node_text(source, scope_node).trim().to_string());
        match node.child_by_field_name(spec.name_field) {
            Some(inner) => node = inner,
            None => break,
        }
    }
    if segments.is_empty() {
        return None;
    }
    Some(segments.join("::"))
}

/// Emits a function definition: a `Function` + `Defines` at file/namespace
/// scope, a `Method` + `HasMethod` (with `receiver_type`) inside a class body,
/// and — for an owner-qualified declarator at file/namespace scope
/// (`double geometry::Circle::area() const { … }`) — a `Method` + `HasMethod`
/// re-attached to that owner (issue #124 item 5) instead of a file-scoped
/// `Function`.
///
/// Preconditions: `node`'s kind is in `cf.func_def_kinds`. Postconditions:
/// exactly one node is emitted with exactly one matching edge, unless the
/// declarator binds no name (then nothing is emitted and no `seq` is consumed);
/// the name is what the declarator binds, never a parameter (issue #123); the
/// body, when present, is scanned for calls attributed to the emitted QN.
/// Invariant: `ctx.next_seq` advances exactly once per emitted definition,
/// preserving the single per-file DFS ordering the QN contract depends on.
///
/// The owner path is resolved against the CURRENT scope: at file scope
/// `geometry::Circle` becomes `{file}::geometry::Circle`, and inside
/// `namespace geometry { double Circle::area() … }` the qualifier `Circle`
/// becomes `{file}::geometry::Circle` — the same QN the class emitted. A
/// definition whose owner is declared in ANOTHER file (a header) yields a QN no
/// node in this file carries; that is a dangling edge target for the resolver to
/// match by name, exactly as `Extends` targets already are.
fn emit_function(
    spec: &LangSpec,
    cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    enclosing_type: Option<&str>,
) {
    let Some(declarator) = node.child_by_field_name(cf.naming.declarator_field) else {
        return;
    };
    let name = declarator_name(cf.naming, ctx.source, declarator);
    if name.is_empty() {
        return;
    }
    // An out-of-body definition re-attaches to its qualified owner; inside a
    // class body the enclosing class already IS the owner.
    let qualified_owner = match enclosing_type {
        Some(_) => None,
        None => owner_path(cf, spec, ctx.source, declarator).map(|path| qual(scope, &path)),
    };
    let owner = enclosing_type.or(qualified_owner.as_deref());
    let seq = ctx.next_seq();
    let qn = spec.conventions.def_qn(owner.unwrap_or(scope), &name, seq);
    let mut props = Vec::new();
    if let Some(rec) = owner {
        props.push(("receiver_type".to_string(), rec.to_string()));
    }
    let (label, edge) = match owner {
        Some(_) => (LABEL_METHOD, "HasMethod"),
        None => (LABEL_FUNCTION, "Defines"),
    };
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: props,
    });
    ctx.refs.push(ExtractedRef {
        kind: edge.to_string(),
        from_qualified_name: owner.unwrap_or(scope).to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) {
        calls::walk_calls(spec, ctx, body, &qn);
    }
}
