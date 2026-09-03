// parser::spec::declarative — the generic interpreter for the data-driven
// `LanguageConventions` implementation (issue #220 phase 1, "the declarative
// deep path"). The rule-data TYPES this interprets live in
// `declarative_rules.rs` (split out to keep both files under the
// coding-standards §4.1 500-line cap).
//
// `DeclarativeConventions` implements `LanguageConventions` exactly ONCE by
// matching a `&'static ConventionSpec`'s rule variants and delegating to a
// small number of shared algorithms — generalizing the pattern
// `c_family.rs` already uses for C/C++/ObjC (functions shared across a
// family, selected by data) to "shared across every family, selected by a
// data discriminant." This is the default path for every migrated and newly
// onboarded language; `LanguageConventions` itself survives unchanged as the
// rare hand-written escape hatch for a language whose grammar demonstrably
// does not fit any declared rule variant (issue #220 design comment, "The
// escape hatch").

use tree_sitter::Node;

use super::conventions::{CallEntry, ImportEntry, LanguageConventions};
use super::declarative_rules::{
    CallSiteQnScheme, CalleeTransform, ConventionSpec, ImportRule, ModifierSource, PropertySet,
    QnScheme, RefToRule, VisibilityRule,
};
use super::lang_spec::LangSpec;
use crate::parser::{node_field_text, node_text, qual};

// ---------------------------------------------------------------------------
// The generic interpreter
// ---------------------------------------------------------------------------

/// Implements `LanguageConventions` once by interpreting a
/// `&'static ConventionSpec`. This is the default path for every migrated and
/// newly onboarded language (issue #220 design comment, "The escape hatch");
/// `LanguageConventions` itself survives unchanged as the rare hand-written
/// escape hatch for a language whose grammar demonstrably does not fit any
/// declared rule variant.
pub(crate) struct DeclarativeConventions(pub &'static ConventionSpec);

/// Accepts a callee string iff non-empty and starting with an identifier
/// character. Mirrors `c_family::identifier_callee` (the same decision point
/// both `GoConventions::call_callee` and the C family's `member_access_callee`
/// funnel through), lifted here so the declarative dispatcher shares it too.
fn accept_ident_callee(text: String, require_leading_ident: bool) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    if require_leading_ident
        && !text
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
    {
        return None;
    }
    Some(text)
}

fn tail_segment(text: &str, separators: &[char]) -> String {
    text.rsplit(separators)
        .next()
        .unwrap_or("")
        .trim_end_matches('(')
        .trim()
        .to_string()
}

impl LanguageConventions for DeclarativeConventions {
    fn visibility_of(&self, name: &str) -> String {
        match &self.0.visibility {
            VisibilityRule::NameCase {
                public_label,
                else_label,
            } => match name.chars().next() {
                Some(c) if c.is_uppercase() => public_label.to_string(),
                _ => else_label.to_string(),
            },
            VisibilityRule::SigilPrefix {
                prefix,
                private_label,
                public_label,
                dunder_exempt,
            } => {
                let double = format!("{prefix}{prefix}");
                if *dunder_exempt && name.starts_with(&double) && name.ends_with(&double) {
                    public_label.to_string()
                } else if name.starts_with(*prefix) {
                    private_label.to_string()
                } else {
                    public_label.to_string()
                }
            }
            // No node available in this method's signature — the name-only
            // fallback is the trait's own default shape (`default_label`).
            VisibilityRule::ModifierKeyword { default_label, .. } => default_label.to_string(),
            VisibilityRule::AlwaysPublic => "public".to_string(),
        }
    }

    fn node_visibility(&self, source: &str, node: Node, name: &str) -> String {
        match &self.0.visibility {
            VisibilityRule::ModifierKeyword {
                modifier_source,
                candidates,
                default_label,
            } => {
                let text = match modifier_source {
                    ModifierSource::Field(f) => node_field_text(source, node, f),
                    // Scan only IMMEDIATE children for the first one whose
                    // kind matches — never the whole subtree/whole node text
                    // (that would leak a NESTED declaration's own modifiers
                    // into this node's result; see `ModifierSource::ChildKind`'s
                    // doc comment in `declarative_rules.rs`).
                    ModifierSource::ChildKind(kind) => {
                        let mut cursor = node.walk();
                        let found = node.children(&mut cursor).find(|c| c.kind() == *kind);
                        found.map(|c| node_text(source, c)).unwrap_or_default()
                    }
                };
                for (keyword, label) in *candidates {
                    if text.split_whitespace().any(|tok| tok == *keyword) {
                        return label.to_string();
                    }
                }
                default_label.to_string()
            }
            _ => self.visibility_of(name),
        }
    }

    fn receiver_type(&self, receiver_text: &str) -> String {
        let Some(pattern) = &self.0.receiver_pattern else {
            return String::new();
        };
        let mut text = receiver_text.trim();
        if pattern.strip_parens {
            text = text.trim_start_matches('(').trim_end_matches(')');
        }
        let mut result = if pattern.take_last_whitespace_token {
            text.split_whitespace().last().unwrap_or("").to_string()
        } else {
            text.to_string()
        };
        for sigil in pattern.strip_leading_sigils {
            result = result.trim_start_matches(*sigil).to_string();
        }
        result
    }

    fn def_qn(&self, scope: &str, name: &str, seq: u64) -> String {
        match self.0.qn_scheme {
            QnScheme::SeqSuffixed => format!("{scope}::{name}#{seq}"),
            QnScheme::PlainDedup => qual(scope, name),
        }
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        let kind = call_node.kind();
        let row = self
            .0
            .callee_dispatch
            .iter()
            .find(|r| r.node_kind.is_none_or(|k| k == kind))?;
        let mut text = node_field_text(source, call_node, row.field);
        if text.is_empty() {
            if let Some(fallback) = row.fallback_field {
                text = node_field_text(source, call_node, fallback);
            }
        }
        let shaped = match row.transform {
            CalleeTransform::Verbatim => text,
            CalleeTransform::TailSegment(separators) => tail_segment(&text, separators),
        };
        let shaped = match row.suffix {
            Some(suffix) if !shaped.is_empty() => format!("{shaped}{suffix}"),
            _ => shaped,
        };
        accept_ident_callee(shaped, row.require_leading_ident)
    }

    fn call_entry(
        &self,
        _source: &str,
        call_node: Node,
        caller_qn: &str,
        callee: &str,
        seq: u64,
    ) -> CallEntry {
        let rule = &self.0.call_entry;
        let line = call_node.start_position().row + 1;
        let col = call_node.start_position().column + 1;
        let qualified_name = match rule.qn_scheme {
            CallSiteQnScheme::LineColSeq => format!("{caller_qn}::call@{line}:{col}#{seq}"),
            CallSiteQnScheme::ByteSpan => format!(
                "{caller_qn}::call@{line}:{col}#{}-{}",
                call_node.start_byte(),
                call_node.end_byte()
            ),
        };
        let ref_to = match rule.ref_to {
            RefToRule::Verbatim => callee.to_string(),
            RefToRule::OwnQn => qualified_name.clone(),
            RefToRule::TailSegment(separators) => {
                let seg = tail_segment(callee, separators);
                if seg.is_empty() {
                    callee.to_string()
                } else {
                    seg
                }
            }
        };
        // source: LSP 3.17 Base Protocol — positions are 0-based; `col` above
        // is 1-based (this spec's QN convention), so `lsp_col` carries the
        // raw tree-sitter 0-based column separately. Read by
        // indexer::persist::nodes::append_label_properties.
        let lsp_col = call_node.start_position().column.to_string();
        let mut properties = match rule.properties {
            PropertySet::CalleeOnly => vec![("callee_name".to_string(), callee.to_string())],
            PropertySet::CalleeAndCaller => vec![
                ("callee_name".to_string(), callee.to_string()),
                ("caller_qn".to_string(), caller_qn.to_string()),
            ],
        };
        properties.push(("lsp_col".to_string(), lsp_col));
        CallEntry {
            name: callee.to_string(),
            qualified_name,
            visibility: rule.visibility.to_string(),
            properties,
            start_line: call_node.start_position().row as u64 + 1,
            end_line: call_node.end_position().row as u64 + 1,
            ref_kind: rule.ref_kind,
            ref_to,
        }
    }

    fn imports_of(
        &self,
        source: &str,
        spec: &LangSpec,
        import_stmt: Node,
        scope: &str,
    ) -> Vec<ImportEntry> {
        match &self.0.import_rule {
            ImportRule::StatementStrip {
                strip_prefixes,
                trim_end,
                delimiter_pair,
            } => statement_strip_entry(
                source,
                import_stmt,
                scope,
                strip_prefixes,
                trim_end,
                *delimiter_pair,
            )
            .into_iter()
            .collect(),
            ImportRule::ContainerTable => {
                let Some(path_field) = spec.import_path_field else {
                    return Vec::new();
                };
                container_table_entries(
                    source,
                    import_stmt,
                    scope,
                    spec.import_spec_kinds,
                    path_field,
                )
            }
        }
    }
}

/// Pure string-shaping half of `ImportRule::StatementStrip`: strips known
/// prefixes, a trailing char set, and an optional delimiter pair, returning
/// the cleaned import path — or `None` when nothing survives the strip.
/// Split from `statement_strip_entry` (which adds the node's line span) so
/// the shaping rules are testable without a real import-statement `Node`
/// (Move 5: I/O — reading node text — separated from the pure transform).
/// `pub(super)`: exercised directly by `declarative_tests.rs`.
pub(super) fn shape_statement_strip(
    text: &str,
    strip_prefixes: &[&str],
    trim_end: &[char],
    delimiter_pair: Option<(char, char)>,
) -> Option<String> {
    let mut text = text.trim().to_string();
    for prefix in strip_prefixes {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim_start().to_string();
        }
    }
    for c in trim_end {
        text = text.trim_end_matches(*c).to_string();
    }
    text = text.trim().to_string();
    if let Some((open, close)) = delimiter_pair {
        text = text
            .trim_start_matches(open)
            .trim_end_matches(close)
            .trim()
            .to_string();
    }
    let cleaned = text.trim_matches('"').to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// `ImportRule::StatementStrip` interpreter: one statement's own text, minus
/// known prefixes and a trailing delimiter/bracket pair, is the cleaned import
/// path.
fn statement_strip_entry(
    source: &str,
    node: Node,
    scope: &str,
    strip_prefixes: &[&str],
    trim_end: &[char],
    delimiter_pair: Option<(char, char)>,
) -> Option<ImportEntry> {
    let text = node_text(source, node);
    let cleaned = shape_statement_strip(&text, strip_prefixes, trim_end, delimiter_pair)?;
    let display_name = cleaned
        .rsplit(['/', '.'])
        .next()
        .unwrap_or(&cleaned)
        .to_string();
    Some(ImportEntry {
        display_name,
        qualified_name: qual(scope, &format!("import:{cleaned}")),
        ref_to: cleaned.clone(),
        properties: vec![("path".to_string(), cleaned)],
        visibility: "package".to_string(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
    })
}

/// `ImportRule::ContainerTable` interpreter: stack DFS over the import
/// statement's subtree; each node matching `leaf_kinds` yields one
/// `ImportEntry` from its `path_field` text. Reproduces
/// `GoConventions::imports_of` / `go_import_entry` exactly (`go.rs:31-151`)
/// when `leaf_kinds = spec.import_spec_kinds` and `path_field = "path"`.
fn container_table_entries(
    source: &str,
    import_stmt: Node,
    scope: &str,
    leaf_kinds: &[&str],
    path_field: &str,
) -> Vec<ImportEntry> {
    let mut out = Vec::new();
    let mut stack = vec![import_stmt];
    while let Some(n) = stack.pop() {
        if leaf_kinds.contains(&n.kind()) {
            let path = node_field_text(source, n, path_field);
            let cleaned = path.trim_matches('"').to_string();
            if !cleaned.is_empty() {
                let display_name = cleaned.rsplit('/').next().unwrap_or(&cleaned).to_string();
                out.push(ImportEntry {
                    display_name,
                    qualified_name: format!("{scope}::import:{cleaned}"),
                    ref_to: cleaned.clone(),
                    properties: vec![("path".to_string(), cleaned)],
                    visibility: "public".to_string(),
                    start_line: n.start_position().row as u64 + 1,
                    end_line: n.end_position().row as u64 + 1,
                });
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    out
}
