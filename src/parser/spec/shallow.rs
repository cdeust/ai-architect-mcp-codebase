// parser::spec::shallow — the behavior-free extraction path (ADR-0056).
//
// A `ShallowSpec` is node-kind lists plus a grammar factory. Nothing else: no
// visibility rule, no qualified-name scheme, no callee transform, no import
// dispatch, no inheritance model. Adding a language on this path is a data row
// and a fixture — zero walker code, zero conventions code, zero conditionals.
//
// This is the tier ADR-0055 §2 specified ("Tier 2 — a language is data + one
// fixture, not a walker") and left unbuilt, and it is what the reference
// implementation's breadth actually rests on: a language with only node-type
// lists still produces useful output, so the long tail keeps a shallow result
// instead of being absent. AP's deep path needs six required trait methods to
// emit anything at all, which is why it is capped at ten languages.
//
// What this path deliberately does NOT do (ADR-0056 decision 3):
//   - It does not infer visibility. A name-case guess (uppercase => public) is
//     right for Go and WRONG for Java/Swift/Kotlin/C#, which carry visibility
//     in modifier keywords. An absent field is honest; a plausible-but-false
//     one is a defect that §13.1 F2 forbids as a silent degraded mode.
//   - It emits no inheritance edges, no import aliasing, and no per-language
//     qualified-name scheme.
// Languages needing that depth are promoted to the deep path (`LangSpec`), not
// granted a conditional here.
//
// INVARIANT (ADR-0056 Risk #1, machine-checked by
// `shallow_walker_has_no_language_conditionals`): this module must contain no
// reference to a specific `Language` variant. The reference implementation's
// defect is 176 `lang == CBM_LANG_*` sites across its extractors; that is the
// growing-conditional-chain §1.2 forbids, and the tripwire exists so this path
// cannot drift into it.

use std::collections::HashSet;

use tree_sitter::{Language as TsLanguage, Node, Parser};

use crate::parser::{
    collect_error_ranges, count_parse_errors, node_field_text, node_text, parse_tree_too_deep,
    parse_with_timeout, qual, ExtractedNode, ExtractedRef, Language, ParseResult, LABEL_CALL_SITE,
    LABEL_FUNCTION, LABEL_IMPORT, LABEL_METHOD, LABEL_STRUCT, MAX_TREE_DEPTH,
};

/// Qualified-name separator, shared with the deep path's `qual`.
const QN_SEP: &str = "::";

/// Segment separators a qualified callee may use across grammars (`a.b`,
/// `a::b`, `a->b`). Reducing a callee to its trailing segment is the one
/// transform the shallow path applies, and it is grammar-agnostic: every
/// separator here is punctuation, never a language keyword.
const CALLEE_SEPARATORS: [&str; 3] = [".", QN_SEP, "->"];

/// Quote characters stripped from an import path. Grammars surface a string
/// literal (Go `"fmt"`) with its quotes inside the node text; removing them is
/// lexical cleanup, not a language rule.
const QUOTE_CHARS: [char; 3] = ['"', '\'', '`'];

/// A language described by node kinds alone (ADR-0056 decision 1).
///
/// Every field is read by `parse_shallow`; none is reserved (§9). Fields the
/// reference implementation carries for concerns AP's graph has no node for
/// (branching / throw / env-access node types) are deliberately absent until a
/// consumer exists.
pub(crate) struct ShallowSpec {
    /// Which language this row describes. Read by the registry and the guard.
    pub language: Language,
    /// Declaration kinds emitted as `Function` + `Defines`, or as `Method` +
    /// `HasMethod` when reached inside a `class_node_kinds` body.
    pub function_node_kinds: &'static [&'static str],
    /// Declaration kinds emitted as `Method` + `HasMethod` regardless of
    /// nesting, for grammars with a distinct method node.
    pub method_node_kinds: &'static [&'static str],
    /// Class-like declaration kinds emitted as `Struct` + `Defines` and
    /// recursed into, so their members become methods.
    pub class_node_kinds: &'static [&'static str],
    /// Call expression kinds emitted as `CallSite` + `Calls`.
    pub call_node_kinds: &'static [&'static str],
    /// Field naming the callee on a call node, when the grammar has one (Go
    /// `function`, Ruby `method`). `None` falls back to the call's first named
    /// child.
    ///
    /// This field exists because "the first named child is the callee" is NOT
    /// universal: a grammar that models a receiver (Ruby `foo.bar` => fields
    /// `receiver` + `method`) puts the RECEIVER first, so the fallback would
    /// record `foo` instead of `bar`. That divergence is a property of the
    /// grammar, so it belongs in the row — naming it here is what keeps the
    /// walker free of the `if (lang == RUBY)` conditional the reference
    /// implementation resorts to (ADR-0056 Risk #1).
    pub callee_field: Option<&'static str>,
    /// Import statement kinds emitted as `Import` + `Imports`.
    pub import_node_kinds: &'static [&'static str],
    /// Field naming a declaration (usually `name`).
    pub name_field: &'static str,
    /// Field carrying a body, when the grammar names one. `None` falls back to
    /// scanning the whole declaration node for calls.
    pub body_field: Option<&'static str>,
    /// Grammar factory (the Rust tree-sitter crate's `LANGUAGE`).
    pub ts_language: fn() -> TsLanguage,
}

/// Per-file walk state. `emitted_qns` backs the collision dedup that keeps
/// every definition's primary key unique without a per-language sequence
/// scheme (the deep path's `#seq`).
struct ShallowCtx<'a> {
    source: &'a str,
    nodes: Vec<ExtractedNode>,
    refs: Vec<ExtractedRef>,
    emitted_qns: HashSet<String>,
}

impl ShallowCtx<'_> {
    /// Returns a unique QN: the input if unseen, else `qn@{start_line}`. Two
    /// same-named definitions in one file (an overload, a re-`def`) therefore
    /// each keep a distinct primary key while the readable name survives for
    /// name-based resolver lookups.
    fn dedup(&mut self, qn: String, start_line: u64) -> String {
        if self.emitted_qns.insert(qn.clone()) {
            return qn;
        }
        let unique = format!("{qn}@{start_line}");
        self.emitted_qns.insert(unique.clone());
        unique
    }

    fn push_node(&mut self, node: ExtractedNode) {
        self.nodes.push(node);
    }

    fn push_ref(&mut self, kind: &str, from: &str, to: &str) {
        self.refs.push(ExtractedRef {
            kind: kind.to_string(),
            from_qualified_name: from.to_string(),
            to_qualified_name: to.to_string(),
        });
    }
}

fn line_of(node: Node) -> u64 {
    node.start_position().row as u64 + 1
}

fn end_line_of(node: Node) -> u64 {
    node.end_position().row as u64 + 1
}

fn kind_in(kinds: &[&str], k: &str) -> bool {
    kinds.contains(&k)
}

/// The named children of `node`, in order.
///
/// This is the shallow path's key generic device. Tree-sitter marks a
/// grammar's keywords and punctuation (`import`, `from`, `;`, `(`) as
/// **unnamed** children and its content (identifiers, paths, literals) as
/// **named** ones. Reading only named children therefore strips keywords in
/// every grammar without a per-language keyword list — the thing that would
/// otherwise force one conditional per language.
fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    let out: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
    out
}

/// Trailing segment of a possibly-qualified callee (`Utils.parse` -> `parse`,
/// `a::b::c` -> `c`), with surrounding whitespace removed.
///
/// `pub(super)` for direct unit testing: the separator arithmetic
/// (`idx + sep.len()`) and the empty-candidate guard are only observable for
/// multi-byte separators (`::`, `->`) and for text ENDING in a separator,
/// neither of which a realistic parsed fixture reaches. Mutation testing
/// showed exactly that — three survivors here that no end-to-end fixture
/// could kill (§12.1), so the honest fix is to test the function head-on
/// rather than contort a fixture to reach it.
pub(super) fn last_segment(text: &str) -> String {
    let mut tail = text.trim();
    for sep in CALLEE_SEPARATORS {
        if let Some(idx) = tail.rfind(sep) {
            let candidate = &tail[idx + sep.len()..];
            // Only advance past a separator that actually shortens the text,
            // so `->` and `::` cannot strip a trailing empty segment.
            if !candidate.trim().is_empty() {
                tail = candidate.trim();
            }
        }
    }
    tail.to_string()
}

/// The callee text of a call node: the `callee_field` child when the row names
/// one, else the call's first named child, reduced in both cases to the
/// trailing qualified segment. `None` when neither yields text (an unparsed or
/// malformed call), so nothing is invented.
fn callee_of(spec: &ShallowSpec, source: &str, call_node: Node) -> Option<String> {
    let raw = match spec
        .callee_field
        .and_then(|f| call_node.child_by_field_name(f))
    {
        Some(named) => node_text(source, named),
        None => node_text(source, call_node.named_child(0)?),
    };
    let callee = last_segment(&raw);
    if callee.is_empty() {
        None
    } else {
        Some(callee)
    }
}

/// The import path of an import statement: the concatenated text of its named
/// children with quote characters stripped. Keywords are excluded structurally
/// (see `named_children`), so no per-language keyword list is needed.
fn import_path_of(source: &str, import_node: Node) -> String {
    let children = named_children(import_node);
    let raw = if children.is_empty() {
        node_text(source, import_node)
    } else {
        children
            .iter()
            .map(|c| node_text(source, *c))
            .collect::<Vec<_>>()
            .join(" ")
    };
    raw.trim()
        .trim_matches(|c| QUOTE_CHARS.contains(&c))
        .trim()
        .to_string()
}

/// The node to scan for calls: the named body field when the grammar has one,
/// else the declaration node itself (so an expression-bodied definition still
/// yields its calls).
fn call_scan_of<'t>(spec: &ShallowSpec, node: Node<'t>) -> Node<'t> {
    spec.body_field
        .and_then(|f| node.child_by_field_name(f))
        .unwrap_or(node)
}

/// Emits one `CallSite` node plus its `Calls` edge, attributed to `caller_qn`.
fn emit_call(spec: &ShallowSpec, ctx: &mut ShallowCtx, node: Node, caller_qn: &str) {
    let Some(callee) = callee_of(spec, ctx.source, node) else {
        return;
    };
    let line = line_of(node);
    let col = node.start_position().column as u64 + 1;
    let qn = format!("{caller_qn}{QN_SEP}call@{line}:{col}");
    ctx.push_node(ExtractedNode {
        label: LABEL_CALL_SITE.to_string(),
        name: callee.clone(),
        qualified_name: qn.clone(),
        start_line: line,
        end_line: end_line_of(node),
        // Shallow rows carry no visibility (ADR-0056 decision 3).
        visibility: String::new(),
        properties: vec![("callee_name".to_string(), callee.clone())],
    });
    ctx.push_ref("Calls", &qn, &callee);
}

/// Emits `CallSite` nodes and `Calls` edges for every call under `root`,
/// attributed to the definition `caller_qn`.
fn walk_calls(spec: &ShallowSpec, ctx: &mut ShallowCtx, root: Node, caller_qn: &str) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if kind_in(spec.call_node_kinds, n.kind()) {
            emit_call(spec, ctx, n, caller_qn);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

/// Emits an `Import` node plus its `Imports` edge. An import whose path is
/// empty is skipped rather than emitted as a blank node.
fn emit_import(ctx: &mut ShallowCtx, node: Node, scope: &str) {
    let path = import_path_of(ctx.source, node);
    if path.is_empty() {
        return;
    }
    let display = last_segment(&path);
    let qn = qual(scope, &format!("import:{path}"));
    ctx.push_node(ExtractedNode {
        label: LABEL_IMPORT.to_string(),
        name: if display.is_empty() {
            path.clone()
        } else {
            display
        },
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: String::new(),
        properties: vec![("path".to_string(), path.clone())],
    });
    ctx.push_ref("Imports", &qn, &path);
}

/// Emits a function or method definition and scans its body for calls.
///
/// `as_method` selects the label/edge pair: a definition reached inside a
/// class body, or declared with a method node kind, is a `Method` +
/// `HasMethod`; anything else is a `Function` + `Defines`.
fn emit_def(spec: &ShallowSpec, ctx: &mut ShallowCtx, node: Node, scope: &str, as_method: bool) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let start_line = line_of(node);
    let qn = ctx.dedup(qual(scope, &name), start_line);
    let (label, edge) = if as_method {
        (LABEL_METHOD, "HasMethod")
    } else {
        (LABEL_FUNCTION, "Defines")
    };
    ctx.push_node(ExtractedNode {
        label: label.to_string(),
        name,
        qualified_name: qn.clone(),
        start_line,
        end_line: end_line_of(node),
        visibility: String::new(),
        properties: Vec::new(),
    });
    ctx.push_ref(edge, scope, &qn);
    let body = call_scan_of(spec, node);
    walk_calls(spec, ctx, body, &qn);
}

/// Emits a class-like declaration and recurses into it so its members become
/// methods scoped to the class.
fn emit_class(spec: &ShallowSpec, ctx: &mut ShallowCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let start_line = line_of(node);
    let qn = ctx.dedup(qual(scope, &name), start_line);
    ctx.push_node(ExtractedNode {
        label: LABEL_STRUCT.to_string(),
        name,
        qualified_name: qn.clone(),
        start_line,
        end_line: end_line_of(node),
        // No inheritance edges and no visibility on the shallow path.
        visibility: String::new(),
        properties: Vec::new(),
    });
    ctx.push_ref("Defines", scope, &qn);
    let body = spec
        .body_field
        .and_then(|f| node.child_by_field_name(f))
        .unwrap_or(node);
    walk(spec, ctx, body, &qn, true);
}

/// Recursive descent over the tree.
///
/// `in_class` makes a function-kind definition a method by enclosing scope,
/// which is how most grammars express methods; `method_node_kinds` covers the
/// grammars that give methods their own node kind.
fn walk(spec: &ShallowSpec, ctx: &mut ShallowCtx, parent: Node, scope: &str, in_class: bool) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        let k = child.kind();
        if kind_in(spec.class_node_kinds, k) {
            emit_class(spec, ctx, child, scope);
        } else if kind_in(spec.method_node_kinds, k) {
            emit_def(spec, ctx, child, scope, true);
        } else if kind_in(spec.function_node_kinds, k) {
            emit_def(spec, ctx, child, scope, in_class);
        } else if kind_in(spec.import_node_kinds, k) {
            emit_import(ctx, child, scope);
        } else {
            // A call reached HERE is one outside any definition — top-level or
            // class-body code — so it is attributed to the enclosing scope
            // (the file or the class) rather than dropped.
            //
            // This matters more on the shallow path than on the deep one: in
            // several shallow languages the module system IS top-level code
            // (Ruby's `require` is an ordinary call, not an import node), so
            // ignoring file-scope calls would discard a language's entire
            // dependency signal. Definitions are handled by the arms above and
            // scan their own bodies, so nothing is counted twice.
            if kind_in(spec.call_node_kinds, k) {
                emit_call(spec, ctx, child, scope);
            }
            // Descend regardless, so definitions nested in blocks/namespaces
            // the row does not model — and calls nested in arguments — are
            // still reached.
            walk(spec, ctx, child, scope, in_class);
        }
    }
}

/// Parses `source` with `spec`'s grammar and extracts the shallow
/// `ParseResult` (ADR-0056 decision 2).
///
/// Preconditions: `spec.ts_language` is the grammar matching `source`;
/// `file_path` is the file's repo-relative id and the top scope.
/// Postconditions: returns `Ok(ParseResult)` whose nodes are
/// `Function`/`Method`/`Struct`/`CallSite`/`Import` and whose refs are
/// `Defines`/`HasMethod`/`Calls`/`Imports`, plus the shared parse-error
/// signals; `Err` only on grammar-set or parse-timeout failure. Invariant: no
/// node carries a visibility string and no inheritance edge is emitted — the
/// shallow path reports what the node kinds prove and nothing more.
pub(crate) fn parse_shallow(
    spec: &ShallowSpec,
    source: &str,
    file_path: &str,
) -> Result<ParseResult, String> {
    let lang: TsLanguage = (spec.ts_language)();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set {:?} language: {e}", spec.language))?;
    let tree = parse_with_timeout(&mut parser, source)?;
    // Depth guard (issue #148): the shallow walker recurses per tree level too;
    // reject a pathologically deep tree before walking it.
    if parse_tree_too_deep(tree.root_node(), MAX_TREE_DEPTH) {
        return Err(format!(
            "parse_tree_too_deep: exceeds {MAX_TREE_DEPTH} levels (adversarial or generated nesting)"
        ));
    }

    let mut ctx = ShallowCtx {
        source,
        nodes: Vec::new(),
        refs: Vec::new(),
        emitted_qns: HashSet::new(),
    };
    walk(spec, &mut ctx, tree.root_node(), file_path, false);
    Ok(ParseResult {
        nodes: ctx.nodes,
        refs: ctx.refs,
        parse_errors: count_parse_errors(tree.root_node()),
        error_ranges: collect_error_ranges(tree.root_node()),
    })
}
