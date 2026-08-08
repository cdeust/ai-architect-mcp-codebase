// parser::spec::declarative_tests — direct unit tests for the declarative
// interpreter (`declarative.rs`), split out to keep that file under the
// coding-standards §4.1 500-line file cap (a pure move; no behavior here).
//
// Each `VisibilityRule`/`ImportRule` variant Go's own row does not select
// (`SigilPrefix`, `ModifierKeyword`, `StatementStrip`) is exercised directly
// here — see `declarative.rs`'s module doc comment for why they exist before
// any `LangSpec` row selects them (issue #220 phase 1, forthcoming Java
// migration).
//
// `ConventionSpec` values below are `static`, not `let`-bound locals:
// `DeclarativeConventions` wraps a `&'static ConventionSpec` (it must be
// `'static` to live inside a `static LangSpec.conventions` in production), so
// a stack-local binding does not satisfy the borrow the wrapper requires.

use tree_sitter::Node;

use super::conventions::LanguageConventions;
use super::declarative::{shape_statement_strip, DeclarativeConventions};
use super::declarative_rules::{
    CallEntryRule, CallSiteQnScheme, CalleeDispatchRow, CalleeTransform, ConventionSpec,
    ImportRule, PropertySet, QnScheme, ReceiverPattern, RefToRule, VisibilityRule,
};

// --- VisibilityRule::NameCase (Go's shape, exercised in production via
// go_parity_tests.rs; repeated here as a direct unit boundary test) ---

static NAME_CASE_SPEC: ConventionSpec = ConventionSpec {
    visibility: VisibilityRule::NameCase {
        public_label: "public",
        else_label: "package",
    },
    receiver_pattern: None,
    qn_scheme: QnScheme::SeqSuffixed,
    callee_dispatch: &[],
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::LineColSeq,
        ref_kind: "Calls",
        ref_to: RefToRule::Verbatim,
        visibility: "public",
        properties: PropertySet::CalleeOnly,
    },
    import_rule: ImportRule::ContainerTable,
};

#[test]
fn name_case_visibility_matches_the_go_shape() {
    let conv = DeclarativeConventions(&NAME_CASE_SPEC);
    assert_eq!(conv.visibility_of("Exported"), "public");
    assert_eq!(conv.visibility_of("unexported"), "package");
    assert_eq!(conv.visibility_of(""), "package");
}

// --- VisibilityRule::SigilPrefix (Python's shape; not yet selected by any
// LangSpec row) ---

static SIGIL_PREFIX_SPEC: ConventionSpec = ConventionSpec {
    visibility: VisibilityRule::SigilPrefix {
        prefix: '_',
        private_label: "private",
        public_label: "",
        dunder_exempt: true,
    },
    receiver_pattern: None,
    qn_scheme: QnScheme::PlainDedup,
    callee_dispatch: &[],
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::ByteSpan,
        ref_kind: "Defines",
        ref_to: RefToRule::OwnQn,
        visibility: "",
        properties: PropertySet::CalleeAndCaller,
    },
    import_rule: ImportRule::ContainerTable,
};

#[test]
fn sigil_prefix_visibility_matches_the_python_shape() {
    let conv = DeclarativeConventions(&SIGIL_PREFIX_SPEC);
    assert_eq!(conv.visibility_of("public_name"), "");
    assert_eq!(conv.visibility_of("_private_name"), "private");
    assert_eq!(conv.visibility_of("__dunder__"), "");
}

// --- VisibilityRule::ModifierKeyword (Java's shape; not yet selected by any
// LangSpec row) — exercised against Go's own grammar purely to prove the
// field-scan ALGORITHM, not any language's real behavior. ---

static MODIFIER_KEYWORD_SPEC: ConventionSpec = ConventionSpec {
    visibility: VisibilityRule::ModifierKeyword {
        modifier_field: Some("name"),
        candidates: &[("public", "public"), ("private", "private")],
        default_label: "package",
    },
    receiver_pattern: None,
    qn_scheme: QnScheme::SeqSuffixed,
    callee_dispatch: &[],
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::LineColSeq,
        ref_kind: "Calls",
        ref_to: RefToRule::Verbatim,
        visibility: "public",
        properties: PropertySet::CalleeOnly,
    },
    import_rule: ImportRule::ContainerTable,
};

#[test]
fn modifier_keyword_node_visibility_scans_the_named_field() {
    let conv = DeclarativeConventions(&MODIFIER_KEYWORD_SPEC);

    // Parse a trivial Go source purely to obtain a real `Node` whose `name`
    // field's text we control — this validates the interpreter's
    // scan-a-named-field mechanism, not Go's own (name-case) visibility.
    let source = "package p\nfunc private() {}\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node();
    let func = root
        .named_child(1)
        .expect("function_declaration is the second top-level child");
    assert_eq!(func.kind(), "function_declaration");

    assert_eq!(conv.node_visibility(source, func, "private"), "private");
}

// --- ReceiverPattern (Go's shape) ---

static RECEIVER_PATTERN_SPEC: ConventionSpec = ConventionSpec {
    visibility: VisibilityRule::AlwaysPublic,
    receiver_pattern: Some(ReceiverPattern {
        strip_parens: true,
        take_last_whitespace_token: true,
        strip_leading_sigils: &['*'],
    }),
    qn_scheme: QnScheme::SeqSuffixed,
    callee_dispatch: &[],
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::LineColSeq,
        ref_kind: "Calls",
        ref_to: RefToRule::Verbatim,
        visibility: "public",
        properties: PropertySet::CalleeOnly,
    },
    import_rule: ImportRule::ContainerTable,
};

#[test]
fn receiver_pattern_matches_the_go_shape() {
    let conv = DeclarativeConventions(&RECEIVER_PATTERN_SPEC);
    assert_eq!(conv.receiver_type("(c *T)"), "T");
    assert_eq!(conv.receiver_type("(c T)"), "T");
    assert_eq!(conv.receiver_type("(*T)"), "T");
}

static NO_RECEIVER_PATTERN_SPEC: ConventionSpec = ConventionSpec {
    visibility: VisibilityRule::AlwaysPublic,
    receiver_pattern: None,
    qn_scheme: QnScheme::SeqSuffixed,
    callee_dispatch: &[],
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::LineColSeq,
        ref_kind: "Calls",
        ref_to: RefToRule::Verbatim,
        visibility: "public",
        properties: PropertySet::CalleeOnly,
    },
    import_rule: ImportRule::ContainerTable,
};

#[test]
fn no_receiver_pattern_yields_empty_string() {
    let conv = DeclarativeConventions(&NO_RECEIVER_PATTERN_SPEC);
    assert_eq!(conv.receiver_type("(c *T)"), "");
}

// --- QnScheme ---

static SEQ_SUFFIXED_SPEC: ConventionSpec = ConventionSpec {
    visibility: VisibilityRule::AlwaysPublic,
    receiver_pattern: None,
    qn_scheme: QnScheme::SeqSuffixed,
    callee_dispatch: &[],
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::LineColSeq,
        ref_kind: "Calls",
        ref_to: RefToRule::Verbatim,
        visibility: "public",
        properties: PropertySet::CalleeOnly,
    },
    import_rule: ImportRule::ContainerTable,
};

static PLAIN_DEDUP_SPEC: ConventionSpec = ConventionSpec {
    visibility: VisibilityRule::AlwaysPublic,
    receiver_pattern: None,
    qn_scheme: QnScheme::PlainDedup,
    callee_dispatch: &[],
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::LineColSeq,
        ref_kind: "Calls",
        ref_to: RefToRule::Verbatim,
        visibility: "public",
        properties: PropertySet::CalleeOnly,
    },
    import_rule: ImportRule::ContainerTable,
};

#[test]
fn qn_scheme_variants_match_their_shapes() {
    assert_eq!(
        DeclarativeConventions(&SEQ_SUFFIXED_SPEC).def_qn("scope", "name", 3),
        "scope::name#3"
    );
    assert_eq!(
        DeclarativeConventions(&PLAIN_DEDUP_SPEC).def_qn("scope", "name", 3),
        "scope::name"
    );
}

// --- CalleeDispatchRow / CalleeTransform ---

static VERBATIM_CALLEE_SPEC: ConventionSpec = ConventionSpec {
    visibility: VisibilityRule::AlwaysPublic,
    receiver_pattern: None,
    qn_scheme: QnScheme::PlainDedup,
    callee_dispatch: &[CalleeDispatchRow {
        node_kind: None,
        field: "function",
        fallback_field: None,
        transform: CalleeTransform::Verbatim,
        require_leading_ident: false,
        suffix: None,
    }],
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::ByteSpan,
        ref_kind: "Defines",
        ref_to: RefToRule::OwnQn,
        visibility: "",
        properties: PropertySet::CalleeAndCaller,
    },
    import_rule: ImportRule::ContainerTable,
};

/// Parses `source` with tree-sitter-go and returns the first node of `kind`
/// found by a pre-order DFS. Test-only helper shared by the interpreter
/// branch tests below — any grammar can supply the `Node`s these tests need,
/// since the branches under test (`RefToRule::TailSegment`,
/// `ImportRule::StatementStrip`) read node text/fields generically and are
/// not Go-specific themselves.
fn first_node_of_kind<'t>(tree: &'t tree_sitter::Tree, source: &str, kind: &str) -> Node<'t> {
    let _ = source;
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == kind {
            return n;
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    panic!("no {kind} node found");
}

fn parse_go(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

#[test]
fn callee_dispatch_verbatim_transform_and_call_entry_are_wired() {
    let conv = DeclarativeConventions(&VERBATIM_CALLEE_SPEC);
    let source = "package p\nfunc f() { g(1) }\n";
    let tree = parse_go(source);
    let call = first_node_of_kind(&tree, source, "call_expression");

    let callee = conv
        .call_callee(source, call)
        .expect("Verbatim transform must accept a non-empty field text");
    assert_eq!(
        callee, "g",
        "Verbatim: field text used as-is, no tail split"
    );

    let entry = conv.call_entry(source, call, "caller", &callee, 1);
    assert_eq!(entry.visibility, "");
    assert_eq!(entry.ref_kind, "Defines");
    // RefToRule::OwnQn: the ref target IS the call site's own QN.
    assert_eq!(entry.ref_to, entry.qualified_name);
    assert_eq!(
        entry.properties,
        vec![
            ("callee_name".to_string(), "g".to_string()),
            ("caller_qn".to_string(), "caller".to_string()),
        ],
        "PropertySet::CalleeAndCaller"
    );
    // CallSiteQnScheme::ByteSpan: `{caller}::call@{line}:{col}#{start}-{end}`.
    assert!(
        entry.qualified_name.contains('-') && entry.qualified_name.contains("::call@"),
        "ByteSpan QN scheme, got {}",
        entry.qualified_name
    );
}

static TAIL_SEGMENT_REF_TO_SPEC: ConventionSpec = ConventionSpec {
    visibility: VisibilityRule::AlwaysPublic,
    receiver_pattern: None,
    qn_scheme: QnScheme::SeqSuffixed,
    callee_dispatch: &[CalleeDispatchRow {
        node_kind: None,
        field: "function",
        fallback_field: None,
        transform: CalleeTransform::Verbatim,
        require_leading_ident: false,
        suffix: None,
    }],
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::LineColSeq,
        ref_kind: "Calls",
        // TypeScript's shape: ref_to is the tail segment of the (possibly
        // dotted) callee text, even when the callee text itself is kept
        // verbatim as the CallSite's own name.
        ref_to: RefToRule::TailSegment(&['.']),
        visibility: "public",
        properties: PropertySet::CalleeOnly,
    },
    import_rule: ImportRule::ContainerTable,
};

#[test]
fn ref_to_tail_segment_splits_the_callee_for_the_edge_target() {
    let conv = DeclarativeConventions(&TAIL_SEGMENT_REF_TO_SPEC);
    let source = "package p\nfunc f() { pkg.Sub.Call() }\n";
    let tree = parse_go(source);
    let call = first_node_of_kind(&tree, source, "call_expression");
    let callee = conv
        .call_callee(source, call)
        .expect("Verbatim transform must accept a non-empty field text");
    assert_eq!(callee, "pkg.Sub.Call", "callee kept verbatim by this row");
    let entry = conv.call_entry(source, call, "caller", &callee, 1);
    assert_eq!(
        entry.ref_to, "Call",
        "TailSegment('.') splits the dotted callee for the edge target"
    );
    assert_eq!(
        entry.name, "pkg.Sub.Call",
        "the CallSite's own display name stays the full callee text"
    );
}

static STATEMENT_STRIP_IMPORT_SPEC: ConventionSpec = ConventionSpec {
    visibility: VisibilityRule::AlwaysPublic,
    receiver_pattern: None,
    qn_scheme: QnScheme::SeqSuffixed,
    callee_dispatch: &[],
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::LineColSeq,
        ref_kind: "Calls",
        ref_to: RefToRule::Verbatim,
        visibility: "public",
        properties: PropertySet::CalleeOnly,
    },
    import_rule: ImportRule::StatementStrip {
        strip_prefixes: &["import"],
        trim_end: &[],
        delimiter_pair: None,
    },
};

#[test]
fn import_rule_statement_strip_is_wired_through_imports_of() {
    let conv = DeclarativeConventions(&STATEMENT_STRIP_IMPORT_SPEC);
    let source = "package p\nimport \"fmt\"\n";
    let tree = parse_go(source);
    // Any statement-shaped node will do: `ImportRule::StatementStrip` reads
    // the node's own text verbatim, independent of node kind.
    let stmt = first_node_of_kind(&tree, source, "import_declaration");
    // `imports_of` takes a `&LangSpec` only to satisfy the trait signature —
    // `ImportRule::StatementStrip` never reads it (only `ContainerTable`
    // does, for its `leaf_kinds`/`path_field` descent). Any real `LangSpec`
    // reference is safe to pass; `GO_SPEC` is convenient and already public.
    let entries = conv.imports_of(source, &super::go::GO_SPEC, stmt, "scope");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].display_name, "fmt",
        "surrounding quotes are stripped"
    );
    assert_eq!(entries[0].ref_to, "fmt");
}

// --- ImportRule::StatementStrip (Java's shape; not yet selected by any
// LangSpec row) ---

#[test]
fn statement_strip_reproduces_a_prefixed_delimited_import() {
    let cleaned = shape_statement_strip(
        "import com.app.Config;",
        &["import", "static"],
        &[';'],
        None,
    )
    .expect("a well-formed import statement must shape to a path");
    assert_eq!(cleaned, "com.app.Config");

    let static_cleaned = shape_statement_strip(
        "import static java.util.Objects.requireNonNull;",
        &["import static", "import"],
        &[';'],
        None,
    )
    .expect("a static import must also shape to a path");
    assert_eq!(static_cleaned, "java.util.Objects.requireNonNull");

    assert_eq!(
        shape_statement_strip("import ;", &["import"], &[';'], None),
        None,
        "an empty path must yield no entry"
    );
}

#[test]
fn statement_strip_delimiter_pair_strips_angle_brackets() {
    let cleaned = shape_statement_strip("#include <stdio.h>", &["#include"], &[], Some(('<', '>')))
        .expect("a bracketed include must shape to a path");
    assert_eq!(cleaned, "stdio.h");
}
