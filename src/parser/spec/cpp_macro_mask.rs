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

/// `source` with every call of a macro-style name that stands alone as a
/// statement at declaration level (class, namespace or file scope) blanked,
/// same length, or `None` when there is none. `ETL_STATIC_ASSERT((N > 0), "..")`
/// in a class body reads as a declaration with a `>` in it and loses the class.
/// A call in a function body is never touched: its arguments hold real calls.
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
            if let Some(j) = statement_call_close(&toks, &closes, i) {
                blank.push((t.start, toks[j].end));
                i = j;
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

/// The index of the `)` of the call whose name is token `name`, when the call
/// is a whole statement (followed by `;`).
fn statement_call_close(toks: &[Lexed], closes: &[Option<usize>], name: usize) -> Option<usize> {
    let close = closes.get(name + 1).copied().flatten()?;
    (toks.get(close + 1).map(|n| n.tok) == Some(Tok::Punct(b';'))).then_some(close)
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
mod tests {
    use super::mask_specifier_macros;

    // source: tree-sitter-cpp 0.23.4 on ETLCPP 7d604f2e `string.h`: each shape
    // below (trailing `ETL_NOEXCEPT` / `ETL_OVERRIDE`, leading `ETL_CONSTANT` /
    // `ETL_EXPLICIT_STRING_FROM_CHAR`) produced an ERROR node.
    fn masked(src: &str) -> String {
        mask_specifier_macros(src).unwrap_or_else(|| src.to_string())
    }

    #[test]
    fn blanks_trailing_specifiers_after_a_parameter_list() {
        let src = "void f(int a) ETL_NOEXCEPT ETL_OVERRIDE { }";
        let m = masked(src);
        assert_eq!(m.len(), src.len());
        assert!(!m.contains("ETL_"));
        assert!(m.starts_with("void f(int a)"));
        assert!(m.ends_with("{ }"));
    }

    #[test]
    fn blanks_leading_specifiers_before_a_declaration() {
        let m = masked(
            "struct S { ETL_CONSTANT size_t N = 3; ETL_EXPLICIT_STRING_FROM_CHAR S(int x); };",
        );
        assert!(!m.contains("ETL_"));
        assert!(m.contains("size_t N = 3;"));
        assert!(m.contains("S(int x);"));
    }

    #[test]
    fn keeps_function_like_macros_and_plain_words() {
        for src in [
            "void f() { ETL_ASSERT(x, ETL_ERROR(e)); }",
            "int x = LIMIT_MAX;",
            "void f() { return SOME_VALUE; }",
            "HANDLE h;",
            "SECURITY_ATTRIBUTES sa;",
            "// ETL_NOEXCEPT in a comment\nint a;",
            "const char* s = \"ETL_NOEXCEPT\";",
            "#define ETL_NOEXCEPT noexcept\nint a;",
        ] {
            assert_eq!(mask_specifier_macros(src), None, "{src}");
        }
    }

    #[test]
    fn keeps_offsets_and_lines() {
        let src = "class A {\n  void f() ETL_NOEXCEPT;\n};\n";
        let m = masked(src);
        assert_eq!(m.len(), src.len());
        assert_eq!(m.lines().count(), src.lines().count());
    }

    #[test]
    fn blanks_a_macro_after_a_specifier_keyword_and_after_if() {
        let m = masked("struct S { static ETL_CONSTANT size_t N = 3; };");
        assert!(!m.contains("ETL_CONSTANT") && m.contains("static"));
        let m = masked("void f() { if ETL_IF_CONSTEXPR (x) { g(); } }");
        assert!(!m.contains("ETL_IF_CONSTEXPR") && m.contains("if "));
    }

    #[test]
    fn blanks_a_macro_call_at_declaration_level_only() {
        use super::blank_scope_macro_calls as blank;
        let class =
            "class A : public B {\npublic:\n  ETL_STATIC_ASSERT((N > 0U), \"zero\");\n  int x;\n};";
        let out = blank(class).expect("class-level call is blanked");
        assert!(!out.contains("ETL_STATIC_ASSERT") && out.contains("int x;"));
        assert_eq!(out.len(), class.len());
        // In a function body the arguments hold real calls: never touched.
        assert_eq!(blank("void f() { ETL_ASSERT(g(), \"m\"); }"), None);
    }

    #[test]
    fn collapses_conditionals_to_their_first_branch() {
        use super::collapse_conditionals as collapse;
        let src = "#if A\nvirtual void r() X\n#else\nvoid r()\n#endif\n{ }\n";
        let out = collapse(src).expect("conditional collapsed");
        assert_eq!(out.len(), src.len());
        assert_eq!(out.lines().count(), src.lines().count());
        assert!(out.contains("virtual void r() X"));
        assert!(!out.contains("void r()\n") && !out.contains("#"));
        assert!(out.contains("{ }"));
        assert_eq!(collapse("int a;\n"), None);
        // Whole declarations in each branch: the `#else` holds definitions.
        assert_eq!(
            collapse("#if A\nstruct X {};\n#else\nstruct Y {};\n#endif\n"),
            None
        );
    }

    #[test]
    fn an_unclosed_call_is_left_alone() {
        let src = "class A {\n  A_B( ;\n  A_C(x);\n};\n";
        let out = super::blank_scope_macro_calls(src).expect("the closed call is blanked");
        assert!(out.contains("A_B( ;"), "{out}");
        assert!(!out.contains("A_C"), "{out}");
        assert!(super::blank_scope_macro_calls("A_B( ;\nA_C( ;\n").is_none());
    }

    #[test]
    fn parens_match_in_one_pass() {
        let toks = super::lex("f(g(1), (2)) ) (");
        let closes = super::paren_closes(&toks);
        let at = |n: usize| {
            toks.iter()
                .enumerate()
                .filter(|(_, t)| t.tok == super::Tok::Punct(b'('))
                .nth(n)
                .map(|(i, _)| closes[i])
        };
        assert_eq!(at(0), Some(Some(10)));
        assert_eq!(at(3), Some(None));
    }
}
