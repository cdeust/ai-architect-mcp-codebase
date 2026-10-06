//! Blanks the specifier macros that make tree-sitter-cpp give up on a header
//! (issue #410): an unknown `ETL_NOEXCEPT`, `ETL_OVERRIDE` or `ETL_CONSTANT`
//! turns a declaration, then the whole file, into an ERROR node, and the
//! namespace and class scope are lost. The mask writes spaces of the same
//! length, so offsets and lines are unchanged; the caller keeps the masked
//! parse only when it has fewer ERROR/MISSING nodes.

/// What the mask needs to know about a lexical token.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tok {
    Ident,
    Punct(u8),
    /// `::`
    Scope,
    /// `->`
    Arrow,
    /// A whole preprocessor directive line.
    Directive,
    /// A literal, a number or anything else that is no part of a declaration head.
    Other,
}

struct Lexed {
    tok: Tok,
    start: usize,
    end: usize,
}

/// End of the run of bytes from `i` on that satisfy `keep`.
fn run_end(b: &[u8], mut i: usize, keep: impl Fn(u8) -> bool) -> usize {
    while i < b.len() && keep(b[i]) {
        i += 1;
    }
    i
}

/// End of the block comment that opens at `i`.
fn block_comment_end(b: &[u8], mut i: usize) -> usize {
    i += 2;
    while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
        i += 1;
    }
    (i + 2).min(b.len())
}

/// End of the preprocessor line that starts at `i`, backslash continuations included.
fn directive_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i] != b'\n' {
        if b[i] == b'\\' && b.get(i + 1) == Some(&b'\n') {
            i += 1;
        }
        i += 1;
    }
    i
}

/// End of the string or character literal that opens at `i`.
fn quoted_end(b: &[u8], mut i: usize) -> usize {
    let quote = b[i];
    i += 1;
    while i < b.len() && b[i] != quote && b[i] != b'\n' {
        i += if b[i] == b'\\' { 2 } else { 1 };
    }
    (i + 1).min(b.len())
}

fn lex(source: &str) -> Vec<Lexed> {
    let b = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        let (tok, end) = match c {
            b'\n' => {
                line_start = true;
                i += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' => {
                i += 1;
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                i = run_end(b, i, |x| x != b'\n');
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = block_comment_end(b, i);
                continue;
            }
            b'#' if line_start => (Tok::Directive, directive_end(b, i)),
            b'"' | b'\'' => (Tok::Other, quoted_end(b, i)),
            _ if c.is_ascii_alphabetic() || c == b'_' => (
                Tok::Ident,
                run_end(b, i, |x| x.is_ascii_alphanumeric() || x == b'_'),
            ),
            _ if c.is_ascii_digit() => (
                Tok::Other,
                run_end(b, i, |x| {
                    x.is_ascii_alphanumeric() || x == b'_' || x == b'.'
                }),
            ),
            _ => match (c, b.get(i + 1)) {
                (b':', Some(b':')) => (Tok::Scope, i + 2),
                (b'-', Some(b'>')) => (Tok::Arrow, i + 2),
                _ => (Tok::Punct(c), i + 1),
            },
        };
        line_start = false;
        out.push(Lexed { tok, start: i, end });
        i = end;
    }
    out
}

/// Written like a macro: upper case, digits and `_`, at least one `_`.
fn macro_style(word: &str) -> bool {
    word.len() >= 3
        && word.contains('_')
        && word.as_bytes()[0].is_ascii_uppercase()
        && word
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
}

/// Specifier keywords after which a declaration head goes on.
const SPECIFIERS: [&str; 8] = [
    "static",
    "inline",
    "constexpr",
    "virtual",
    "explicit",
    "friend",
    "extern",
    "const",
];

/// Keywords that start a statement, never a type: a macro before one is a statement
/// of its own (`DECL_E return e.m();`), not a specifier of a declaration, and
/// blanking it would hide the statement from the reader of declarations.
const STATEMENT_KEYWORDS: [&str; 12] = [
    "return",
    "throw",
    "goto",
    "break",
    "continue",
    "if",
    "for",
    "while",
    "do",
    "switch",
    "co_return",
    "co_yield",
];

/// True when a token after `prev` starts or continues a declaration head: after
/// `;`, `{`, `}`, `>`, an access label's `:`, a preprocessor line, a specifier
/// keyword, or at the start of the file.
fn opens_declaration(source: &str, prev: Option<&Lexed>) -> bool {
    let Some(prev) = prev else {
        return true;
    };
    match prev.tok {
        Tok::Directive
        | Tok::Punct(b';')
        | Tok::Punct(b'{')
        | Tok::Punct(b'}')
        | Tok::Punct(b'>')
        | Tok::Punct(b':') => true,
        Tok::Ident => SPECIFIERS.contains(&&source[prev.start..prev.end]),
        _ => false,
    }
}

/// `source` with the macro specifiers blanked, same length, or `None` when none
/// was found.
///
/// A token is blanked when it is written like a macro, is not a call (`NAME(`),
/// and either trails a parameter list (follows `)` or another blanked macro) or
/// leads a declaration (follows `opens_declaration`, then an identifier and
/// another identifier, `::`, `(`, `<`, `&` or `*`: a type and a name, or a constructor).
pub(crate) fn mask_specifier_macros(source: &str) -> Option<String> {
    let toks = lex(source);
    let mut masked: Vec<(usize, usize)> = Vec::new();
    let mut last_was_masked = false;
    for (i, t) in toks.iter().enumerate() {
        if t.tok != Tok::Ident || !macro_style(&source[t.start..t.end]) {
            last_was_masked = false;
            continue;
        }
        let prev_tok = i.checked_sub(1).map(|p| &toks[p]);
        let prev = prev_tok.map(|p| p.tok);
        let next = toks.get(i + 1).map(|n| n.tok);
        let after_if =
            prev_tok.is_some_and(|p| p.tok == Tok::Ident && &source[p.start..p.end] == "if");
        if next == Some(Tok::Punct(b'(')) && !after_if {
            last_was_masked = false;
            continue;
        }
        let trailing = prev == Some(Tok::Punct(b')')) || last_was_masked || after_if;
        let leading = opens_declaration(source, prev_tok)
            && next == Some(Tok::Ident)
            && !toks
                .get(i + 1)
                .is_some_and(|n| STATEMENT_KEYWORDS.contains(&&source[n.start..n.end]))
            && matches!(
                toks.get(i + 2).map(|n| n.tok),
                Some(Tok::Ident)
                    | Some(Tok::Scope)
                    | Some(Tok::Punct(b'('))
                    | Some(Tok::Punct(b'<'))
                    | Some(Tok::Punct(b'&'))
                    | Some(Tok::Punct(b'*'))
            );
        if trailing || leading {
            masked.push((t.start, t.end));
            last_was_masked = true;
        } else {
            last_was_masked = false;
        }
    }
    if masked.is_empty() {
        return None;
    }
    let mut bytes = source.as_bytes().to_vec();
    for (start, end) in masked {
        bytes[start..end].fill(b' ');
    }
    // Only ASCII identifier bytes were replaced by ASCII spaces.
    String::from_utf8(bytes).ok()
}

/// One open `#if`: where its current branch began and what is known so far.
struct Conditional {
    tokens_at_branch_start: usize,
    split: bool,
    else_from: Option<usize>,
    directives: Vec<(usize, usize)>,
}

/// `source` keeping the first branch of every `#if`/`#ifdef`/`#ifndef` that
/// cuts a declaration, and blanking its `#elif`/`#else` branches and directives,
/// same length, or `None` when no conditional cuts one. A conditional cuts a
/// declaration when a branch has code and does not end on `;`, `{`, `}` or `:`:
/// `#if A virtual void r() #else void r() #endif { .. }` is unparsable as is.
/// A conditional whose branches are whole declarations is left alone: dropping
/// its `#else` would drop the definitions in it.
fn collapse_conditionals(source: &str) -> Option<String> {
    let mut blank: Vec<(usize, usize)> = Vec::new();
    let mut stack: Vec<Conditional> = Vec::new();
    let mut tokens = 0usize;
    let mut last: Option<Tok> = None;
    for t in &lex(source) {
        if t.tok != Tok::Directive {
            tokens += 1;
            last = Some(t.tok);
            continue;
        }
        match directive_word(source, t) {
            "if" | "ifdef" | "ifndef" => stack.push(Conditional {
                tokens_at_branch_start: tokens,
                split: false,
                else_from: None,
                directives: vec![(t.start, t.end)],
            }),
            word @ ("elif" | "else" | "endif") => {
                on_branch_directive(&mut stack, &mut blank, word, t, (tokens, last));
            }
            _ => {}
        }
    }
    blank_out(source, blank)
}

/// Whether a branch that ends on `last` ends a whole declaration.
fn ends_a_declaration(last: Option<Tok>) -> bool {
    matches!(
        last,
        None | Some(Tok::Punct(b';'))
            | Some(Tok::Punct(b'{'))
            | Some(Tok::Punct(b'}'))
            | Some(Tok::Punct(b':'))
    )
}

/// The word after the `#` of a directive token (`if`, `else`, `endif`, ...).
fn directive_word<'a>(source: &'a str, t: &Lexed) -> &'a str {
    source[t.start + 1..t.end]
        .trim_start()
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
}

/// An `#elif`, `#else` or `#endif` closes the current branch of the innermost
/// open conditional; `seen` is the token count and the last token so far. A
/// split conditional has its directives and its `#else` part queued in `blank`.
fn on_branch_directive(
    stack: &mut Vec<Conditional>,
    blank: &mut Vec<(usize, usize)>,
    word: &str,
    t: &Lexed,
    seen: (usize, Option<Tok>),
) {
    let (tokens, last) = seen;
    let Some(top) = stack.last_mut() else {
        return;
    };
    if tokens > top.tokens_at_branch_start && !ends_a_declaration(last) {
        top.split = true;
    }
    top.tokens_at_branch_start = tokens;
    top.directives.push((t.start, t.end));
    if word != "endif" {
        if top.else_from.is_none() {
            top.else_from = Some(t.start);
        }
        return;
    }
    let Some(done) = stack.pop() else {
        return;
    };
    if done.split {
        blank.extend(done.directives);
        if let Some(from) = done.else_from {
            blank.push((from, t.end));
        }
    }
}

/// `source` with the byte ranges blanked (newlines kept), or `None` when there
/// is no range.
fn blank_out(source: &str, ranges: Vec<(usize, usize)>) -> Option<String> {
    if ranges.is_empty() {
        return None;
    }
    let mut bytes = source.as_bytes().to_vec();
    for (start, end) in ranges {
        for b in &mut bytes[start..end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
    String::from_utf8(bytes).ok()
}

/// `source` with every macro statement at declaration level (class, namespace or
/// file scope) blanked, same length, or `None` when there is none. A macro
/// statement is a call of a macro-style name followed by `;`
/// (`ETL_STATIC_ASSERT((N > 0), "..");` reads as a declaration with a `>` in it and
/// loses the class), or one that needs no `;`: a call followed by another macro
/// statement or by `}`, and a bare macro-style name followed by `}`. ETL's
/// `ETL_DECLARE_ENUM_TYPE(T, int) ETL_ENUM_TYPE(A, "a") ETL_END_ENUM_TYPE }` is that
/// shape (issue #412): left in, the macros make the class swallow the rest of the
/// header. A call in a function body is never touched: its arguments hold real calls.
fn blank_scope_macro_calls(source: &str) -> Option<String> {
    let toks = lex(source);
    let closes = paren_closes(&toks);
    let mut scopes: Vec<bool> = Vec::new();
    // `statement` starts after the last `;`, `{`, `}` or directive; `head` also
    // restarts after an access label's `:`.
    let mut statement: usize = 0;
    let mut head: usize = 0;
    let mut blank: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        match t.tok {
            Tok::Punct(b';') | Tok::Punct(b'}') | Tok::Directive => {
                statement = i + 1;
                head = i + 1;
            }
            Tok::Punct(b':') => head = i + 1,
            Tok::Punct(b'{') => {
                scopes.push(opens_scope(source, &toks[statement.min(i)..i]));
                statement = i + 1;
                head = i + 1;
            }
            _ => {}
        }
        if t.tok == Tok::Punct(b'}') {
            scopes.pop();
        }
        let at_declaration_level = scopes.last().copied().unwrap_or(true);
        if t.tok == Tok::Ident
            && at_declaration_level
            && head == i
            && macro_style(&source[t.start..t.end])
        {
            if let Some(j) = macro_statement_end(source, &toks, &closes, i) {
                blank.push((t.start, toks[j].end));
                i = j;
                // A statement without `;` leaves the next macro at the head.
                statement = j + 1;
                head = j + 1;
            }
        }
        i += 1;
    }
    blank_out(source, blank)
}

/// Whether a `{` after `intro` opens a class, struct, union or namespace body.
fn opens_scope(source: &str, intro: &[Lexed]) -> bool {
    !intro.iter().any(|x| x.tok == Tok::Punct(b'('))
        && intro.iter().any(|x| {
            x.tok == Tok::Ident
                && matches!(
                    &source[x.start..x.end],
                    "class" | "struct" | "union" | "namespace"
                )
        })
}

/// For every `(` token the index of its matching `)`, in one pass; `None` for a
/// `(` never closed and for every other token.
fn paren_closes(toks: &[Lexed]) -> Vec<Option<usize>> {
    let mut closes = vec![None; toks.len()];
    let mut open: Vec<usize> = Vec::new();
    for (j, x) in toks.iter().enumerate() {
        match x.tok {
            Tok::Punct(b'(') => open.push(j),
            Tok::Punct(b')') => {
                if let Some(o) = open.pop() {
                    closes[o] = Some(j);
                }
            }
            _ => {}
        }
    }
    closes
}

/// True when `next` ends a macro statement that has no `;`: it is a `}` or another
/// macro-style name.
fn ends_a_bare_statement(source: &str, next: Option<&Lexed>) -> bool {
    next.is_some_and(|n| {
        n.tok == Tok::Punct(b'}') || (n.tok == Tok::Ident && macro_style(&source[n.start..n.end]))
    })
}

/// The index of the last token of the macro statement that starts at token `name`:
/// the `)` of a call followed by `;`, `}` or another macro-style name, or `name`
/// itself when it is a bare macro-style name followed by `}`. `None` otherwise.
fn macro_statement_end(
    source: &str,
    toks: &[Lexed],
    closes: &[Option<usize>],
    name: usize,
) -> Option<usize> {
    if toks.get(name + 1).map(|n| n.tok) != Some(Tok::Punct(b'(')) {
        let next = toks.get(name + 1);
        return (next.map(|n| n.tok) == Some(Tok::Punct(b'}'))).then_some(name);
    }
    let close = closes.get(name + 1).copied().flatten()?;
    let after = toks.get(close + 1);
    (after.map(|n| n.tok) == Some(Tok::Punct(b';')) || ends_a_bare_statement(source, after))
        .then_some(close)
}

/// The rewrites of `source` worth a second parse, least invasive first: the
/// macro specifiers blanked, then that and the declaration-level macro calls
/// blanked, then that and the conditionals collapsed.
pub(crate) fn variants(source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = source.to_string();
    for step in [
        mask_specifier_macros,
        blank_scope_macro_calls,
        collapse_conditionals,
    ] {
        if let Some(next) = step(&current) {
            out.push(next.clone());
            current = next;
        }
    }
    out
}

#[cfg(test)]
#[path = "cpp_macro_mask_tests.rs"]
mod tests;
