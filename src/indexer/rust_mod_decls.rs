// rust_mod_decls — the file-module declarations (`mod name;`) of one Rust
// source file, with the `#[cfg]` and `#[path]` attributes on each (issue #291).
//
// Layer: pure parse, no I/O. Only top-level, bodiless declarations are
// returned: they are the edges of the module tree that span files. A
// declaration nested in an inline `mod a { mod b; }` is skipped, so a file
// reached only through one is never classified (under-reporting, never a
// false flag). `cfg_attr` is not expanded, for the same reason.
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
        _ => {}
    }
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
    fn an_unparseable_cfg_makes_the_gate_undecidable() {
        let decls = mod_decls("#[cfg(feature = \"a\")]\n#[cfg(weird::path)]\nmod m;\n");
        assert_eq!(decls[0].cfg, None);
    }
}
