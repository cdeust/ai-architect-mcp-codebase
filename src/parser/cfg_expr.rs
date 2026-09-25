// cfg_expr: parses a `#[cfg(...)]` predicate and evaluates it against a set of
// enabled Cargo features (issue #291).
//
// Layer: pure, no I/O. Only `feature = "..."` leaves are decidable from
// `cargo metadata`; every other leaf (`test`, `unix`, `target_os = "..."`,
// `debug_assertions`, …) depends on the build invocation and is `Unknown`.
// Evaluation is three-valued (Kleene logic), so a predicate is reported
// `False` only when the feature leaves alone force it false whatever the
// unknown leaves turn out to be: `all(feature = "x", unix)` with `x` off is
// `False`; `any(feature = "x", unix)` with `x` off is `Unknown`. The caller
// flags a module only on `False`, a compiled module is never flagged.
// source: The Rust Reference, "Conditional compilation" (configuration
// predicates `all`, `any`, `not`, key-value options).
//
// Issue #353: the predicate also has a CANONICAL form and a COMPACT text, so
// two items of one name under mutually exclusive `#[cfg]` predicates can be told
// apart. Every option is kept (`kani`, `test`, `unix`, `target_os = "..."`):
// evaluation still leaves them `Unknown`, but identity must not erase them.

use std::collections::{BTreeMap, BTreeSet};

pub(crate) use super::cfg_lex::without_comments;
use super::cfg_lex::{tokenize, Token};

/// A parsed configuration predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CfgPredicate {
    Feature(String),
    /// Any option other than `feature = "..."`, not decidable here, but kept
    /// by name so twins under `cfg(kani)` and `cfg(not(kani))` stay distinct.
    /// `value` is `None` for a bare name (`unix`, `kani`).
    Option {
        key: String,
        value: Option<String>,
    },
    All(Vec<CfgPredicate>),
    Any(Vec<CfgPredicate>),
    Not(Box<CfgPredicate>),
}

/// Three-valued truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Truth {
    True,
    False,
    Unknown,
}

/// One build the graph can be read under: the Cargo features it enables and the
/// bare options (`kani`, `test`, `miri`) it decides. The default profile decides
/// no option, so every option stays `Unknown` under it. A second profile, such as
/// a Kani build, is a value of this type: nothing else in the evaluation changes.
/// source: issue #353 (the Kani extension point).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BuildProfile {
    pub features: BTreeSet<String>,
    /// `true` for an option the build sets, `false` for one it certainly does
    /// not; an option absent from the map is `Unknown`.
    pub options: BTreeMap<String, bool>,
}

impl BuildProfile {
    /// The profile `cargo metadata` describes: default features, no option decided.
    pub(crate) fn with_features(features: BTreeSet<String>) -> Self {
        BuildProfile {
            features,
            options: BTreeMap::new(),
        }
    }
}

impl CfgPredicate {
    /// Evaluates against the features the build enables; every option is
    /// `Unknown`.
    pub(crate) fn eval(&self, enabled: &BTreeSet<String>) -> Truth {
        self.eval_with(enabled, &BTreeMap::new())
    }

    /// Evaluates under `profile`.
    pub(crate) fn eval_in(&self, profile: &BuildProfile) -> Truth {
        self.eval_with(&profile.features, &profile.options)
    }

    fn eval_with(&self, enabled: &BTreeSet<String>, options: &BTreeMap<String, bool>) -> Truth {
        match self {
            CfgPredicate::Feature(f) if enabled.contains(f) => Truth::True,
            CfgPredicate::Feature(_) => Truth::False,
            CfgPredicate::Option { key, value: None } => match options.get(key) {
                Some(true) => Truth::True,
                Some(false) => Truth::False,
                None => Truth::Unknown,
            },
            CfgPredicate::Option { .. } => Truth::Unknown,
            CfgPredicate::All(items) => fold(items, (enabled, options), Truth::False, Truth::True),
            CfgPredicate::Any(items) => fold(items, (enabled, options), Truth::True, Truth::False),
            CfgPredicate::Not(inner) => match inner.eval_with(enabled, options) {
                Truth::True => Truth::False,
                Truth::False => Truth::True,
                Truth::Unknown => Truth::Unknown,
            },
        }
    }
}

/// `all` short-circuits on `False` and is `True` on an empty list; `any` is
/// the dual. Any `Unknown` left after no short-circuit makes the result
/// `Unknown`.
fn fold(
    items: &[CfgPredicate],
    build: (&BTreeSet<String>, &BTreeMap<String, bool>),
    absorbing: Truth,
    empty: Truth,
) -> Truth {
    let mut result = empty;
    for item in items {
        match item.eval_with(build.0, build.1) {
            t if t == absorbing => return absorbing,
            Truth::Unknown => result = Truth::Unknown,
            _ => {}
        }
    }
    result
}

impl CfgPredicate {
    /// True when the predicate holds only if the bare option `key` is set: the
    /// option itself, or an `all` with a term that requires it. `any`, `not` and
    /// every other shape never do, so `cfg(any(test, feature = "x"))` and
    /// `cfg(not(test))` are not test-only. Syntactic, like `canonical`.
    /// source: issue #354.
    pub(crate) fn requires_option(&self, key: &str) -> bool {
        match self {
            CfgPredicate::Option {
                key: k,
                value: None,
            } => k == key,
            CfgPredicate::All(items) => items.iter().any(|item| item.requires_option(key)),
            _ => false,
        }
    }

    /// The canonical form: nested `all`/`any` of the same kind flattened,
    /// duplicates removed, terms sorted, a one-term `all`/`any` unwrapped and
    /// `not(not(x))` folded to `x`. Two spellings of one condition therefore
    /// compare equal, and the order of `all(a, b)` never makes two twins look
    /// different. It is syntactic only: no boolean simplification beyond this.
    pub(crate) fn canonical(&self) -> CfgPredicate {
        match self {
            CfgPredicate::All(items) => Self::join(items, true),
            CfgPredicate::Any(items) => Self::join(items, false),
            CfgPredicate::Not(inner) => match inner.canonical() {
                CfgPredicate::Not(twice) => *twice,
                other => CfgPredicate::Not(Box::new(other)),
            },
            leaf => leaf.clone(),
        }
    }

    fn join(items: &[CfgPredicate], is_all: bool) -> CfgPredicate {
        let mut flat: Vec<CfgPredicate> = Vec::new();
        for item in items {
            match item.canonical() {
                CfgPredicate::All(inner) if is_all => flat.extend(inner),
                CfgPredicate::Any(inner) if !is_all => flat.extend(inner),
                other => flat.push(other),
            }
        }
        flat.sort_by_key(CfgPredicate::compact);
        flat.dedup();
        match (flat.len(), is_all) {
            (1, _) => flat.remove(0),
            (_, true) => CfgPredicate::All(flat),
            (_, false) => CfgPredicate::Any(flat),
        }
    }

    /// The compact text of the predicate, used inside an item id
    /// (`src/lib.rs::pick#cfg(not(feature=fast))`). Spaces and quotes are
    /// dropped; a value or a name outside `[A-Za-z0-9_-]` is percent-encoded,
    /// so the text never holds `::`, `.`, `:` or a `#`, and the id parsers
    /// (`strip_seq_suffix`, the `::` splitters) cannot misread it.
    pub(crate) fn compact(&self) -> String {
        match self {
            CfgPredicate::Feature(name) => format!("feature={}", encode(name)),
            CfgPredicate::Option { key, value: None } => encode(key),
            CfgPredicate::Option {
                key,
                value: Some(v),
            } => format!("{}={}", encode(key), encode(v)),
            CfgPredicate::All(items) => Self::compact_list("all", items),
            CfgPredicate::Any(items) => Self::compact_list("any", items),
            CfgPredicate::Not(inner) => format!("not({})", inner.compact()),
        }
    }

    fn compact_list(name: &str, items: &[CfgPredicate]) -> String {
        let parts: Vec<String> = items.iter().map(CfgPredicate::compact).collect();
        format!("{name}({})", parts.join(","))
    }
}

/// Percent-encodes every byte outside `[A-Za-z0-9_-]`.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Parses the argument text of a `cfg` attribute, parentheses included ,
/// `(feature = "x")`, `(all(feature = "x", unix))`. `None` when the text is
/// not one well-formed predicate.
pub(crate) fn parse_cfg_arguments(text: &str) -> Option<CfgPredicate> {
    let tokens = tokenize(&without_comments(text))?;
    let mut parser = Parser { tokens, pos: 0 };
    parser.expect(&Token::Open)?;
    let predicate = parser.predicate()?;
    parser.eat(&Token::Comma);
    parser.expect(&Token::Close)?;
    (parser.pos == parser.tokens.len()).then_some(predicate)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn eat(&mut self, token: &Token) -> bool {
        let matched = self.tokens.get(self.pos) == Some(token);
        if matched {
            self.pos += 1;
        }
        matched
    }

    fn expect(&mut self, token: &Token) -> Option<()> {
        self.eat(token).then_some(())
    }

    fn predicate(&mut self) -> Option<CfgPredicate> {
        let Some(Token::Ident(name)) = self.tokens.get(self.pos).cloned() else {
            return None;
        };
        self.pos += 1;
        if self.eat(&Token::Eq) {
            let Some(Token::Str(value)) = self.tokens.get(self.pos).cloned() else {
                return None;
            };
            self.pos += 1;
            return Some(match name.as_str() {
                "feature" => CfgPredicate::Feature(value),
                _ => CfgPredicate::Option {
                    key: name,
                    value: Some(value),
                },
            });
        }
        if !self.eat(&Token::Open) {
            return Some(CfgPredicate::Option {
                key: name,
                value: None,
            });
        }
        let items = self.list()?;
        match name.as_str() {
            "all" => Some(CfgPredicate::All(items)),
            "any" => Some(CfgPredicate::Any(items)),
            "not" if items.len() == 1 => items
                .into_iter()
                .next()
                .map(|p| CfgPredicate::Not(Box::new(p))),
            _ => None,
        }
    }

    /// A comma-separated predicate list after `(`, trailing comma allowed,
    /// consuming the closing `)`.
    fn list(&mut self) -> Option<Vec<CfgPredicate>> {
        let mut items = Vec::new();
        while !self.eat(&Token::Close) {
            items.push(self.predicate()?);
            if !self.eat(&Token::Comma) {
                self.expect(&Token::Close)?;
                break;
            }
        }
        Some(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled(features: &[&str]) -> BTreeSet<String> {
        features.iter().map(|f| f.to_string()).collect()
    }

    fn eval(text: &str, on: &[&str]) -> Truth {
        parse_cfg_arguments(text)
            .expect("parses")
            .eval(&enabled(on))
    }

    #[test]
    fn a_bare_feature_is_false_exactly_when_the_feature_is_off() {
        assert_eq!(eval("(feature = \"extra\")", &[]), Truth::False);
        assert_eq!(eval("(feature = \"extra\")", &["extra"]), Truth::True);
    }

    #[test]
    fn non_feature_options_are_never_decided() {
        assert_eq!(eval("(test)", &[]), Truth::Unknown);
        assert_eq!(eval("(target_os = \"linux\")", &[]), Truth::Unknown);
    }

    #[test]
    fn all_is_false_when_one_feature_is_off_even_beside_an_unknown() {
        assert_eq!(eval("(all(feature = \"x\", unix))", &[]), Truth::False);
        assert_eq!(eval("(all(feature = \"x\", unix))", &["x"]), Truth::Unknown);
    }

    #[test]
    fn any_is_false_only_when_every_branch_is_a_disabled_feature() {
        assert_eq!(eval("(any(feature = \"x\", unix))", &[]), Truth::Unknown);
        assert_eq!(
            eval("(any(feature = \"x\", feature = \"y\",))", &[]),
            Truth::False
        );
        assert_eq!(
            eval("(any(feature = \"x\", feature = \"y\"))", &["y"]),
            Truth::True
        );
    }

    #[test]
    fn not_of_a_default_feature_is_compiled_out() {
        assert_eq!(eval("(not(feature = \"std\"))", &["std"]), Truth::False);
        assert_eq!(eval("(not(feature = \"std\"))", &[]), Truth::True);
        assert_eq!(eval("(not(test))", &[]), Truth::Unknown);
    }

    #[test]
    fn malformed_predicates_are_rejected_not_guessed() {
        for text in [
            "",
            "(",
            "(feature = )",
            "(not(a, b))",
            "(feature = \"x\") junk",
            "(a::b)",
        ] {
            assert_eq!(parse_cfg_arguments(text), None, "{text}");
        }
    }

    fn compact(text: &str) -> String {
        parse_cfg_arguments(text)
            .expect("parses")
            .canonical()
            .compact()
    }

    #[test]
    fn an_option_keeps_its_name_so_kani_and_miri_twins_stay_distinct() {
        assert_eq!(compact("(kani)"), "kani");
        assert_ne!(compact("(kani)"), compact("(miri)"));
        assert_eq!(compact("(not(kani))"), "not(kani)");
        assert_eq!(compact("(target_os = \"linux\")"), "target_os=linux");
    }

    #[test]
    fn canonical_flattens_sorts_dedups_and_unwraps() {
        assert_eq!(compact("(all(b, all(a, b)))"), "all(a,b)");
        assert_eq!(compact("(any(any(x), y))"), "any(x,y)");
        assert_eq!(compact("(all(unix))"), "unix");
        assert_eq!(compact("(not(not(kani)))"), "kani");
        assert_eq!(compact("(all(a, b))"), compact("(all(b, a))"));
    }

    #[test]
    fn a_feature_is_written_with_its_key_and_a_value_is_percent_encoded() {
        assert_eq!(compact("(not(feature = \"fast\"))"), "not(feature=fast)");
        assert_eq!(compact("(feature = \"a.b:c\")"), "feature=a%2Eb%3Ac");
    }

    #[test]
    fn the_compact_text_never_holds_an_id_separator() {
        let text = compact("(all(feature = \"x::y\", target_os = \"a#b.c\"))");
        for bad in ["::", ".", "#", " ", "\""] {
            assert!(!text.contains(bad), "{text} holds {bad}");
        }
    }

    fn compact_of(text: &str) -> String {
        parse_cfg_arguments(text)
            .expect("parses")
            .canonical()
            .compact()
    }

    #[test]
    fn a_comment_inside_a_cfg_does_not_change_the_gate() {
        let plain = compact_of("(all(feature = \"a\", unix))");
        let line = compact_of("(all(feature = \"a\", // why\n unix))");
        let block = compact_of("(all(feature = \"a\", /* why /* nested */ */ unix))");
        assert_eq!(plain, "all(feature=a,unix)");
        assert_eq!(line, plain);
        assert_eq!(block, plain);
    }

    #[test]
    fn a_comment_marker_inside_a_string_is_text() {
        assert_eq!(
            without_comments("(feature = \"a//b\") // gone"),
            "(feature = \"a//b\")  "
        );
        assert_eq!(compact_of("(feature = \"a//b\")"), "feature=a%2F%2Fb");
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string() {
        let parsed = parse_cfg_arguments("(feature = \"a\\\"b\")").expect("parses");
        assert_eq!(parsed, CfgPredicate::Feature("a\"b".to_string()));
        assert_eq!(parsed.compact(), "feature=a%22b");
        let escaped_backslash = parse_cfg_arguments("(feature = \"a\\\\\")").expect("parses");
        assert_eq!(escaped_backslash, CfgPredicate::Feature("a\\".to_string()));
    }

    #[test]
    fn an_unterminated_string_or_an_unknown_escape_does_not_parse() {
        assert!(parse_cfg_arguments("(feature = \"a)").is_none());
        assert!(parse_cfg_arguments("(feature = \"a\\qb\")").is_none());
    }
}
