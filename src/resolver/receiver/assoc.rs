// resolver::receiver::assoc: issue #370. `let x = Type::assoc(..)` names the
// type `assoc` belongs to, not the type it returns. A hint read off that form
// is kept only for a candidate whose owner's `assoc` returns `Self`, the owner
// itself or the owner with generic arguments, or when `assoc` is a variant of
// the owner enum (`Response::Refused(..)` builds a `Response`).
//
// The return type is the text the parser stored on the `Method` node
// (`walkers::type_uses::return_type_text`), read once per resolve pass.

use super::*;
use std::collections::HashSet;

/// The suffix a `#[cfg]` twin's qualified name carries (issue #353).
const TWIN_SUFFIX: &str = "#cfg(";

/// Declared return types of methods and qualified names of enum variants, each
/// keyed by the qualified name without a `#[cfg]` twin suffix, so the twins of
/// one item share a key.
#[derive(Default)]
pub(in crate::resolver) struct AssocFacts {
    returns: HashMap<String, Vec<String>>,
    variants: HashSet<String>,
}

impl AssocFacts {
    /// A graph without the `return_type` column or without a `Variant` table
    /// yields no facts, so every `assoc:` hint then finds no candidate.
    pub(in crate::resolver) fn load(store: &GraphStore) -> AssocFacts {
        let mut facts = AssocFacts::default();
        if let Ok(qr) = store.execute_query("MATCH (m:Method) RETURN m.qualified_name, m.return_type")
        {
            for row in qr.rows.iter().filter(|r| r.len() >= 2) {
                facts
                    .returns
                    .entry(without_twin_suffix(&row[0]).to_string())
                    .or_default()
                    .push(row[1].clone());
            }
        }
        if let Ok(qr) = store.execute_query("MATCH (v:Variant) RETURN v.qualified_name") {
            for row in &qr.rows {
                if let Some(qn) = row.first() {
                    facts.variants.insert(without_twin_suffix(qn).to_string());
                }
            }
        }
        facts
    }

    /// True when `assoc` of the type owning `candidate` builds that type: every
    /// method `assoc` of the owner (one, or `#[cfg]` twins that must agree)
    /// returns it, or `assoc` is a variant of the owner.
    pub(in crate::resolver) fn builds_owner(&self, candidate: &SymbolEntry, assoc: &str) -> bool {
        let Some((owner, _)) = candidate.qualified_name.rsplit_once("::") else {
            return false;
        };
        let key = format!("{}::{assoc}", without_twin_suffix(owner));
        let name = strip_generics(owner.rsplit("::").next().unwrap_or(owner));
        match self.returns.get(&key) {
            Some(types) => types.iter().all(|t| returns_the_type(t, name)),
            None => self.variants.contains(&key),
        }
    }
}

fn without_twin_suffix(qn: &str) -> &str {
    qn.split(TWIN_SUFFIX).next().unwrap_or(qn)
}

/// True when `declared` is `Self`, `name` or `name<..>`. Any other text (a
/// wrapper such as `Option<Name>`, `Box<Name>`, `impl Trait`, another type,
/// a qualified path, or no declared type at all) is not the owner.
fn returns_the_type(declared: &str, name: &str) -> bool {
    let t = declared.trim();
    if t == "Self" || t == name {
        return true;
    }
    t.strip_prefix(name)
        .map(str::trim_start)
        .is_some_and(|rest| rest.starts_with('<') && rest.ends_with('>'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(qn: &str) -> SymbolEntry {
        SymbolEntry {
            id: qn.to_string(),
            label: "Method".to_string(),
            qualified_name: qn.to_string(),
        }
    }

    fn facts(returns: &[(&str, &str)], variants: &[&str]) -> AssocFacts {
        let mut f = AssocFacts::default();
        for (qn, t) in returns {
            f.returns
                .entry(without_twin_suffix(qn).to_string())
                .or_default()
                .push(t.to_string());
        }
        f.variants = variants.iter().map(|v| v.to_string()).collect();
        f
    }

    #[test]
    fn self_the_type_and_the_type_with_generics_build_the_owner() {
        for declared in ["Self", "Set", "Set<T>", "Set <u8>"] {
            assert!(returns_the_type(declared, "Set"), "{declared}");
        }
        for declared in [
            "Option<Set>",
            "Result<Set, E>",
            "Box<Set>",
            "impl Debug",
            "Other",
            "SetBuilder",
            "crate::Set",
            "&Self",
            "",
        ] {
            assert!(!returns_the_type(declared, "Set"), "{declared}");
        }
    }

    #[test]
    fn the_owner_of_a_generic_impl_is_read_without_its_arguments() {
        let f = facts(&[("a.rs::Gen<T>::new", "Gen<T>")], &[]);
        assert!(f.builds_owner(&entry("a.rs::Gen<T>::m"), "new"));
    }

    #[test]
    fn cfg_twins_of_assoc_must_all_return_the_type() {
        let agree = facts(
            &[
                ("a.rs::Set::new#cfg(unix)", "Self"),
                ("a.rs::Set::new#cfg(not(unix))", "Set"),
            ],
            &[],
        );
        assert!(agree.builds_owner(&entry("a.rs::Set::m"), "new"));
        let differ = facts(
            &[
                ("a.rs::Set::new#cfg(unix)", "Self"),
                ("a.rs::Set::new#cfg(not(unix))", "Option<Set>"),
            ],
            &[],
        );
        assert!(!differ.builds_owner(&entry("a.rs::Set::m"), "new"));
    }

    #[test]
    fn a_variant_builds_its_enum_and_an_unknown_assoc_builds_nothing() {
        let f = facts(&[], &["a.rs::Resp::Refused"]);
        assert!(f.builds_owner(&entry("a.rs::Resp::m"), "Refused"));
        assert!(!f.builds_owner(&entry("a.rs::Resp::m"), "Missing"));
    }
}
