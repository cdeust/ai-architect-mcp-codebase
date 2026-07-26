// parser::spec::walkers::constants — the constant/variant/field emitters.
//
// Split out of `walkers/mod.rs` (issue #101, §4.1) as a pure move; the
// `ExtractedNode`/`ExtractedRef` contract is unchanged.

use tree_sitter::Node;

use super::super::lang_spec::LangSpec;
use super::{end_line_of, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_CONSTANT, LABEL_VARIANT,
};

/// Emits each `Constant` + `Defines` a `member_constant_kinds` node binds,
/// under the current scope, name/visibility/properties shaped by the conventions
/// (Kotlin `enum_entry` → one; a `property_declaration` `val`/`var` → one, a
/// destructuring `val (a, b)` → several). A member yielding no `MemberConstant`
/// (empty/malformed name) emits nothing. Every emitted node shares the property
/// node's own line span (Kotlin binds all destructured names on one statement).
pub(super) fn emit_member_constant(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let start_line = line_of(node);
    let end_line = end_line_of(node);
    for mc in spec.conventions.member_constants(ctx.source, node) {
        // mutation note (§12): the `mc.name.is_empty()` skip guard is a defensive
        // generic-walker invariant; its removal is EQUIVALENT for the migrated
        // set, because the sole `member_constants` impl (Kotlin) already filters
        // empty names out of its `Vec`. The guard remains as the walker's own
        // gate for any future language whose `member_constants` yields an empty
        // name.
        if mc.name.is_empty() {
            continue;
        }
        let qn = qual(scope, &mc.name);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_CONSTANT.to_string(),
            name: mc.name,
            qualified_name: qn.clone(),
            start_line,
            end_line,
            visibility: mc.visibility,
            properties: mc.properties,
        });
        ctx.refs.push(ExtractedRef {
            kind: "Defines".to_string(),
            from_qualified_name: scope.to_string(),
            to_qualified_name: qn,
        });
    }
}

/// Emits one `Variant` per name a member declaration carries, under the
/// enclosing enum's scope. The edge kind and visibility come from the
/// conventions (Java `enum_constant` → `HasVariant`/`public`; Swift `enum_entry`
/// → `Defines`/`internal`). A single declaration may bind several names
/// (Swift `case green, blue` is ONE `enum_entry` node with two `name` fields),
/// so every `name_field` child is emitted — a grammar with a single-name enum
/// member (Java) yields exactly one, unchanged. Each variant carries the
/// declaration node's line span (matching both hand-written walkers), not the
/// individual name's.
pub(super) fn emit_variant(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    // mutation note (§12): collecting ALL `name_field` children (rather than the
    // first via `node_field_text`) is a strict generalization — Java's
    // single-name `enum_constant` still yields one variant, so the Java parity
    // corpus is unchanged; Swift's `case green, blue` yields two, killing the
    // "first-name-only" regression (asserted by the Swift parity corpus).
    let mut names: Vec<String> = Vec::new();
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            if cursor.field_name() == Some(spec.name_field) {
                let t = node_text(ctx.source, cursor.node());
                if !t.is_empty() {
                    names.push(t);
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    let visibility = spec.conventions.variant_visibility();
    let edge_kind = spec.conventions.variant_edge_kind();
    for name in names {
        let qn = qual(scope, &name);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_VARIANT.to_string(),
            name: name.clone(),
            qualified_name: qn.clone(),
            start_line: line_of(node),
            end_line: end_line_of(node),
            visibility: visibility.clone(),
            properties: Vec::new(),
        });
        ctx.refs.push(ExtractedRef {
            kind: edge_kind.to_string(),
            from_qualified_name: scope.to_string(),
            to_qualified_name: qn,
        });
    }
}

/// Emits one `Constant` + `Defines` per declared name in a member-field
/// declaration (Java `field_declaration`). A single declaration may declare
/// several names (`int x, y;`), so every `variable_declarator_kind` child is a
/// constant. Visibility comes from the declaration node's modifiers (shared by
/// every declarator); line spans come from each declarator.
pub(super) fn emit_variable_fields(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let declarator_kind = match spec.variable_declarator_kind {
        Some(k) => k,
        None => return,
    };
    let visibility = spec.conventions.node_visibility(ctx.source, node, "");
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != declarator_kind {
            continue;
        }
        let name = node_field_text(ctx.source, child, spec.name_field);
        if name.is_empty() {
            continue;
        }
        let qn = qual(scope, &name);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_CONSTANT.to_string(),
            name: name.clone(),
            qualified_name: qn.clone(),
            start_line: line_of(child),
            end_line: end_line_of(child),
            visibility: visibility.clone(),
            properties: Vec::new(),
        });
        ctx.refs.push(ExtractedRef {
            kind: "Defines".to_string(),
            from_qualified_name: scope.to_string(),
            to_qualified_name: qn,
        });
    }
}

/// Emits one `Constant` node + `Defines` edge per value-declaration name the
/// conventions accept (`is_constant_name`). Stack DFS mirrors the hand-written
/// walker so grouped `const ( ... )` blocks are fully descended.
pub(super) fn walk_value_decl(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if kind_in(spec.value_spec_node_kinds, n.kind()) {
            emit_value_spec(spec, ctx, n, scope);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

/// Emits the constants declared by one value-spec node. Two shapes, selected
/// by `spec.value_name_field`. `Some(field)` (Python): the single name is the
/// `field` child, and the node's line spans come from the value-spec node
/// itself. `None` (Go): every `value_name_kind` child is a name, and its own
/// line spans are used (a grouped `const (…)` block has one line per spec).
/// Both shapes funnel through the SAME constant guard, so a language with a
/// real `is_constant_name` filter (Python's `UPPER_SNAKE`) makes the guard
/// observably load-bearing — see the §12 note below.
fn emit_value_spec(spec: &LangSpec, ctx: &mut WalkCtx, n: Node, scope: &str) {
    let candidates: Vec<(String, Node)> = match spec.value_name_field {
        Some(field) => match n.child_by_field_name(field) {
            // mutation note (§12): the `nn.kind() == value_name_kind` guard →
            // `true` mutant is EQUIVALENT for the migrated languages. It only
            // matters when the value name field holds a NON-identifier node
            // (attribute `a.B`, subscript `a[B]`, tuple `A, B`), and every such
            // node's text carries punctuation (`.`/`[`/`,`) that the downstream
            // `is_constant_name` (`is_upper_snake_case`) filter rejects — so
            // relaxing the guard emits nothing extra. The guard is kept as a
            // precise, self-documenting restriction to the identifier case.
            Some(nn) if nn.kind() == spec.value_name_kind => {
                vec![(node_text(ctx.source, nn), n)]
            }
            _ => Vec::new(),
        },
        None => {
            let mut v = Vec::new();
            let mut cursor = n.walk();
            for child in n.children(&mut cursor) {
                if child.kind() == spec.value_name_kind {
                    v.push((node_text(ctx.source, child), child));
                }
            }
            v
        }
    };

    for (name, line_node) in candidates {
        // §12: the `||` → `&&` mutant on this guard was EQUIVALENT under Go
        // (Go's `is_constant_name` is constant-true and `value_name_kind`
        // nodes are never empty, so both operands are always false). Python's
        // `is_upper_snake_case` filter returns false for a lowercase module
        // assignment, making the two operators observably different: under
        // `||` a lowercase name is correctly skipped; under `&&` it would be
        // emitted as a spurious `Constant`. A Python fixture with a lowercase
        // module assignment (asserted absent) now KILLS this mutant.
        if name.is_empty() || !spec.conventions.is_constant_name(&name) {
            continue;
        }
        let props = match spec.value_type_field {
            Some(tf) => vec![(
                "type_annotation".to_string(),
                node_field_text(ctx.source, n, tf),
            )],
            None => Vec::new(),
        };
        let qn = qual(scope, &name);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_CONSTANT.to_string(),
            name: name.clone(),
            qualified_name: qn.clone(),
            start_line: line_of(line_node),
            end_line: end_line_of(line_node),
            visibility: spec.conventions.constant_visibility(&name),
            properties: props,
        });
        ctx.refs.push(ExtractedRef {
            kind: "Defines".to_string(),
            from_qualified_name: scope.to_string(),
            to_qualified_name: qn,
        });
    }
}
