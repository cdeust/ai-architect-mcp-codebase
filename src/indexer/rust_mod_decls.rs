// rust_mod_decls — the file-module declarations (`mod name;`) of one Rust
// source file, with the `#[cfg]` and `#[path]` attributes on each (issue #291).
//
// Layer: pure parse, no I/O. Only top-level, bodiless declarations are
// returned: they are the edges of the module tree that span files. A
// declaration nested in an inline `mod a { mod b; }` is skipped, so a file
// reached only through one is never classified (under-reporting, never a
// false flag). `cfg_attr(pred, path = "..")` is read as an alternative path
// under `pred` (issue #366); every other `cfg_attr` is ignored.
// source: The Rust Reference, "Modules" (module source filenames, the `path`
// attribute) and "Conditional compilation" (the `cfg` attribute).

use crate::parser::cfg_expr::{self, CfgPredicate};
use tree_sitter::{Node, Parser};

/// One `mod name;` item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModDecl {
    pub name: String,
    /// The `#[path = "..."]` value, when present.
    pub path_attr: Option<String>,
    /// Every `#[cfg(...)]` on the item (they combine as `all`). `None` when
    /// one of them did not parse, so the gate is undecidable.
    pub cfg: Option<Vec<CfgPredicate>>,
    /// The source text of the `cfg` attributes, for the coverage detail.
    pub cfg_text: String,
    /// Every `#[cfg_attr(pred, path = "...")]`: the file the declaration names
    /// when `pred` holds (issue #366). `pred` is `None` when it did not parse
    /// or the `path` sits in a nested `cfg_attr`.
    pub alt_paths: Vec<AltPath>,
}

/// One `path` a `cfg_attr` gives a declaration, with the predicate that gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AltPath {
    pub pred: Option<CfgPredicate>,
    pub path: String,
}

/// Parses `source` and returns its top-level `mod name;` declarations.
/// A source the Rust grammar cannot load yields nothing.
pub(crate) fn mod_decls(source: &str) -> Vec<ModDecl> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut decls = Vec::new();
    let mut attributes: Vec<Node> = Vec::new();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        match child.kind() {
            "attribute_item" => attributes.push(child),
            "line_comment" | "block_comment" => {}
            "mod_item" => {
                if child.child_by_field_name("body").is_none() {
                    decls.extend(mod_decl(child, &attributes, source));
                }
                attributes.clear();
            }
            _ => attributes.clear(),
        }
    }
    decls
}

fn mod_decl(item: Node, attributes: &[Node], source: &str) -> Option<ModDecl> {
    let name = text(item.child_by_field_name("name")?, source).to_string();
    let mut decl = ModDecl {
        name,
        path_attr: None,
        cfg: Some(Vec::new()),
        cfg_text: String::new(),
        alt_paths: Vec::new(),
    };
    for attribute_item in attributes {
        let Some(attribute) = attribute_item.named_child(0) else {
            continue;
        };
        record_attribute(&mut decl, attribute, source, text(*attribute_item, source));
    }
    Some(decl)
}

/// Folds one `attribute` node (`cfg(...)` / `path = "..."`) into `decl`.
fn record_attribute(decl: &mut ModDecl, attribute: Node, source: &str, item_text: &str) {
    let Some(name) = attribute.named_child(0).map(|n| text(n, source)) else {
        return;
    };
    match name {
        "cfg" => {
            let parsed = attribute
                .child_by_field_name("arguments")
                .and_then(|args| cfg_expr::parse_cfg_arguments(text(args, source)));
            match (parsed, decl.cfg.as_mut()) {
                (Some(predicate), Some(all)) => all.push(predicate),
                _ => decl.cfg = None,
            }
            if !decl.cfg_text.is_empty() {
                decl.cfg_text.push(' ');
            }
            decl.cfg_text.push_str(item_text);
        }
        "path" => {
            decl.path_attr = attribute
                .child_by_field_name("value")
                .map(|v| text(v, source).trim_matches('"').to_string());
        }
        "cfg_attr" => {
            if let Some(args) = attribute.child_by_field_name("arguments") {
                decl.alt_paths
                    .extend(cfg_attr_paths(text(args, source), true));
            }
        }
        _ => {}
    }
}

/// The `path = ".."` attributes of a `cfg_attr` argument list `(pred, attr, ..)`,
/// each under `pred`. A `path` inside a nested `cfg_attr` is kept with an
/// unknown predicate, because only the outer one is evaluated.
fn cfg_attr_paths(arguments: &str, outer: bool) -> Vec<AltPath> {
    let inner = arguments
        .trim()
        .strip_prefix('(')
        .and_then(|t| t.strip_suffix(')'))
        .unwrap_or("");
    let parts = split_top_level(inner);
    let Some((first, rest)) = parts.split_first() else {
        return Vec::new();
    };
    let pred = if outer {
        cfg_expr::parse_cfg_arguments(&format!("({first})"))
    } else {
        None
    };
    let mut out = Vec::new();
    for part in rest {
        if let Some(path) = path_value(part) {
            out.push(AltPath {
                pred: pred.clone(),
                path,
            });
        } else if let Some(nested) = part.trim().strip_prefix("cfg_attr") {
            out.extend(cfg_attr_paths(nested, false));
        }
    }
    out
}

/// The value of `path = "..."`, when `part` is that attribute.
fn path_value(part: &str) -> Option<String> {
    let rest = part
        .trim()
        .strip_prefix("path")?
        .trim_start()
        .strip_prefix('=')?;
    let value = rest.trim().strip_prefix('"')?.strip_suffix('"')?;
    Some(value.to_string())
}

/// Splits `text` on the commas outside parentheses and string literals.
fn split_top_level(text: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut start, mut in_str, mut escaped) =
        (Vec::new(), 0i32, 0usize, false, false);
    for (i, c) in text.char_indices() {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(text[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = text[start..].trim();
    if !last.is_empty() {
        parts.push(last);
    }
    parts
}

fn text<'s>(node: Node, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feature_gated_mod_carries_its_parsed_cfg_and_source_text() {
        let decls = mod_decls("#[cfg(feature = \"extra\")]\npub mod extra;\nmod plain;\n");
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].name, "extra");
        assert_eq!(
            decls[0].cfg,
            Some(vec![CfgPredicate::Feature("extra".into())])
        );
        assert_eq!(decls[0].cfg_text, "#[cfg(feature = \"extra\")]");
        assert_eq!(
            (decls[1].name.as_str(), decls[1].cfg.as_deref()),
            ("plain", Some(&[][..]))
        );
    }

    #[test]
    fn inline_modules_and_attributes_of_other_items_are_not_declarations() {
        let decls = mod_decls(
            "#[cfg(feature = \"x\")]\nfn f() {}\nmod inline { mod nested; }\n// c\n#[path = \"p/q.rs\"]\nmod q;\n",
        );
        assert_eq!(decls.len(), 1, "{decls:?}");
        assert_eq!(decls[0].name, "q");
        assert_eq!(decls[0].path_attr.as_deref(), Some("p/q.rs"));
        assert_eq!(decls[0].cfg.as_deref(), Some(&[][..]));
    }

    #[test]
    fn a_cfg_attr_path_is_an_alternative_path_under_its_predicate() {
        let decls = mod_decls(
            "#[cfg_attr(feature = \"fast\", path = \"fast.rs\")]\n\
             #[cfg_attr(not(feature = \"fast\"), allow(dead_code), path = \"slow.rs\")]\n\
             #[cfg_attr(unix, cfg_attr(test, path = \"t.rs\"))]\n\
             #[cfg_attr(unix, allow(dead_code))]\nmod imp;\n",
        );
        let alts: Vec<(Option<CfgPredicate>, &str)> = decls[0]
            .alt_paths
            .iter()
            .map(|a| (a.pred.clone(), a.path.as_str()))
            .collect();
        let fast = CfgPredicate::Feature("fast".into());
        assert_eq!(
            alts,
            [
                (Some(fast.clone()), "fast.rs"),
                (Some(CfgPredicate::Not(Box::new(fast))), "slow.rs"),
                (None, "t.rs"),
            ]
        );
        assert_eq!(decls[0].path_attr, None);
        assert_eq!(decls[0].cfg.as_deref(), Some(&[][..]));
    }

    #[test]
    fn an_unparseable_cfg_makes_the_gate_undecidable() {
        let decls = mod_decls("#[cfg(feature = \"a\")]\n#[cfg(weird::path)]\nmod m;\n");
        assert_eq!(decls[0].cfg, None);
    }
}
