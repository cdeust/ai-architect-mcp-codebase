// cfg_expr — parses a `#[cfg(...)]` predicate and evaluates it against a set of
// enabled Cargo features (issue #291).
//
// Layer: pure, no I/O. Only `feature = "..."` leaves are decidable from
// `cargo metadata`; every other leaf (`test`, `unix`, `target_os = "..."`,
// `debug_assertions`, …) depends on the build invocation and is `Unknown`.
// Evaluation is three-valued (Kleene logic), so a predicate is reported
// `False` only when the feature leaves alone force it false whatever the
// unknown leaves turn out to be: `all(feature = "x", unix)` with `x` off is
// `False`; `any(feature = "x", unix)` with `x` off is `Unknown`. The caller
// flags a module only on `False` — a compiled module is never flagged.
// source: The Rust Reference, "Conditional compilation" (configuration
// predicates `all`, `any`, `not`, key-value options).

use std::collections::BTreeSet;

/// A parsed configuration predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CfgPredicate {
    Feature(String),
    /// Any option other than `feature = "..."` — not decidable here.
    Other,
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

impl CfgPredicate {
    /// Evaluates against the features the build enables.
    pub(crate) fn eval(&self, enabled: &BTreeSet<String>) -> Truth {
        match self {
            CfgPredicate::Feature(f) if enabled.contains(f) => Truth::True,
            CfgPredicate::Feature(_) => Truth::False,
            CfgPredicate::Other => Truth::Unknown,
            CfgPredicate::All(items) => fold(items, enabled, Truth::False, Truth::True),
            CfgPredicate::Any(items) => fold(items, enabled, Truth::True, Truth::False),
            CfgPredicate::Not(inner) => match inner.eval(enabled) {
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
    enabled: &BTreeSet<String>,
    absorbing: Truth,
    empty: Truth,
) -> Truth {
    let mut result = empty;
    for item in items {
        match item.eval(enabled) {
            t if t == absorbing => return absorbing,
            Truth::Unknown => result = Truth::Unknown,
            _ => {}
        }
    }
    result
}

/// Parses the argument text of a `cfg` attribute, parentheses included —
/// `(feature = "x")`, `(all(feature = "x", unix))`. `None` when the text is
/// not one well-formed predicate.
pub(crate) fn parse_cfg_arguments(text: &str) -> Option<CfgPredicate> {
    let tokens = tokenize(text)?;
    let mut parser = Parser { tokens, pos: 0 };
    parser.expect(&Token::Open)?;
    let predicate = parser.predicate()?;
    parser.eat(&Token::Comma);
    parser.expect(&Token::Close)?;
    (parser.pos == parser.tokens.len()).then_some(predicate)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Str(String),
    Eq,
    Open,
    Close,
    Comma,
}

fn tokenize(text: &str) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' | ')' | ',' | '=' => {
                chars.next();
                tokens.push(match c {
                    '(' => Token::Open,
                    ')' => Token::Close,
                    ',' => Token::Comma,
                    _ => Token::Eq,
                });
            }
            '"' => {
                chars.next();
                let literal: String = chars.by_ref().take_while(|&c| c != '"').collect();
                tokens.push(Token::Str(literal));
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut ident = String::new();
                while let Some(&c) = chars.peek().filter(|c| c.is_alphanumeric() || **c == '_') {
                    ident.push(c);
                    chars.next();
                }
                tokens.push(Token::Ident(ident));
            }
            _ => return None,
        }
    }
    Some(tokens)
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
                _ => CfgPredicate::Other,
            });
        }
        if !self.eat(&Token::Open) {
            return Some(CfgPredicate::Other);
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
}
