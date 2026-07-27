// parser::spec::walkers::rust_types — the Rust TYPE-and-member emitters: structs
// and unions with their fields, enums with their variants, traits with their
// requirements, and `impl` blocks with their methods.
//
// Split out of `walkers/rust.rs` along the concern boundary (§4.1, 500-line cap):
// that file keeps the walker SPINE (dispatch, the derive-attribute accumulator,
// the shared `Def`/`push_def` emission, and the flat items — functions, consts,
// macro definitions, type aliases, modules); this one holds every emitter whose
// unit of work is a TYPE and the members hanging off it. Pure move — no logic
// change, and the parity pin (`rust_parity_tests`) is the proof.

use tree_sitter::Node;

use super::super::lang_spec::LangSpec;
use super::rust::{
    decl_list_body, emit_derive_implements, has_async, implements_props, push_def, Def,
    DeriveScope, RustSpecs,
};
use super::{call_scan_of, calls, kind_in, types, WalkCtx};
use crate::parser::{
    node_field_text, qual, ExtractedRef, LABEL_ENUM, LABEL_FIELD, LABEL_METHOD, LABEL_STRUCT,
    LABEL_TRAIT, LABEL_VARIANT,
};

/// The impl block a method is being emitted under: its receiver QN and the trait
/// it implements (empty for an inherent impl). A parameter object (§4.4).
struct ImplTarget<'a> {
    receiver_qn: &'a str,
    trait_name: &'a str,
}

/// Emits a struct or union (`Struct` + `Defines`), its named fields, then its
/// derive edges. A tuple or unit struct has no `field_declaration_list` body, so
/// it yields no fields.
pub(super) fn emit_struct(specs: RustSpecs, ctx: &mut WalkCtx, node: Node, ds: DeriveScope) {
    let spec = specs.spec;
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(ds.scope, &name);
    push_def(
        ctx,
        node,
        Def {
            label: LABEL_STRUCT,
            name: &name,
            qn: &qn,
            visibility: spec.conventions.node_visibility(ctx.source, node, &name),
            properties: implements_props(ds.derives),
            edge_kind: "Defines",
            edge_from: ds.scope,
        },
    );
    emit_fields(spec, ctx, node, &qn);
    emit_derive_implements(spec, ctx, node, ds);
}

/// Emits one `Field` + `HasField` per named member of the node's field-list body.
fn emit_fields(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, owner_qn: &str) {
    // mutation note (§12): replacing this match guard with `true` produces a
    // surviving mutant that is a proven EQUIVALENT, and the proof is the grammar,
    // not the corpus. `emit_fields` is reachable only from `emit_struct`, i.e. only
    // for `struct_item` / `union_item`. Their `body` field admits exactly
    // `field_declaration_list` and `ordered_field_declaration_list`
    // (`union_item`: only the former), and an `ordered_field_declaration_list` has
    // NO `field_declaration` children — its named children are `attribute_item`
    // and `visibility_modifier`. So the loop below emits nothing for the only body
    // kind the guard rejects, and no Rust source can observe its removal. The guard
    // is kept as the faithful copy of the pre-migration walker's check and as a
    // cheap barrier should a future caller pass some other node.
    // source: tree-sitter-rust 0.23.3 node-types.json (struct_item.body,
    //   union_item.body, ordered_field_declaration_list.children).
    let body = match spec.body_field.and_then(|f| node.child_by_field_name(f)) {
        Some(b) if kind_in(spec.field_container_kinds, b.kind()) => b,
        _ => return,
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if !kind_in(spec.field_node_kinds, child.kind()) {
            continue;
        }
        let name = node_field_text(ctx.source, child, spec.name_field);
        if name.is_empty() {
            continue;
        }
        let type_ann = node_field_text(ctx.source, child, spec.type_field);
        let fqn = qual(owner_qn, &name);
        push_def(
            ctx,
            child,
            Def {
                label: LABEL_FIELD,
                name: &name,
                qn: &fqn,
                visibility: spec.conventions.node_visibility(ctx.source, child, &name),
                properties: vec![("type_annotation".to_string(), type_ann)],
                edge_kind: "HasField",
                edge_from: owner_qn,
            },
        );
    }
}

/// Emits an enum (`Enum` + `Defines`), its variants, then its derive edges.
pub(super) fn emit_enum(specs: RustSpecs, ctx: &mut WalkCtx, node: Node, ds: DeriveScope) {
    let spec = specs.spec;
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(ds.scope, &name);
    push_def(
        ctx,
        node,
        Def {
            label: LABEL_ENUM,
            name: &name,
            qn: &qn,
            visibility: spec.conventions.node_visibility(ctx.source, node, &name),
            properties: implements_props(ds.derives),
            edge_kind: "Defines",
            edge_from: ds.scope,
        },
    );
    emit_variants(specs, ctx, node, &qn);
    emit_derive_implements(spec, ctx, node, ds);
}

/// Emits one `Variant` + `HasVariant` per member of the enum's variant-list body.
/// A variant carries no visibility and no properties, whatever its own shape
/// (unit, tuple, or struct-bodied) — the hand-written walker's model.
fn emit_variants(specs: RustSpecs, ctx: &mut WalkCtx, enum_node: Node, enum_qn: &str) {
    let spec = specs.spec;
    // mutation note (§12): as in `emit_fields`, replacing this match guard with
    // `true` yields a surviving EQUIVALENT mutant, proven from the grammar rather
    // than from the corpus: `emit_variants` is reachable only from `emit_enum`, and
    // `enum_item`'s `body` field admits exactly ONE kind, `enum_variant_list`. The
    // guard therefore can never be false when the body is present, so no Rust
    // source can observe its removal. Kept as the faithful copy of the
    // pre-migration walker's check.
    // source: tree-sitter-rust 0.23.3 node-types.json (enum_item.body).
    let body = match spec
        .body_field
        .and_then(|f| enum_node.child_by_field_name(f))
    {
        Some(b) if kind_in(specs.rf.variant_list_kinds, b.kind()) => b,
        _ => return,
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if !kind_in(specs.rf.variant_kinds, child.kind()) {
            continue;
        }
        let name = node_field_text(ctx.source, child, spec.name_field);
        if name.is_empty() {
            continue;
        }
        let vqn = qual(enum_qn, &name);
        push_def(
            ctx,
            child,
            Def {
                label: LABEL_VARIANT,
                name: &name,
                qn: &vqn,
                visibility: String::new(),
                properties: Vec::new(),
                edge_kind: "HasVariant",
                edge_from: enum_qn,
            },
        );
    }
}

/// Emits a trait (`Trait` + `Defines`), one `Extends` per supertrait, then its
/// requirements as `Method`s.
pub(super) fn emit_trait(specs: RustSpecs, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let spec = specs.spec;
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    // Supertraits are the `extends_field` (`bounds`) children of `base_node_kinds`
    // — exactly the shared `collect_bases` contract, so Rust rides it.
    let supers = types::collect_bases(spec, ctx.source, node);
    let mut props = Vec::new();
    if !supers.is_empty() {
        props.push(("supertraits".to_string(), supers.join(",")));
    }
    push_def(
        ctx,
        node,
        Def {
            label: LABEL_TRAIT,
            name: &name,
            qn: &qn,
            visibility: spec.conventions.node_visibility(ctx.source, node, &name),
            properties: props,
            edge_kind: "Defines",
            edge_from: scope,
        },
    );
    for sup in &supers {
        ctx.refs.push(ExtractedRef {
            kind: "Extends".to_string(),
            from_qualified_name: qn.clone(),
            to_qualified_name: sup.clone(),
        });
    }
    emit_trait_methods(specs, ctx, node, &qn);
}

/// Emits one `Method` + `HasMethod` per trait requirement (a bodiless
/// `function_signature_item` or a defaulted `function_item`), receiver-typed to
/// the trait, and scans a DEFAULT body for calls exactly as `emit_impl_method`
/// does (#131). A bodiless `function_signature_item` has no `body` field and
/// Rust's `function_body_kinds` is empty, so `call_scan_of` returns `None` for it
/// — a requirement without a default stays call-free, and a signature's parameter
/// defaults are never scanned. This is the same asymmetry Swift #100 (unscanned
/// computed-property getters) and TS #142 (unscanned object-literal bodies)
/// closed, in a different grammar.
fn emit_trait_methods(specs: RustSpecs, ctx: &mut WalkCtx, trait_node: Node, trait_qn: &str) {
    let (spec, rf) = (specs.spec, specs.rf);
    let body = match decl_list_body(specs, trait_node) {
        Some(b) => b,
        None => return,
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        let is_sig = kind_in(rf.function_signature_kinds, child.kind());
        if !is_sig && !kind_in(rf.function_kinds, child.kind()) {
            continue;
        }
        let name = node_field_text(ctx.source, child, spec.name_field);
        if name.is_empty() {
            continue;
        }
        let seq = ctx.next_seq();
        let mqn = spec.conventions.def_qn(trait_qn, &name, seq);
        // A bodiless requirement is reported non-async unconditionally; only a
        // defaulted `fn` has its modifiers read. The hand-written walker's split,
        // preserved.
        let is_async = !is_sig && has_async(rf, child);
        push_def(
            ctx,
            child,
            Def {
                label: LABEL_METHOD,
                name: &name,
                qn: &mqn,
                visibility: spec.conventions.node_visibility(ctx.source, child, &name),
                properties: vec![
                    ("is_async".to_string(), is_async.to_string()),
                    ("receiver_type".to_string(), trait_qn.to_string()),
                ],
                edge_kind: "HasMethod",
                edge_from: trait_qn,
            },
        );
        // A defaulted requirement (`function_item`) has a `body`, so its calls
        // reach the graph keyed by the method's own QN; a bodiless requirement
        // (`function_signature_item`) yields `None` here and stays call-free.
        if let Some(body) = call_scan_of(spec, child) {
            calls::walk_calls(spec, ctx, body, &mqn);
        }
    }
}

/// Walks an `impl` block. The block itself emits NO node: its methods attach to
/// `{scope}::{type_text}` — the QN of the enclosing scope (the module QN when the
/// `impl` sits inside a module, the file path at file level), matching the QN the
/// `Struct`/`Enum` node itself already uses (#130). The type text is verbatim
/// including generics (`Wrapper<T>`), so a nested generic impl composes to
/// `{module_qn}::Wrapper<T>`.
pub(super) fn emit_impl(specs: RustSpecs, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let (spec, rf) = (specs.spec, specs.rf);
    let impl_type = node_field_text(ctx.source, node, spec.type_field);
    if impl_type.is_empty() {
        return;
    }
    let trait_name = node_field_text(ctx.source, node, rf.trait_field);
    let receiver_qn = qual(scope, &impl_type);
    let body = match decl_list_body(specs, node) {
        Some(b) => b,
        None => return,
    };
    let target = ImplTarget {
        receiver_qn: &receiver_qn,
        trait_name: &trait_name,
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if !kind_in(rf.function_kinds, child.kind())
            && !kind_in(rf.function_signature_kinds, child.kind())
        {
            continue;
        }
        emit_impl_method(specs, ctx, child, &target);
    }
}

/// Emits one `impl` member as a `Method` + `HasMethod` receiver-typed to the
/// impl type, carrying `trait_name` for a trait impl, and scans its body for
/// calls (unlike a trait requirement).
fn emit_impl_method(specs: RustSpecs, ctx: &mut WalkCtx, node: Node, target: &ImplTarget) {
    let (spec, rf) = (specs.spec, specs.rf);
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let seq = ctx.next_seq();
    let mqn = spec.conventions.def_qn(target.receiver_qn, &name, seq);
    let mut props = vec![
        ("is_async".to_string(), has_async(rf, node).to_string()),
        ("receiver_type".to_string(), target.receiver_qn.to_string()),
    ];
    if !target.trait_name.is_empty() {
        props.push(("trait_name".to_string(), target.trait_name.to_string()));
    }
    push_def(
        ctx,
        node,
        Def {
            label: LABEL_METHOD,
            name: &name,
            qn: &mqn,
            visibility: spec.conventions.node_visibility(ctx.source, node, &name),
            properties: props,
            edge_kind: "HasMethod",
            edge_from: target.receiver_qn,
        },
    );
    if let Some(body) = call_scan_of(spec, node) {
        calls::walk_calls(spec, ctx, body, &mqn);
    }
}
