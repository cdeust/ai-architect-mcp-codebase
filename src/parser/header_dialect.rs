// parser::header_dialect — which grammar a `.h` header is parsed with (#399).
//
// `.h` is shared by C, C++ and Objective-C, and `Language::from_extension`
// maps it to C. The rule applied here:
//   - `language: "cpp"` or `"objc"` given: every `.h` gets that grammar. The
//     caller said what the tree is; before #399 the walk dropped the headers
//     of such a tree entirely.
//   - `language: "c"` given: C, as before.
//   - no language given: decided per file. C++ when the header itself holds a
//     construct the C grammar has no production for (`namespace`, `template <`,
//     a `class` definition, an access specifier, `::`), read outside comments,
//     literals, `[[...]]` attributes, GCC asm operand lists and any `#if`
//     group that tests `__cplusplus`. C otherwise.
//
// Why per file rather than per project: a C++ project commonly vendors C
// libraries whose headers must keep the C walker (prototypes, linkage), and a
// header-only C++ library has no `.cpp` file to vote with. Why constructs
// rather than the C grammar's error count: C headers routinely wrap their
// declarations in `#ifdef __cplusplus / extern "C" {`, which is not C syntax,
// so an error count would move exactly those headers to C++ (asserted by
// `a_c_header_with_a_cplusplus_guard_is_not_error_free_under_c`).

use super::Language;

/// The grammar for a `.h` file, given the index's language filter and the
/// header's text. See the module doc for the rule.
pub fn header_language(filter: Option<Language>, source: &str) -> Language {
    match filter {
        Some(lang @ (Language::Cpp | Language::ObjC)) => lang,
        Some(_) => Language::C,
        None if declares_cpp(source) => Language::Cpp,
        None => Language::C,
    }
}

/// True when the extension is the one shared by C, C++ and Objective-C.
pub fn is_shared_header(ext: &str) -> bool {
    ext == "h"
}

/// True when a language filter makes the walk keep `.h` files: the three
/// languages whose trees use them.
pub fn filter_keeps_headers(filter: Language) -> bool {
    matches!(filter, Language::C | Language::Cpp | Language::ObjC)
}

/// True when the header holds a C++-only construct outside the regions the
/// module doc excludes.
fn declares_cpp(source: &str) -> bool {
    let tokens = strip_attributes(tokenize(source));
    let in_asm = asm_operands(&tokens);
    (0..tokens.len()).any(|i| !in_asm[i] && cpp_construct_at(&tokens, i))
}

/// Marks the tokens inside the parentheses of a GCC extended asm statement
/// (`asm`, `__asm` or `__asm__`, optional `volatile` / `goto` / `inline`
/// qualifiers, then `( ... )`). Its operand lists are separated by `:`, so
/// `"dsb" ::: "memory"`, `: "r"(v) :: out` or `:::: out` spell `::` in plain
/// C, even before a name (an `asm goto` label). Outside those parentheses a
/// `::` before a name has no C reading: `struct D : ::Base`, `case X: ::f();`.
fn asm_operands(tokens: &[Tok]) -> Vec<bool> {
    let mut inside = vec![false; tokens.len()];
    let mut i = 0;
    while i < tokens.len() {
        if !matches!(tokens[i], Tok::Ident("asm" | "__asm" | "__asm__")) {
            i += 1;
            continue;
        }
        let mut open = i + 1;
        while matches!(tokens.get(open), Some(Tok::Ident(q)) if ASM_QUALIFIERS.contains(q)) {
            open += 1;
        }
        i = match tokens.get(open) {
            Some(Tok::Punct('(')) => mark_parenthesised(tokens, open, &mut inside),
            _ => open,
        };
    }
    inside
}

/// Qualifiers GCC accepts between `asm` and its `(`.
/// source: https://gcc.gnu.org/onlinedocs/gcc/Extended-Asm.html ("asm
/// asm-qualifiers ( AssemblerTemplate ..."), plus the `__volatile__` spelling.
const ASM_QUALIFIERS: &[&str] = &["volatile", "__volatile__", "__volatile", "goto", "inline"];

/// Marks every token from the `(` at `open` to its matching `)` and returns
/// the index after it (the end of the tokens when it never closes).
fn mark_parenthesised(tokens: &[Tok], open: usize, inside: &mut [bool]) -> usize {
    let mut depth = 0usize;
    for (k, tok) in tokens.iter().enumerate().skip(open) {
        inside[k] = true;
        match tok {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => {
                depth -= 1;
                if depth == 0 {
                    return k + 1;
                }
            }
            _ => {}
        }
    }
    tokens.len()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok<'a> {
    Ident(&'a str),
    Scope,
    Punct(char),
}

/// The C++-only constructs: `::` before a name, `namespace x` / `namespace {`,
/// `template <`, `class X {` / `class X :` (with an optional `final`), and
/// `public:` / `private:` / `protected:`.
fn cpp_construct_at(tokens: &[Tok], i: usize) -> bool {
    let at = |k: usize| tokens.get(i + k);
    match tokens[i] {
        Tok::Scope => names_a_scope(tokens, i),
        Tok::Ident("namespace") => matches!(at(1), Some(Tok::Ident(_) | Tok::Punct('{'))),
        Tok::Ident("template") => at(1) == Some(&Tok::Punct('<')),
        Tok::Ident("class") => class_definition_follows(&tokens[i + 1..]),
        Tok::Ident("public" | "private" | "protected") => at(1) == Some(&Tok::Punct(':')),
        _ => false,
    }
}

/// `::` qualifies a name when an identifier follows it. The colons of GCC asm
/// operand lists never reach here: `declares_cpp` skips the tokens inside an
/// asm statement (`asm_operands`).
fn names_a_scope(tokens: &[Tok], i: usize) -> bool {
    matches!(tokens.get(i + 1), Some(Tok::Ident(_)))
}

/// `X {`, `X :`, `X final {` or `X final :` after `class`.
fn class_definition_follows(rest: &[Tok]) -> bool {
    let body = |t: Option<&Tok>| matches!(t, Some(Tok::Punct('{' | ':')));
    match rest {
        [Tok::Ident(_), Tok::Ident("final"), next, ..] => body(Some(next)),
        [Tok::Ident(_), next, ..] => body(Some(next)),
        _ => false,
    }
}

/// Drops every token between `[[` and the matching `]]`: a C23 attribute such
/// as `[[gnu::unused]]` spells `::` without being C++.
fn strip_attributes(tokens: Vec<Tok>) -> Vec<Tok> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut depth = 0usize;
    let mut i = 0;
    while i < tokens.len() {
        let pair = (tokens.get(i), tokens.get(i + 1));
        if pair == (Some(&Tok::Punct('[')), Some(&Tok::Punct('['))) {
            depth += 1;
            i += 2;
        } else if depth > 0 && pair == (Some(&Tok::Punct(']')), Some(&Tok::Punct(']'))) {
            depth -= 1;
            i += 2;
        } else {
            if depth == 0 {
                out.push(tokens[i].clone());
            }
            i += 1;
        }
    }
    out
}

/// Tokens of the source outside comments, string and character literals,
/// preprocessor lines, and any `#if` group that tests `__cplusplus`.
fn tokenize(source: &str) -> Vec<Tok<'_>> {
    let mut lexer = Lexer {
        src: source,
        bytes: source.as_bytes(),
        pos: 0,
        groups: Vec::new(),
    };
    let mut out = Vec::new();
    while let Some(tok) = lexer.next_token() {
        if !lexer.in_cplusplus_group() {
            out.push(tok);
        }
    }
    out
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// One entry per open `#if` group: whether its condition names `__cplusplus`.
    groups: Vec<bool>,
}

impl<'a> Lexer<'a> {
    fn in_cplusplus_group(&self) -> bool {
        self.groups.iter().any(|&g| g)
    }

    fn next_token(&mut self) -> Option<Tok<'a>> {
        loop {
            let c = *self.bytes.get(self.pos)?;
            if c.is_ascii_whitespace() {
                self.pos += 1;
            } else if c == b'#' && self.at_line_start() {
                self.directive();
            } else if !self.skip_comment_or_literal() {
                return Some(self.token());
            }
        }
    }

    fn at_line_start(&self) -> bool {
        self.bytes[..self.pos]
            .iter()
            .rev()
            .take_while(|&&b| b != b'\n')
            .all(|b| b.is_ascii_whitespace())
    }

    /// Consumes one comment or literal at `pos`; false when there is none.
    fn skip_comment_or_literal(&mut self) -> bool {
        let rest = &self.bytes[self.pos..];
        if rest.starts_with(b"//") {
            self.pos = self.find_from(self.pos, b"\n").unwrap_or(self.bytes.len());
        } else if rest.starts_with(b"/*") {
            self.pos = self
                .find_from(self.pos + 2, b"*/")
                .map_or(self.bytes.len(), |e| e + 2);
        } else if matches!(rest[0], b'"' | b'\'') {
            self.pos = self.literal_end(rest[0]);
        } else {
            return false;
        }
        true
    }

    fn literal_end(&self, quote: u8) -> usize {
        let mut i = self.pos + 1;
        while i < self.bytes.len() && self.bytes[i] != quote && self.bytes[i] != b'\n' {
            i += if self.bytes[i] == b'\\' { 2 } else { 1 };
        }
        (i + 1).min(self.bytes.len())
    }

    fn token(&mut self) -> Tok<'a> {
        let start = self.pos;
        let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        if is_ident(self.bytes[start]) {
            while self.pos < self.bytes.len() && is_ident(self.bytes[self.pos]) {
                self.pos += 1;
            }
            return Tok::Ident(&self.src[start..self.pos]);
        }
        if self.bytes[start..].starts_with(b"::") {
            self.pos += 2;
            return Tok::Scope;
        }
        let ch = self.src[start..].chars().next().unwrap_or(' ');
        self.pos += ch.len_utf8();
        Tok::Punct(ch)
    }

    /// Reads one logical preprocessor line (with `\` continuations) and
    /// updates the `#if` group stack.
    fn directive(&mut self) {
        let start = self.pos;
        let mut end = self.find_from(start, b"\n").unwrap_or(self.bytes.len());
        while end > start && self.bytes[end - 1] == b'\\' && end < self.bytes.len() {
            end = self.find_from(end + 1, b"\n").unwrap_or(self.bytes.len());
        }
        self.pos = end;
        let line = self.src[start + 1..end].trim_start();
        let names_cplusplus = line.contains("__cplusplus");
        let word = line
            .split(|c: char| !c.is_ascii_alphabetic())
            .next()
            .unwrap_or("");
        match word {
            "if" | "ifdef" | "ifndef" => self.groups.push(names_cplusplus),
            "elif" | "elifdef" | "elifndef" => {
                if let Some(top) = self.groups.last_mut() {
                    *top |= names_cplusplus;
                }
            }
            "endif" => {
                self.groups.pop();
            }
            _ => {}
        }
    }

    fn find_from(&self, from: usize, needle: &[u8]) -> Option<usize> {
        self.bytes
            .get(from..)?
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| from + p)
    }
}

#[cfg(test)]
#[path = "header_dialect_tests.rs"]
mod tests;
