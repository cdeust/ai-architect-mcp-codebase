// parser::spec::declarative_rules — the closed rule-data types for the
// data-driven `LanguageConventions` implementation (issue #220 phase 1, "the
// declarative deep path"). Split from `declarative.rs` (the interpreter) to
// keep both files under the coding-standards §4.1 500-line cap; a pure move,
// no behavior here.
//
// `LanguageConventions` (`conventions.rs`) requires six methods with no
// default body. Every hand-written `impl` for the ten migrated languages
// (`go.rs`, `python.rs`, `java.rs`, ...) reduces those six methods to a small,
// closed set of shapes (name-case visibility, a receiver strip pattern, a
// two-valued QN scheme, a small ordered callee-dispatch table, a fully
// combinatorial call-site shaper, and a container-descent import shape) — see
// issue #220 comment "Declarative deep-extraction path — design" for the full
// grounding against the real implementations.
//
// `ConventionSpec` is that closed set as pure data; `declarative.rs`'s
// `DeclarativeConventions` implements `LanguageConventions` exactly ONCE by
// interpreting a `&'static ConventionSpec`, generalizing the pattern
// `c_family.rs` already uses for C/C++/ObjC (functions shared across a
// family, selected by data) from "shared within one family" to "shared
// across every family, selected by a data discriminant."
//
// Scope of THIS PR (phase 1, Go only): every variant below is grounded
// against Go's real `GoConventions` (`go.rs`) and proven at exact parity in
// `go_parity_tests.rs`. Two shapes the design names but Go does not exercise
// — `VisibilityRule::SigilPrefix`/`ModifierKeyword` and
// `ImportRule::StatementStrip` — are included now because the shared schema
// must exist before the next migration (Java, `ModifierKeyword` +
// `StatementStrip`) can be a data-only change; each has its own direct unit
// test in `declarative_tests.rs` so it is exercised even though no shipped
// `LangSpec` row selects it yet (coding-standards §9: no reserved dead code
// — a tested, documented, forthcoming-consumer variant is not the same as an
// unreachable stub). `ImportRule::BracketedTree` (Rust's brace/alias/glob
// import tree) is NOT included — it needs a new shared recursive algorithm,
// not just new data (issue #220 design comment §1), and is deferred to
// Rust's migration (sequenced last: Go -> Java -> Kotlin -> Swift -> C ->
// TypeScript -> C++ -> ObjC -> Python -> Rust).
//
// `ContainerTable`'s field set is intentionally the minimal shape Go's row
// proves (`leaf_kinds` + `path_field`) rather than the richer alias/wildcard/
// module-prefix shape the design sketches for Python — extending it is
// deferred to Python's migration, when its exact field names are read from
// tree-sitter-python's `node-types.json` and validated by the spec guard
// (coding-standards §8: no invented constants/shapes without a grounded
// source for THIS session).
//
// issue #220 phase 2 (Java) shared-layer fix: `VisibilityRule::ModifierKeyword`
// originally located the modifier text via `modifier_field: Option<&'static
// str>` — a named-field lookup, with a whole-node-text fallback when `None`.
// Reading tree-sitter-java 0.23.5's real `node-types.json` (not assumed)
// showed BOTH were wrong for Java: `modifiers` is never a named field on any
// Java declaration (always an unnamed-but-typed `children` entry), and a
// whole-node-text scan false-positives on a nested declaration's own
// modifiers (`class Outer { public class Inner {} }` would wrongly read
// `Outer` as public). `ModifierSource` replaces `modifier_field` with two
// explicit, correct location strategies (`Field`/`ChildKind`) — see
// `ModifierSource`'s own doc comment. This is the "fix it in the shared
// layer" step the phase 1 design comment called for before Java could depend
// on `ModifierKeyword`.

// ---------------------------------------------------------------------------
// Rule data types
// ---------------------------------------------------------------------------

/// How the modifier-bearing text is located for `VisibilityRule::ModifierKeyword`.
/// Two shapes, grounded against real grammars read for issue #220 phase 2
/// (Java): a `node-types.json` declaration can expose its modifiers either as
/// a genuine named field, or only as an unnamed-but-typed child — the two are
/// NOT interchangeable, and conflating them is exactly the shared-layer defect
/// phase 2 found and fixes here (see the module doc comment's "phase 2" note).
pub(crate) enum ModifierSource {
    /// A named field (`Node::child_by_field_name`) carries the modifier text
    /// directly. Fits a grammar that exposes modifiers as a real field.
    #[allow(dead_code)]
    Field(&'static str),
    /// Scan the node's own IMMEDIATE children (not the whole subtree, and not
    /// the whole node's text) for the first child whose `kind()` equals this
    /// string, and read that child's own text. Required for
    /// tree-sitter-java: `modifiers` is a real node type but is listed under
    /// every declaration's `children`, never under `fields`, in
    /// tree-sitter-java 0.23.5's `node-types.json` (`class_declaration`,
    /// `method_declaration`, `field_declaration`, `interface_declaration`,
    /// `enum_declaration`, `record_declaration`, `constructor_declaration`,
    /// `annotation_type_declaration` all show the identical shape) — so
    /// `ModifierSource::Field("modifiers")` would silently return "" via
    /// `child_by_field_name` for every one of them. Scanning only IMMEDIATE
    /// children (not the whole node text, which phase 1's original `None`
    /// fallback used) is itself load-bearing: `node.children()` on an
    /// enclosing declaration includes every NESTED declaration's subtree too,
    /// so a whole-text scan of `class Outer { public class Inner {} }` would
    /// find "public" from `Inner`'s own modifiers and wrongly report `Outer`
    /// (which has none) as public. Proven by
    /// `modifier_keyword_node_visibility_does_not_leak_a_nested_declarations_modifier`
    /// in `declarative_tests.rs`.
    ChildKind(&'static str),
}

/// How a declared name's visibility is derived. Drives BOTH `visibility_of`
/// (name-only) and `node_visibility` (node + name) from one value — the
/// finding in issue #220's design comment that `node_visibility` (a
/// *defaulted* trait method) is load-bearing for every modifier-keyword
/// language (Java/Swift/Kotlin/Rust-shaped grammars: most of the roster).
pub(crate) enum VisibilityRule {
    /// First-letter case decides visibility (Go: uppercase ⇒ `public`, else
    /// `else_label`). Drives `visibility_of` directly; `node_visibility` falls
    /// through to `visibility_of` since no node-level signal exists.
    /// source: go.rs:58-64 (`GoConventions::visibility_of`).
    NameCase {
        public_label: &'static str,
        else_label: &'static str,
    },
    /// A leading sigil decides visibility, with an optional dunder exemption
    /// (Python: leading `_` ⇒ `private_label`; `__x__` ⇒ `public_label`, which
    /// Python sets to the empty string — "no visibility recorded"). Included
    /// for the shared schema per issue #220's design (python.rs:42-51); no
    /// current `LangSpec` row selects it. Exercised directly by
    /// `sigil_prefix_visibility_matches_the_python_shape` below.
    // `#[allow(dead_code)]` on this and the two variants below: no shipped
    // `LangSpec` row selects them yet (Go's row selects `NameCase`), so a
    // plain non-test build never constructs them — but each is exercised
    // directly under `#[cfg(test)]` in `declarative_tests.rs` (cited per
    // variant below), and the intended first production consumer is named.
    // Forward schema for a named, imminent consumer is not the same as an
    // unreachable stub (coding-standards §9) — see the module doc comment.
    #[allow(dead_code)]
    SigilPrefix {
        prefix: char,
        private_label: &'static str,
        public_label: &'static str,
        dunder_exempt: bool,
    },
    /// A modifier keyword, located via `modifier_source`, decides visibility;
    /// the first matching `(keyword, label)` pair wins, else `default_label`.
    /// Drives `node_visibility` (needs the AST node); `visibility_of`
    /// (name-only, no node available) returns `default_label`. Included for
    /// the shared schema per issue #220's design (java.rs:44-61,
    /// swift.rs:95-104, kotlin_conventions.rs:220-227). Selected by Java
    /// (issue #220 phase 2, `java.rs`'s `JAVA_RULES`) via
    /// `ModifierSource::ChildKind` — see that variant's doc comment for why
    /// `ModifierSource::Field` cannot express Java's shape. Exercised
    /// directly by `modifier_keyword_node_visibility_scans_the_named_field`
    /// (Field) and `modifier_keyword_node_visibility_scans_a_child_kind`
    /// (ChildKind) in `declarative_tests.rs`.
    ModifierKeyword {
        modifier_source: ModifierSource,
        candidates: &'static [(&'static str, &'static str)],
        default_label: &'static str,
    },
    /// Every declared name is `public` (C: no access keyword at all).
    #[allow(dead_code)]
    AlwaysPublic,
}

/// Receiver-text extraction for a method's receiver (Go `(c *T)` → `T`).
/// `None` on `ConventionSpec` means the language has no receiver concept
/// (every sampled language but Go — issue #220 design comment: "5 of 6
/// sampled languages need `None`"), so `receiver_type` becomes optional
/// rather than a near-universal no-op stub.
pub(crate) struct ReceiverPattern {
    /// Strip a leading `(` and trailing `)` before extracting the type.
    pub strip_parens: bool,
    /// Take the LAST whitespace-separated token (drops a receiver's variable
    /// name: `c *T` → `*T`). When `false`, the whole (post-paren-strip) text
    /// is used verbatim.
    pub take_last_whitespace_token: bool,
    /// Sigils stripped from the front of the extracted token, in order
    /// (Go: `*` for a pointer receiver).
    pub strip_leading_sigils: &'static [char],
}

/// Base ("raw") qualified-name shape for a function/method definition, before
/// the walker's collision dedup. Exactly two shapes across the six sampled
/// languages, zero exceptions (issue #220 design comment).
pub(crate) enum QnScheme {
    /// `{scope}::{name}#{seq}` — already unique, dedup is a no-op (Go, and
    /// every C-family language via `c_family::def_qn`).
    SeqSuffixed,
    /// `{scope}::{name}` — the walker's dedup appends `@line` on collision.
    /// Not selected by Go; included for the shared schema (Python/Rust/TS).
    /// `#[allow(dead_code)]`: forward schema, no production consumer yet —
    /// see `VisibilityRule::SigilPrefix`'s comment above.
    #[allow(dead_code)]
    PlainDedup,
}

/// How a callee field's raw text is shaped into the callee name.
pub(crate) enum CalleeTransform {
    /// Use the field text as-is (Python, TypeScript). `#[allow(dead_code)]`:
    /// forward schema, no production consumer yet — see
    /// `VisibilityRule::SigilPrefix`'s comment above.
    #[allow(dead_code)]
    Verbatim,
    /// Take the tail segment after the last separator in `separators`
    /// (Go: `.` only — `fmt.Println` → `Println`; the C family: `.`, `->`,
    /// `:` via the already-shared `c_family::member_access_callee`).
    TailSegment(&'static [char]),
}

/// One row of an ordered callee-dispatch table. The first row whose
/// `node_kind` is `None` (wildcard) or matches the call node's own kind
/// applies. Reproduces every sampled language directly, including Rust's
/// node-kind dispatch (`macro_invocation` vs. every other call kind) via a
/// two-row table — issue #220 design comment: "No residue found in the
/// sample."
pub(crate) struct CalleeDispatchRow {
    /// `None` matches any call node kind (the common one-row case).
    pub node_kind: Option<&'static str>,
    /// Field read first for the callee text.
    pub field: &'static str,
    /// Field tried when `field`'s text is empty (Java's `name` → `type`
    /// fallback for `object_creation_expression`). Not exercised by Go.
    pub fallback_field: Option<&'static str>,
    pub transform: CalleeTransform,
    /// Gate: the shaped text must start with an identifier character
    /// (`[A-Za-z_]`) to be accepted, else the call is dropped (Go only).
    pub require_leading_ident: bool,
    /// Literal text appended after shaping (Rust's macro `!` suffix). Not
    /// exercised by Go.
    pub suffix: Option<&'static str>,
}

/// QN scheme for a call SITE (distinct from `QnScheme`, which shapes a
/// definition's QN).
pub(crate) enum CallSiteQnScheme {
    /// `{caller}::call@{line}:{col}#{seq}` (Go, Java).
    LineColSeq,
    /// `{caller}::call@{line}:{col}#{start}-{end}` byte span (Python,
    /// TypeScript, Rust). Not exercised by Go. `#[allow(dead_code)]`: forward
    /// schema, no production consumer yet — see
    /// `VisibilityRule::SigilPrefix`'s comment above.
    #[allow(dead_code)]
    ByteSpan,
}

/// How the call site's outgoing edge target (`ref_to`) is derived.
pub(crate) enum RefToRule {
    /// The callee text verbatim (Go, Java, C family).
    Verbatim,
    /// The call site's own qualified name (Python: a file-local `Defines`,
    /// later upgraded to cross-file by the resolver). Not exercised by Go.
    /// `#[allow(dead_code)]`: forward schema, no production consumer yet —
    /// see `VisibilityRule::SigilPrefix`'s comment above.
    #[allow(dead_code)]
    OwnQn,
    /// The tail segment after the last separator (TypeScript: `callee_tail`).
    /// Not exercised by Go. `#[allow(dead_code)]`: forward schema, no
    /// production consumer yet — see `VisibilityRule::SigilPrefix`'s comment
    /// above.
    #[allow(dead_code)]
    TailSegment(&'static [char]),
}

/// Which properties a `CallEntry` carries.
pub(crate) enum PropertySet {
    /// `[callee_name]` (Go, Java, C family).
    CalleeOnly,
    /// `[callee_name, caller_qn]` (Python, TypeScript). Not exercised by Go.
    /// `#[allow(dead_code)]`: forward schema, no production consumer yet —
    /// see `VisibilityRule::SigilPrefix`'s comment above.
    #[allow(dead_code)]
    CalleeAndCaller,
}

/// Fully combinatorial call-site shaping — issue #220 design comment: "Zero
/// residue in the sample — every field is a closed enum choice or a literal
/// string, no per-language logic."
pub(crate) struct CallEntryRule {
    pub qn_scheme: CallSiteQnScheme,
    pub ref_kind: &'static str,
    pub ref_to: RefToRule,
    pub visibility: &'static str,
    pub properties: PropertySet,
}

/// How one import STATEMENT node is interpreted into zero or more
/// `ImportEntry`s.
pub(crate) enum ImportRule {
    /// One statement is one import: strip known prefixes and a trailing
    /// delimiter pair, take the last path segment as the display name (Java
    /// `import`/`static`, C `#include` via the shared `c_family::include_entry`
    /// shape). Included for the shared schema (issue #220 design comment,
    /// java.rs:184-209); no current `LangSpec` row selects it — the intended
    /// first consumer is Java. Exercised directly by
    /// `statement_strip_reproduces_a_prefixed_delimited_import` below.
    /// `#[allow(dead_code)]`: forward schema, no production consumer yet —
    /// see `VisibilityRule::SigilPrefix`'s comment above.
    #[allow(dead_code)]
    StatementStrip {
        strip_prefixes: &'static [&'static str],
        trim_end: &'static [char],
        delimiter_pair: Option<(char, char)>,
    },
    /// Descend the import statement's subtree for nodes matching
    /// `spec.import_spec_kinds`; each yields one entry from its
    /// `spec.import_path_field` text (Go: `import_spec` under
    /// `import_declaration`, path field `"path"`, single row, no alias, no
    /// symbol list — issue #220 design comment: "the simplest instance").
    /// Reads the structural node-kind/field data straight off the `LangSpec`
    /// already passed into `imports_of` rather than duplicating it on
    /// `ConventionSpec` — those two `LangSpec` fields exist for exactly this
    /// (their doc comments already say "Go descends to `import_spec_kinds`");
    /// re-declaring them here would be the same fact asserted twice with no
    /// second source of truth. This is the minimal grounded shape; Python's
    /// richer alias/wildcard/module-prefix dispatch (design comment §1) is
    /// deferred to Python's own migration.
    ContainerTable,
}

/// The closed data set replacing one language's hand-written
/// `impl LanguageConventions` (issue #220 phase 1).
pub(crate) struct ConventionSpec {
    pub visibility: VisibilityRule,
    pub receiver_pattern: Option<ReceiverPattern>,
    pub qn_scheme: QnScheme,
    pub callee_dispatch: &'static [CalleeDispatchRow],
    pub call_entry: CallEntryRule,
    pub import_rule: ImportRule,
}
