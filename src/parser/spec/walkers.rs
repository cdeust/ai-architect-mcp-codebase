// parser::spec::walkers — the generic tree-sitter walkers (ADR-0055 §1).
//
// `walk_defs` / `walk_calls` / `walk_imports` / `walk_embedded` consume a
// `&LangSpec` (node-kind data) and a `&dyn LanguageConventions` (behavior) and
// produce the EXISTING, unchanged `ParseResult` / `ExtractedNode` /
// `ExtractedRef` contract. One implementation each, replacing the per-language
// hand-written walkers one language at a time behind the accuracy gate.
//
// The walker skeleton spans the structural patterns of the migrated languages,
// each pattern gated by (empty-for-non-applicable) spec data:
//   - Go   uses `method_node_kinds` (receiver-scoped), `type_decl_node_kinds`
//          (struct/interface/alias with fields), and a multi-name DFS constant
//          path. It leaves `class_node_kinds` / `decorated_def_kinds` empty.
//   - Python uses `class_node_kinds` (recurse into the body; in-class functions
//          become methods by enclosing scope), `decorated_def_kinds`, and a
//          field-based constant path. It leaves `method_node_kinds` /
//          `type_decl_node_kinds` empty.
// A node kind belongs to exactly one slice, so the dispatch is unambiguous and
// a language selects the arms it needs by populating (or emptying) its slices —
// OCP: adding a language is data, not a new walker (ADR-0055 §1.2).

use std::collections::HashSet;

use tree_sitter::{Node, Parser};

use super::lang_spec::LangSpec;
use crate::parser::{
    collect_error_ranges, count_parse_errors, node_field_text, node_text, parse_with_timeout, qual,
    ExtractedNode, ExtractedRef, ParseResult, LABEL_CALL_SITE, LABEL_CONSTANT, LABEL_ENUM,
    LABEL_FIELD, LABEL_FUNCTION, LABEL_IMPORT, LABEL_METHOD, LABEL_STRUCT, LABEL_TRAIT,
    LABEL_TYPE_ALIAS, LABEL_VARIANT,
};

/// Mutable state threaded through a single file's walk. `next_seq` is the
/// per-file monotonic counter the conventions use to disambiguate overloads
/// (Go's `#seq` suffix) and to key call sites. `emitted_qns` backs the
/// def-QN collision dedup (Python's `@property`/`@setter` case); it is inert
/// for languages whose `def_qn` is already unique (Go's `#seq`).
pub(crate) struct WalkCtx<'a> {
    source: &'a str,
    nodes: Vec<ExtractedNode>,
    refs: Vec<ExtractedRef>,
    next_seq: u64,
    emitted_qns: HashSet<String>,
}

impl WalkCtx<'_> {
    fn next_seq(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }

    /// Returns a unique QN: the input if unseen, else `qn@{start_line}` so every
    /// definition has a unique primary key while preserving the readable name
    /// for resolver name-based lookups. A no-op for `def_qn`s that are already
    /// unique (Go appends `#seq`), the mechanism for Python overload pairs.
    fn dedup(&mut self, qn: String, start_line: u64) -> String {
        if self.emitted_qns.insert(qn.clone()) {
            return qn;
        }
        let unique = format!("{qn}@{start_line}");
        self.emitted_qns.insert(unique.clone());
        unique
    }
}

/// Parses `source` with `spec`'s grammar and extracts the uniform
/// `ParseResult`. This is the table-driven replacement for a language's
/// hand-written `parse_<lang>_file` entry point.
///
/// Preconditions: `spec.ts_language` is the grammar matching `source`'s
/// language; `file_path` is the file's repo-relative id (the top scope).
/// Postconditions: returns `Ok(ParseResult)` whose `nodes`/`refs` are exactly
/// what the generic walkers emit for `source`, plus the shared parse-error
/// signals; `Err` only on grammar-set or parse-timeout failure. Invariant:
/// the public `ExtractedNode`/`ExtractedRef` contract is unchanged.
pub(crate) fn parse_with_spec(
    spec: &LangSpec,
    source: &str,
    file_path: &str,
) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = (spec.ts_language)();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set {:?} language: {e}", spec.language))?;
    let tree = parse_with_timeout(&mut parser, source)?;

    let mut ctx = WalkCtx {
        source,
        nodes: Vec::new(),
        refs: Vec::new(),
        next_seq: 0,
        emitted_qns: HashSet::new(),
    };
    walk_defs(spec, &mut ctx, tree.root_node(), file_path, None);
    Ok(ParseResult {
        nodes: ctx.nodes,
        refs: ctx.refs,
        parse_errors: count_parse_errors(tree.root_node()),
        error_ranges: collect_error_ranges(tree.root_node()),
    })
}

fn kind_in(kinds: &[&str], k: &str) -> bool {
    kinds.contains(&k)
}

/// First direct child of `node` whose kind is in `kinds`, or `None`. The
/// cursor temporary is bound before the return so it drops before the cursor
/// (E0597), matching the hand-written parsers' `child_of_kind` helper.
fn first_child_in<'t>(node: Node<'t>, kinds: &[&str]) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| kind_in(kinds, c.kind()));
    found
}

/// The class-like body to recurse into: the first `class_body_kinds` child when
/// the grammar exposes bodies as child nodes (Kotlin), else the `body_field`
/// child (Go/Python/Java). `None` when neither is present (bodiless class).
fn class_body_of<'t>(spec: &LangSpec, node: Node<'t>) -> Option<Node<'t>> {
    if !spec.class_body_kinds.is_empty() {
        return first_child_in(node, spec.class_body_kinds);
    }
    spec.body_field.and_then(|f| node.child_by_field_name(f))
}

/// The node to scan for calls in a function/method: the first
/// `function_body_kinds` child (falling back to the whole declaration node so an
/// expression-bodied `fun f() = g()` still yields its call) when the grammar
/// exposes bodies as child nodes (Kotlin), else the `body_field` child
/// (Go/Python/Java — `None` for a bodiless abstract method, which scans
/// nothing).
fn call_scan_of<'t>(spec: &LangSpec, node: Node<'t>) -> Option<Node<'t>> {
    if !spec.function_body_kinds.is_empty() {
        return first_child_in(node, spec.function_body_kinds).or(Some(node));
    }
    spec.body_field.and_then(|f| node.child_by_field_name(f))
}

fn line_of(node: Node) -> u64 {
    node.start_position().row as u64 + 1
}

fn end_line_of(node: Node) -> u64 {
    node.end_position().row as u64 + 1
}

/// Top-level definition walker: dispatches each child of `parent` to the
/// concern its node kind names in `spec`, then re-parses any embedded regions.
/// `enclosing_class` is `Some(class_qn)` while walking a class body — a
/// free-function node inside a class body is emitted as a method (Python), and
/// module-level value declarations are skipped inside a class.
pub(crate) fn walk_defs(
    spec: &LangSpec,
    ctx: &mut WalkCtx,
    parent: Node,
    scope: &str,
    enclosing_class: Option<&str>,
) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        let k = child.kind();
        if kind_in(spec.skip_node_kinds, k) {
            continue;
        } else if kind_in(spec.import_node_kinds, k) {
            walk_imports(spec, ctx, child, scope);
        } else if let Some(label) = class_like_label(spec, k) {
            emit_class(spec, ctx, child, scope, label);
        } else if kind_in(spec.decorated_def_kinds, k) {
            emit_decorated(spec, ctx, child, scope, enclosing_class);
        } else if kind_in(spec.type_decl_node_kinds, k) {
            walk_type_decl(spec, ctx, child, scope);
        } else if kind_in(spec.function_node_kinds, k) {
            emit_def(spec, ctx, child, scope, enclosing_class, &[]);
        } else if kind_in(spec.method_node_kinds, k) {
            emit_method_recv(spec, ctx, child, scope);
        } else if kind_in(spec.variant_node_kinds, k) {
            emit_variant(spec, ctx, child, scope);
        } else if kind_in(spec.member_constant_kinds, k) {
            emit_member_constant(spec, ctx, child, scope);
        } else if kind_in(spec.variable_field_kinds, k) {
            emit_variable_fields(spec, ctx, child, scope);
        } else if kind_in(spec.body_wrapper_kinds, k) {
            // A grammar wrapper around further members (Java's
            // `enum_body_declarations`): recurse transparently, same scope and
            // enclosing type, emitting no node of its own.
            walk_defs(spec, ctx, child, scope, enclosing_class);
        } else if kind_in(spec.value_decl_node_kinds, k) && enclosing_class.is_none() {
            walk_value_decl(spec, ctx, child, scope);
        }
    }
    walk_embedded(spec, ctx, parent, scope);
}

/// Maps a class-like node kind to the label it emits, or `None` if the kind is
/// not a class-like. All three share the same emitter (`emit_class`:
/// inheritance + body recursion); only the label differs. Struct/Trait/Enum
/// are disjoint slices, so at most one arm matches.
fn class_like_label(spec: &LangSpec, k: &str) -> Option<&'static str> {
    if kind_in(spec.class_node_kinds, k) {
        Some(LABEL_STRUCT)
    } else if kind_in(spec.interface_node_kinds, k) {
        Some(LABEL_TRAIT)
    } else if kind_in(spec.enum_node_kinds, k) {
        Some(LABEL_ENUM)
    } else {
        None
    }
}

/// Emits a free function (`Function` + `Defines`) or, inside a class body, a
/// method (`Method` + `HasMethod` with the class as receiver). `decorators`
/// are the already-stripped decorator names from an enclosing decorated def.
fn emit_def(
    spec: &LangSpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    enclosing_class: Option<&str>,
    decorators: &[String],
) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let start_line = line_of(node);
    let seq = ctx.next_seq();
    let base_qn = spec.conventions.def_qn(scope, &name, seq);
    let qn = ctx.dedup(base_qn, start_line);

    let mut props = spec.conventions.function_props(ctx.source, node);
    if !decorators.is_empty() {
        props.push(("decorators".to_string(), decorators.join(",")));
    }

    match enclosing_class {
        Some(class_qn) => {
            props.push(("receiver_type".to_string(), class_qn.to_string()));
            ctx.nodes.push(ExtractedNode {
                label: LABEL_METHOD.to_string(),
                name: name.clone(),
                qualified_name: qn.clone(),
                start_line,
                end_line: end_line_of(node),
                visibility: spec.conventions.node_visibility(ctx.source, node, &name),
                properties: props,
            });
            ctx.refs.push(ExtractedRef {
                kind: "HasMethod".to_string(),
                from_qualified_name: scope.to_string(),
                to_qualified_name: qn.clone(),
            });
        }
        None => {
            ctx.nodes.push(ExtractedNode {
                label: LABEL_FUNCTION.to_string(),
                name: name.clone(),
                qualified_name: qn.clone(),
                start_line,
                end_line: end_line_of(node),
                visibility: spec.conventions.node_visibility(ctx.source, node, &name),
                properties: props,
            });
            ctx.refs.push(ExtractedRef {
                kind: "Defines".to_string(),
                from_qualified_name: scope.to_string(),
                to_qualified_name: qn.clone(),
            });
        }
    }

    if let Some(body) = call_scan_of(spec, node) {
        walk_calls(spec, ctx, body, &qn);
    }
}

/// Emits a receiver-scoped method (`Method` + `HasMethod`) for languages whose
/// methods are a distinct node kind carrying a receiver field (Go). The
/// receiver type is parsed from `spec.receiver_field` and scopes the method's
/// QN under `scope::RecvType`.
fn emit_method_recv(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let receiver_text = spec
        .receiver_field
        .map(|rf| node_field_text(ctx.source, node, rf))
        .unwrap_or_default();
    let recv_type = spec.conventions.receiver_type(&receiver_text);
    let scope_qn = if recv_type.is_empty() {
        scope.to_string()
    } else {
        qual(scope, &recv_type)
    };
    let seq = ctx.next_seq();
    let qn = spec.conventions.def_qn(&scope_qn, &name, seq);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.node_visibility(ctx.source, node, &name),
        properties: vec![("receiver_type".to_string(), recv_type)],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: scope_qn.clone(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = call_scan_of(spec, node) {
        walk_calls(spec, ctx, body, &qn);
    }
}

/// Emits a class-like declaration (`label` + `Defines`), its inheritance
/// properties and edges (via the conventions — `bases`/`Extends` for Python,
/// plus `implements`/`Implements` for Java), then recurses into its body with
/// the class as the enclosing scope so member functions become methods.
/// `label` is `Struct`/`Trait`/`Enum` as selected by `class_like_label`.
fn emit_class(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str, label: &'static str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    // Grammars that use one node kind (`class_declaration`) for struct/trait/enum
    // (Kotlin) refine the label by content here; distinct-kind grammars
    // (Go/Python/Java) keep the label `class_like_label` already chose.
    let label = spec.conventions.refine_class_label(ctx.source, node, label);
    let qn = qual(scope, &name);
    let inheritance = spec.conventions.class_inheritance(ctx.source, spec, node);
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.node_visibility(ctx.source, node, &name),
        properties: inheritance.properties,
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    for (kind, to) in inheritance.refs {
        ctx.refs.push(ExtractedRef {
            kind: kind.to_string(),
            from_qualified_name: qn.clone(),
            to_qualified_name: to,
        });
    }
    if let Some(body) = class_body_of(spec, node) {
        walk_defs(spec, ctx, body, &qn, Some(&qn));
    }
}

/// Emits one `member_constant_kinds` node as a `Constant` + `Defines` under the
/// current scope, its name/visibility/properties shaped by the conventions
/// (Kotlin `enum_entry`). A `None` from the conventions (empty name) is skipped.
fn emit_member_constant(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    // mutation note (§12): the `!mc.name.is_empty()` match guard is a defensive
    // generic-walker invariant; its `→ true` mutant is EQUIVALENT for the
    // migrated set, because the sole `member_constant` impl (Kotlin) already
    // returns `None` for an empty name, so a `Some(mc)` here always carries a
    // non-empty name. The guard remains as the walker's own gate for any future
    // language whose `member_constant` returns `Some` with an empty name.
    let mc = match spec.conventions.member_constant(ctx.source, node) {
        Some(mc) if !mc.name.is_empty() => mc,
        _ => return,
    };
    let qn = qual(scope, &mc.name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_CONSTANT.to_string(),
        name: mc.name,
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: mc.visibility,
        properties: mc.properties,
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

/// Emits one enum member as a `Variant` + `HasVariant` under the enclosing
/// enum's scope (Java `enum_constant`). Enum constants are implicitly public.
fn emit_variant(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_VARIANT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: "public".to_string(),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasVariant".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

/// Emits one `Constant` + `Defines` per declared name in a member-field
/// declaration (Java `field_declaration`). A single declaration may declare
/// several names (`int x, y;`), so every `variable_declarator_kind` child is a
/// constant. Visibility comes from the declaration node's modifiers (shared by
/// every declarator); line spans come from each declarator.
fn emit_variable_fields(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
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

/// Base-class names: the `base_node_kinds` children of the class's
/// `extends_field`, verbatim (attribute access like `typing.NamedTuple` is
/// preserved; the resolver looks up by the last segment). Empty when the
/// language has no `extends_field`. Called by the default
/// `LanguageConventions::class_inheritance` (Python); Java overrides that
/// method and does not use this.
pub(super) fn collect_bases(spec: &LangSpec, source: &str, class_node: Node) -> Vec<String> {
    let field = match spec.extends_field {
        Some(f) => f,
        None => return Vec::new(),
    };
    let superclasses = match class_node.child_by_field_name(field) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut names = Vec::new();
    let mut cursor = superclasses.walk();
    for child in superclasses.children(&mut cursor) {
        if kind_in(spec.base_node_kinds, child.kind()) {
            let text = node_text(source, child);
            if !text.is_empty() {
                names.push(text);
            }
        }
    }
    names
}

/// Unwraps a decorated definition: collects the decorator names (stripped of
/// the leading `@`), then dispatches the inner class or function. A decorated
/// class is extracted as a plain class (decorators dropped, matching the
/// pre-migration walker); a decorated function carries its decorators as a
/// property.
fn emit_decorated(
    spec: &LangSpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    enclosing_class: Option<&str>,
) {
    let mut decorators: Vec<String> = Vec::new();
    let mut definition: Option<Node> = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let k = child.kind();
        if spec.decorator_node_kind == Some(k) {
            let text = node_text(ctx.source, child);
            decorators.push(text.trim_start_matches('@').trim().to_string());
        } else if kind_in(spec.class_node_kinds, k) {
            // A decorated class is a plain `Struct` (decorators dropped,
            // matching the pre-migration walker). Python is the only decorated
            // language; its decorated defs are always `class_definition`.
            emit_class(spec, ctx, child, scope, LABEL_STRUCT);
            return;
        } else if kind_in(spec.function_node_kinds, k) {
            definition = Some(child);
        }
    }
    if let Some(func_node) = definition {
        emit_def(spec, ctx, func_node, scope, enclosing_class, &decorators);
    }
}

fn walk_type_decl(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if kind_in(spec.type_spec_node_kinds, child.kind()) {
            emit_type_spec(spec, ctx, child, scope);
        }
    }
}

fn emit_type_spec(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let mut label = LABEL_TYPE_ALIAS;
    let mut struct_type: Option<Node> = None;
    if let Some(ty) = node.child_by_field_name(spec.type_field) {
        if spec.struct_type_kind == Some(ty.kind()) {
            label = LABEL_STRUCT;
            struct_type = Some(ty);
        } else if spec.interface_type_kind == Some(ty.kind()) {
            label = LABEL_TRAIT;
        }
    }
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
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
        to_qualified_name: qn.clone(),
    });
    if let Some(st) = struct_type {
        walk_fields(spec, ctx, st, &qn);
    }
}

/// Emits one `Field` node + `HasField` edge per declared name inside a struct.
/// A field declaration may declare several names (`X, Y int`), so every child
/// carrying the `name` field is emitted — matching the grammar's `multiple`
/// name field.
fn walk_fields(spec: &LangSpec, ctx: &mut WalkCtx, container_parent: Node, owner_qn: &str) {
    let mut c1 = container_parent.walk();
    for container in container_parent.children(&mut c1) {
        if !kind_in(spec.field_container_kinds, container.kind()) {
            continue;
        }
        let mut c2 = container.walk();
        for fd in container.children(&mut c2) {
            if !kind_in(spec.field_node_kinds, fd.kind()) {
                continue;
            }
            let type_text = node_field_text(ctx.source, fd, spec.type_field);
            let mut names: Vec<Node> = Vec::new();
            let mut c3 = fd.walk();
            if c3.goto_first_child() {
                loop {
                    if c3.field_name() == Some(spec.name_field) {
                        names.push(c3.node());
                    }
                    if !c3.goto_next_sibling() {
                        break;
                    }
                }
            }
            for name_node in names {
                let fname = node_text(ctx.source, name_node);
                if fname.is_empty() {
                    continue;
                }
                let fqn = qual(owner_qn, &fname);
                let mut props = Vec::new();
                if !type_text.is_empty() {
                    props.push(("type_annotation".to_string(), type_text.clone()));
                }
                ctx.nodes.push(ExtractedNode {
                    label: LABEL_FIELD.to_string(),
                    name: fname.clone(),
                    qualified_name: fqn.clone(),
                    start_line: line_of(fd),
                    end_line: end_line_of(fd),
                    visibility: spec.conventions.visibility_of(&fname),
                    properties: props,
                });
                ctx.refs.push(ExtractedRef {
                    kind: "HasField".to_string(),
                    from_qualified_name: owner_qn.to_string(),
                    to_qualified_name: fqn,
                });
            }
        }
    }
}

/// Emits one `Constant` node + `Defines` edge per value-declaration name the
/// conventions accept (`is_constant_name`). Stack DFS mirrors the hand-written
/// walker so grouped `const ( ... )` blocks are fully descended.
fn walk_value_decl(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
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

/// Import walker: interprets one import statement into zero or more `Import`
/// nodes + import edges via the conventions (Go descends to `import_spec`
/// nodes; Python dispatches the three Python import-statement kinds).
pub(crate) fn walk_imports(spec: &LangSpec, ctx: &mut WalkCtx, import_node: Node, scope: &str) {
    for entry in spec
        .conventions
        .imports_of(ctx.source, spec, import_node, scope)
    {
        ctx.nodes.push(ExtractedNode {
            label: LABEL_IMPORT.to_string(),
            name: entry.display_name,
            qualified_name: entry.qualified_name,
            start_line: entry.start_line,
            end_line: entry.end_line,
            visibility: entry.visibility,
            properties: entry.properties,
        });
        ctx.refs.push(ExtractedRef {
            kind: spec.conventions.import_ref_kind().to_string(),
            from_qualified_name: scope.to_string(),
            to_qualified_name: entry.ref_to,
        });
    }
}

/// Call walker: emits one `CallSite` node + edge per call expression the
/// conventions accept. Stack DFS matches the hand-written walker so the `seq`
/// counter (which keys Go call-site QNs) is assigned in identical order. `seq`
/// is consumed only when the callee is accepted, so a dropped call (Go's
/// non-identifier callee) does not perturb the counter.
pub(crate) fn walk_calls(spec: &LangSpec, ctx: &mut WalkCtx, root: Node, caller_qn: &str) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if kind_in(spec.call_node_kinds, n.kind()) {
            if let Some(callee) = spec.conventions.call_callee(ctx.source, n) {
                let seq = ctx.next_seq();
                let entry = spec
                    .conventions
                    .call_entry(ctx.source, n, caller_qn, &callee, seq);
                ctx.nodes.push(ExtractedNode {
                    label: LABEL_CALL_SITE.to_string(),
                    name: entry.name,
                    qualified_name: entry.qualified_name,
                    start_line: entry.start_line,
                    end_line: entry.end_line,
                    visibility: entry.visibility,
                    properties: entry.properties,
                });
                ctx.refs.push(ExtractedRef {
                    kind: entry.ref_kind.to_string(),
                    from_qualified_name: caller_qn.to_string(),
                    to_qualified_name: entry.ref_to,
                });
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

/// Embedded-language walker: for each embedded rule, locate every script node
/// under `parent`, re-parse its content child with the embedded language's
/// grammar, and run the generic def walker on the inner tree under `scope`.
/// Empty `spec.embedded` (all ten core languages) makes this a no-op.
pub(crate) fn walk_embedded(spec: &LangSpec, ctx: &mut WalkCtx, parent: Node, scope: &str) {
    if spec.embedded.is_empty() {
        return;
    }
    for emb in spec.embedded {
        let inner_spec = match super::registry::lang_spec(emb.embedded_language) {
            Some(s) => s,
            None => continue,
        };
        let mut stack = vec![parent];
        while let Some(n) = stack.pop() {
            if n.kind() == emb.script_node_kind {
                if let Some(content) = find_child_of_kind(n, emb.content_node_kind) {
                    reparse_embedded(inner_spec, ctx, content, scope);
                }
            }
            let mut cursor = n.walk();
            for c in n.children(&mut cursor) {
                stack.push(c);
            }
        }
    }
}

fn find_child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// Re-parses the byte slice of `content` with `inner_spec`'s grammar and walks
/// the inner tree. The inner nodes/refs are appended to the same `WalkCtx`, so
/// embedded symbols land in the host file's `ParseResult` under `scope`.
fn reparse_embedded(inner_spec: &LangSpec, ctx: &mut WalkCtx, content: Node, scope: &str) {
    let text = node_text(ctx.source, content);
    let lang: tree_sitter::Language = (inner_spec.ts_language)();
    let mut parser = Parser::new();
    if parser.set_language(&lang).is_err() {
        return;
    }
    let tree = match parse_with_timeout(&mut parser, &text) {
        Ok(t) => t,
        Err(_) => return,
    };
    // Inner walk runs on a scoped sub-context so the embedded source's byte
    // offsets resolve against `text`, then its output is merged into `ctx`.
    let mut inner = WalkCtx {
        source: &text,
        nodes: Vec::new(),
        refs: Vec::new(),
        next_seq: ctx.next_seq,
        emitted_qns: std::mem::take(&mut ctx.emitted_qns),
    };
    walk_defs(inner_spec, &mut inner, tree.root_node(), scope, None);
    ctx.next_seq = inner.next_seq;
    ctx.emitted_qns = inner.emitted_qns;
    ctx.nodes.append(&mut inner.nodes);
    ctx.refs.append(&mut inner.refs);
}
