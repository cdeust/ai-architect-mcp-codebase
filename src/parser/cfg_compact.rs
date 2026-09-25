// cfg_compact: reads back the compact text of a `#[cfg]` predicate (issue #353).
//
// `CfgPredicate::compact` writes the text that ends up in the id of a twin item
// and in its `cfg_gate` column (`not(feature=fast)`, `all(feature=a,unix)`).
// The resolver and the indexer need the predicate again, to decide which twin a
// build compiles, so this is the inverse of `compact`: for every predicate `p`,
// `parse_compact(&p.canonical().compact()) == Some(p.canonical())` (a test pins
// it). A text that is not the compact form of one predicate, such as a gate kept
// as raw source because it did not parse, gives `None`: the caller then knows
// nothing about the item, which is the safe answer.

use super::cfg_expr::CfgPredicate;

/// The predicate a compact gate text spells, or `None` when it spells none.
pub(crate) fn parse_compact(text: &str) -> Option<CfgPredicate> {
    let mut reader = Reader {
        bytes: text.as_bytes(),
        pos: 0,
    };
    let predicate = reader.predicate()?;
    (reader.pos == reader.bytes.len()).then_some(predicate)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn word(&mut self) -> Option<String> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'%'))
        {
            self.pos += 1;
        }
        decode(&self.bytes[start..self.pos]).filter(|w| !w.is_empty())
    }

    fn predicate(&mut self) -> Option<CfgPredicate> {
        let name = self.word()?;
        match self.peek() {
            Some(b'(') => {
                self.pos += 1;
                let items = self.list()?;
                match (name.as_str(), items.len()) {
                    ("all", _) => Some(CfgPredicate::All(items)),
                    ("any", _) => Some(CfgPredicate::Any(items)),
                    ("not", 1) => items
                        .into_iter()
                        .next()
                        .map(|p| CfgPredicate::Not(Box::new(p))),
                    _ => None,
                }
            }
            Some(b'=') => {
                self.pos += 1;
                let value = self.word()?;
                Some(if name == "feature" {
                    CfgPredicate::Feature(value)
                } else {
                    CfgPredicate::Option {
                        key: name,
                        value: Some(value),
                    }
                })
            }
            _ => Some(CfgPredicate::Option {
                key: name,
                value: None,
            }),
        }
    }

    /// The comma-separated predicates up to and including the closing `)`.
    fn list(&mut self) -> Option<Vec<CfgPredicate>> {
        let mut items = Vec::new();
        if self.peek() == Some(b')') {
            self.pos += 1;
            return Some(items);
        }
        loop {
            items.push(self.predicate()?);
            match self.peek()? {
                b',' => self.pos += 1,
                b')' => {
                    self.pos += 1;
                    return Some(items);
                }
                _ => return None,
            }
        }
    }
}

/// Reverses `cfg_expr::encode`: `%XX` becomes the byte, the rest is kept.
fn decode(word: &[u8]) -> Option<String> {
    let mut out = Vec::with_capacity(word.len());
    let mut i = 0;
    while i < word.len() {
        if word[i] == b'%' {
            let hex = std::str::from_utf8(word.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(word[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::super::cfg_expr::parse_cfg_arguments;
    use super::*;

    fn canonical(source: &str) -> CfgPredicate {
        parse_cfg_arguments(source)
            .expect("a predicate")
            .canonical()
    }

    #[test]
    fn the_compact_text_of_every_predicate_reads_back_to_it() {
        for source in [
            "(feature = \"fast\")",
            "(not(feature = \"fast\"))",
            "(unix)",
            "(target_os = \"linux\")",
            "(all(feature = \"a\", not(kani), any(unix, windows)))",
            "(any(feature = \"a\", feature = \"b\"))",
            "(feature = \"we ird:name.x\")",
            "(all())",
            "(not(not(kani)))",
        ] {
            let p = canonical(source);
            assert_eq!(parse_compact(&p.compact()), Some(p), "{source}");
        }
    }

    #[test]
    fn text_that_is_not_one_compact_predicate_reads_as_none() {
        for text in [
            "", "not()", "not(a,b)", "all(a", "all(a,)", "a::b", "a b", "feature=", "x=%zz",
            "any(a))", "a=b=c",
        ] {
            assert_eq!(parse_compact(text), None, "{text:?}");
        }
    }
}
