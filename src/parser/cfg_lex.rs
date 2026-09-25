// cfg_lex: the lexer of a `#[cfg(...)]` argument list (issue #353): comment
// removal, string literals with Rust's escapes, and the token stream the
// predicate parser in `cfg_expr` reads. Pure, no I/O.
// source: The Rust Reference, "Comments" (block comments nest) and "String
// literals" (the quote and byte escapes).

use std::iter::Peekable;
use std::str::Chars;

/// `text` with every `//` and `/* */` comment (block comments nest, as in Rust)
/// replaced by one space. A comment marker inside a string literal is text, not
/// a comment. Adding a comment to a `#[cfg(..)]` must never rename an item id
/// (issue #353), so identity is computed on the comment-free text.
pub(crate) fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek().copied()) {
            ('"', _) => {
                out.push(c);
                copy_string_rest(&mut chars, &mut out);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
                out.push(' ');
            }
            ('/', Some('*')) => {
                chars.next();
                skip_block_comment(&mut chars);
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Copies the rest of a string literal (its opening quote already copied) to
/// `out`, up to and including the closing quote; `\"` does not close it.
fn copy_string_rest(chars: &mut Peekable<Chars>, out: &mut String) {
    let mut escaped = false;
    for c in chars.by_ref() {
        out.push(c);
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            break;
        }
    }
}

/// Consumes a block comment whose `/*` is already consumed, nested ones too.
fn skip_block_comment(chars: &mut Peekable<Chars>) {
    let mut depth = 1;
    while let Some(c) = chars.next() {
        match (c, chars.peek().copied()) {
            ('/', Some('*')) => {
                chars.next();
                depth += 1;
            }
            ('*', Some('/')) => {
                chars.next();
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Token {
    Ident(String),
    Str(String),
    Eq,
    Open,
    Close,
    Comma,
}

/// The body of a string literal whose opening quote is already consumed, with
/// Rust's escapes decoded (`\"`, `\\`, `\n`, `\r`, `\t`, `\0`, `\'`, `\xHH`,
/// `\u{H..}`), so `feature = "a\"b"` is one string and an escaped quote does not
/// end it. `None` for an unterminated string or an escape Rust does not have.
fn string_literal(chars: &mut Peekable<Chars>) -> Option<String> {
    let mut literal = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(literal),
            '\\' => literal.push(match chars.next()? {
                '"' => '"',
                '\\' => '\\',
                '\'' => '\'',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '0' => '\0',
                'x' => {
                    let hex: String = chars.by_ref().take(2).collect();
                    char::from(u8::from_str_radix(&hex, 16).ok().filter(|b| *b < 0x80)?)
                }
                'u' => {
                    if chars.next()? != '{' {
                        return None;
                    }
                    let hex: String = chars.by_ref().take_while(|&c| c != '}').collect();
                    char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?
                }
                _ => return None,
            }),
            c => literal.push(c),
        }
    }
}

pub(super) fn tokenize(text: &str) -> Option<Vec<Token>> {
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
                tokens.push(Token::Str(string_literal(&mut chars)?));
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
