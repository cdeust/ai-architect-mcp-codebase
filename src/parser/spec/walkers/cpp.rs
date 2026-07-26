// parser::spec::walkers::cpp — the hybrid C-family class-model definition
// walker (ADR-0055 phase 7). Reproduces the pre-migration hand-written C++
// walker (`parser::cpp::extract`) at exact parity, driven by the
// `CppFamilySpec` sub-table instead of hardcoded `TS_*` constants.
//
// C++ is neither flat C (structs carry FIELDS, enums emit members) nor the pure
// class-model (`walk_defs` with `class_inheritance`/dedup): its hand-written
// walker is a single class-recursive DFS whose semantics match neither, and its
// ONE per-file `seq` counter keys functions, prototypes, AND call sites in one
// DFS order. That last property makes exact parity possible only by reproducing
// the DFS itself — so C++ gets this dedicated walker (the #109 precedent that
// added `clike` for C), and `walk_defs` delegates here whenever a `LangSpec`
// carries `cpp_family: Some(_)`. The six languages riding `walk_defs`/`clike`
// stay untouched. Calls and imports still route through the SHARED generic
// walkers (`calls::walk_calls`, `imports::walk_imports`) via the conventions —
// only the definition shapes are C++-specific.
//
// `enclosing_type` is `Some(class_qn)` while walking a class/struct/union body
// and `None` at file or namespace scope. It drives the two class-scope
// divergences: a `function_definition` becomes a `Method`/`HasMethod` (not a
// `Function`/`Defines`) inside a class, and a `field_declaration` member is only
// emitted inside a class (as a prototype `Method` or a data-member `Constant`).

use tree_sitter::Node;

use super::super::lang_spec::{CppFamilySpec, LangSpec};
use super::{calls, end_line_of, imports, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_CONSTANT, LABEL_ENUM,
    LABEL_FUNCTION, LABEL_METHOD, LABEL_STRUCT,
};

/// Hybrid C++ definition walker: dispatches each child of `parent` to the
/// concern its node kind names in `cf`, recursing into class/namespace bodies
/// and transparently through templates and any unmatched wrapper with named
/// children. The dispatch order matches the hand-written `extract_children`'s
/// `match` arms; all dispatched kinds are disjoint, so order among them is
/// safe and only the default recursion arm must stay last.
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
            emit_enum(spec, cf, ctx, child, scope);
        } else if kind_in(cf.template_kinds, k) {
            // Templates wrap a class/function; descend transparently, same scope
            // and enclosing type, emitting no node of their own.
            walk_cpp_defs(spec, cf, ctx, child, scope, enclosing_type);
        } else if kind_in(cf.func_def_kinds, k) {
            emit_function(spec, cf, ctx, child, scope, enclosing_type);
        } else if kind_in(cf.field_decl_kinds, k) {
            // A member declaration is only emitted inside a class/struct body:
            // a prototype `Method` when it carries a function declarator, else a
            // data-member `Constant`. At file/namespace scope the hand-written
            // walker ignored `field_declaration`s, so this arm is a no-op there.
            if enclosing_type.is_some() {
                if has_function_declarator(cf, child) {
                    emit_member_proto(spec, cf, ctx, child, scope);
                } else {
                    emit_member_field(spec, cf, ctx, child, scope);
                }
            }
        } else if kind_in(cf.typedef_kinds, k) {
            emit_typedef(spec, cf, ctx, child, scope);
        } else if kind_in(spec.import_node_kinds, k) {
            // `preproc_include` and `using_declaration`; the conventions shape
            // each (include vs using) into an `Import`.
            imports::walk_imports(spec, ctx, child, scope);
        } else if child.named_child_count() > 0 {
            // Transparent recursion into an unmatched wrapper with named children
            // (access specifiers, linkage specs, preprocessor conditionals, …),
            // same scope and enclosing type — the hand-written `_ =>` arm.
            // mutation note (§12): the `> 0` → `>= 0` mutant is EQUIVALENT —
            // recursing into a childless node emits nothing (the child loop
            // iterates zero times), so no test can observe a difference. Kept as
            // a faithful copy of the old walker's guard and a cheap leaf skip.
            walk_cpp_defs(spec, cf, ctx, child, scope, enclosing_type);
        }
    }
}

/// The first identifier leaf (`identifier_kinds`) in a right-to-left DFS of
/// `node`. Reproduces the hand-written `find_identifier`'s stack-DFS order
/// EXACTLY (children pushed left-to-right onto a LIFO stack, so the last child's
/// subtree is searched first). That order is load-bearing: for a declarator
/// `add(int a, int b)` it returns the LAST parameter name (`b`), not `add` — a
/// pre-existing naming defect the migration preserves for parity (issue #123,
/// the C++ analog of the C fix #106). Not "the name" — "the name the old walker
/// produced".
fn find_identifier(cf: &CppFamilySpec, source: &str, node: Node) -> String {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if kind_in(cf.identifier_kinds, n.kind()) {
            return node_text(source, n);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    String::new()
}

/// Whether `node` carries a function declarator anywhere in its subtree — the
/// test that distinguishes a member `field_declaration` that is a method
/// prototype (`int f() const;`) from a data member (`int x;`). DFS over the
/// whole node, matching the hand-written `has_function_declarator`.
fn has_function_declarator(cf: &CppFamilySpec, node: Node) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == cf.func_declarator_kind {
            return true;
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    false
}

/// The declaration's name: the `name_field` text, or — when absent — the first
/// identifier leaf (LIFO DFS). Matches the hand-written
/// `node_field_text(node,"name")`-then-`find_identifier` fallback used for
/// namespaces, classes, structs, and enums.
fn named_or_first_identifier(
    cf: &CppFamilySpec,
    spec: &LangSpec,
    source: &str,
    node: Node,
) -> String {
    let field = node_field_text(source, node, spec.name_field);
    if field.is_empty() {
        find_identifier(cf, source, node)
    } else {
        field
    }
}

/// Emits a namespace (`Struct`, `is_namespace=true`) + `Defines`, then recurses
/// its body as a NON-class scope (`enclosing_type = None`, so inner functions
/// stay `Function`s). An anonymous namespace emits no node and recurses its body
/// under the unchanged scope — matching the hand-written `extract_namespace`.
fn emit_namespace(spec: &LangSpec, cf: &CppFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = named_or_first_identifier(cf, spec, ctx.source, node);
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
/// become methods / data members). Matches the hand-written `extract_class_like`.
fn emit_class_like(
    spec: &LangSpec,
    cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    is_class: bool,
) {
    let name = named_or_first_identifier(cf, spec, ctx.source, node);
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
    if let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) {
        walk_cpp_defs(spec, cf, ctx, body, &qn, Some(&qn));
    }
}

/// Emits one `Extends` edge per base type in the node's `base_clause_kind`
/// child. The base clause is a DIRECT child of the class/struct node (it has no
/// `bases` field), and each base is a `base_type_kinds` child of that clause
/// (access specifiers and virtual/attribute tokens are skipped). Matches the
/// hand-written base-class loop.
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

/// Emits an enum (`Enum` + `Defines`) with NO body recursion — the hand-written
/// walker never emitted enum members, and `enum class` shares `enum_specifier`,
/// so both surface as a bare `Enum`. Preserved for parity.
fn emit_enum(spec: &LangSpec, cf: &CppFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = named_or_first_identifier(cf, spec, ctx.source, node);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_ENUM.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

/// Emits a function definition: a `Function` + `Defines` at file/namespace
/// scope, or a `Method` + `HasMethod` (with `receiver_type`) inside a class
/// body. The name is the first identifier leaf of the `declarator_field` child;
/// the QN is `{scope}::{name}#{seq}`; the body is scanned for calls via the
/// shared generic call walker. Matches the hand-written `extract_function`.
fn emit_function(
    spec: &LangSpec,
    cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    enclosing_type: Option<&str>,
) {
    let name = node
        .child_by_field_name(cf.declarator_field)
        .map(|d| find_identifier(cf, ctx.source, d))
        .unwrap_or_default();
    if name.is_empty() {
        return;
    }
    let seq = ctx.next_seq();
    let qn = spec.conventions.def_qn(scope, &name, seq);
    let mut props = Vec::new();
    if let Some(rec) = enclosing_type {
        props.push(("receiver_type".to_string(), rec.to_string()));
    }
    let (label, edge) = match enclosing_type {
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
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) {
        calls::walk_calls(spec, ctx, body, &qn);
    }
}

/// Emits a member function prototype (`Method`, `is_prototype=true`, receiver =
/// enclosing scope) + `HasMethod`, `{scope}::{name}#{seq}`. No body ⇒ no calls.
/// The name is the first identifier leaf of the WHOLE `field_declaration`
/// (LIFO DFS — lands on the last parameter name when parameters are named, e.g.
/// `operator+(const Shape& other)` → `other`). Matches `extract_member_proto`.
fn emit_member_proto(
    spec: &LangSpec,
    cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
) {
    let name = find_identifier(cf, ctx.source, node);
    if name.is_empty() {
        return;
    }
    let seq = ctx.next_seq();
    let qn = spec.conventions.def_qn(scope, &name, seq);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: vec![
            ("is_prototype".to_string(), "true".to_string()),
            ("receiver_type".to_string(), scope.to_string()),
        ],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

/// Emits a data member (`Constant` + `Defines`, `{scope}::{name}`). The name is
/// the first identifier leaf of the whole `field_declaration` (LIFO DFS).
/// Matches `extract_member_field`. Note: the graph models a C++ data member as a
/// `Constant`, NOT a `Field`/`HasField` (the flat C-family model) — a
/// hand-written divergence preserved for parity.
fn emit_member_field(
    spec: &LangSpec,
    cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
) {
    let name = find_identifier(cf, ctx.source, node);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_CONSTANT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

/// Emits a typedef as a `Constant` (`typedef=true`) + `Defines`. The name is the
/// first identifier leaf of the whole `type_definition` (LIFO DFS lands on the
/// declared alias, which follows the aliased type in child order). Matches
/// `extract_typedef`.
fn emit_typedef(spec: &LangSpec, cf: &CppFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = find_identifier(cf, ctx.source, node);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_CONSTANT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: vec![("typedef".to_string(), "true".to_string())],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}
