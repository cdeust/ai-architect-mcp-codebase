// clustering::impact_other_owner — which open call sites that share the target's
// bare name cannot be the target (issue #392).
//
// `get_impact` counts the unresolved `CallSite` nodes naming the target by its
// bare identifier. For a common name (`new`, `from`) that count took in
// `io::BufWriter::new` and `Vec::new` for `TaskSet::new`. A call of a method
// reaches it only through its owner, so a site whose spelling names ANOTHER owner
// cannot be the target; the count drops it and reports how many it dropped. A
// site whose owner is unknown stays: the count is a lower bound and never loses
// a site that could still be the target.
//
// Scope: a Rust target that is a method of a struct or enum the repository
// defines. Every other target keeps every site, because there the spelling does
// not decide the owner: a C++ or Java receiver of another type may inherit the
// method, and a C or C++ callee is recorded as its last segment only, so it
// carries no qualifier to compare.
//
// source: The Rust Reference, "Method-call expressions" (the receiver type, then
// the types it dereferences to, decide the method) and "Path expressions" (a
// `Type::item` path names an item of that type or a trait it implements).

use std::collections::HashMap;

use crate::call_evidence::callee_names_type;
use crate::graph_store::callsite_reasons::REASON_EXTERNAL_CALLEE;
use crate::graph_store::{cypher_str, GraphStore, NODE_ENUM, NODE_METHOD, NODE_STRUCT};
use crate::parser::generic_args::strip_generics;

/// The `language` of a Rust call site and method.
const RUST: &str = "rust";

/// Types of the standard library that hold no way to reach a type of the
/// repository by method call: the primitives and the collections, none of which
/// dereferences to a user type (`Vec<T>` to `[T]`, `String` to `str`). A smart
/// pointer (`Box`, `Rc`, `Arc`, `Cow`, guards) is absent on purpose: it
/// dereferences to the type it wraps, and a hint has its generics stripped.
/// source: doc.rust-lang.org/std, the `Deref` implementors; The Rust Reference,
/// "Primitive Types".
const CLOSED_STD_TYPES: [&str; 27] = [
    "bool",
    "char",
    "str",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "f32",
    "f64",
    "String",
    "Vec",
    "VecDeque",
    "HashMap",
    "HashSet",
    "BTreeMap",
    "BTreeSet",
    "BinaryHeap",
    "Option",
    "Result",
];

/// How one open call site spells its callee.
pub(super) struct SiteSpelling<'a> {
    pub(super) callee: &'a str,
    pub(super) receiver_hint: &'a str,
    pub(super) reason: &'a str,
    pub(super) language: &'a str,
}

/// The decision "could this site be the target?" for one target, with the facts
/// of the graph it reads memoised (one query per type name, however many sites).
pub(super) struct OtherOwnerFilter<'a> {
    store: &'a GraphStore,
    owner: String,
    trait_name: Option<String>,
    repo_type: HashMap<String, bool>,
    aliased: HashMap<String, bool>,
    derefs: HashMap<String, bool>,
}

impl<'a> OtherOwnerFilter<'a> {
    /// The filter for the target `esc` (a `cypher_str`-quoted id or qualified
    /// name), or `None` when the spelling of a site cannot decide its owner: the
    /// target is not a Rust method, or its owner is not a struct or enum of the
    /// repository (a trait declaration, a blanket `impl<T> Tr for T`, an impl for
    /// `&Foo` or `Box<Foo>`, a primitive).
    pub(super) fn for_target(store: &'a GraphStore, esc: &str) -> Option<Self> {
        let cypher = format!(
            "MATCH (m:{NODE_METHOD}) WHERE (m.id = {esc} OR m.qualified_name = {esc}) \
             AND m.language = {rust} RETURN m.receiver_type, m.trait_name LIMIT 1",
            rust = cypher_str(RUST)
        );
        let rows = store.execute_query(&cypher).ok()?.rows;
        let row = rows.first()?;
        let owner_qn = strip_generics(row.first()?.trim());
        let trait_name = row
            .get(1)
            .map(|t| last_segment(strip_generics(t.trim())).to_string())
            .filter(|t| !t.is_empty());
        let concrete = [NODE_STRUCT, NODE_ENUM].iter().any(|label| {
            exists(
                store,
                &format!(
                    "MATCH (n:{label}) WHERE n.qualified_name = {} RETURN n.id LIMIT 1",
                    cypher_str(owner_qn)
                ),
            ) == Some(true)
        });
        let owner = last_segment(owner_qn);
        (concrete && !owner.is_empty()).then(|| Self {
            store,
            owner: owner.to_string(),
            trait_name,
            repo_type: HashMap::new(),
            aliased: HashMap::new(),
            derefs: HashMap::new(),
        })
    }

    /// True when `site` provably names another owner than the target's.
    ///
    /// precondition: the site's bare callee name equals the target's.
    /// postcondition: `false` whenever the site could still reach the target: an
    /// unknown receiver, a qualifier that is `Self`, the owner, an alias or a
    /// trait of the target, a receiver that may dereference to the owner.
    pub(super) fn excludes(&mut self, site: &SiteSpelling) -> bool {
        if site.language != RUST {
            return false;
        }
        if site.callee.contains('.') {
            return self.receiver_is_another_type(site.receiver_hint);
        }
        match callee_names_type(site.callee) {
            Some(qualifier) => self.path_is_another_owner(qualifier, site.reason),
            None => false,
        }
    }

    /// `Q::name`: the qualifier `Q` names another owner. A type of the repository
    /// other than the owner cannot reach the target. A qualifier the resolver
    /// proved outside the repository (`external_callee`) cannot reach an inherent
    /// method either; for a method of a trait impl it is kept unless it is a
    /// closed std type, because `Default::default()` and `fmt::Display::fmt(..)`
    /// name the trait and do reach the impl.
    fn path_is_another_owner(&mut self, qualifier: &str, reason: &str) -> bool {
        if self.is_self_or_owner(qualifier) || self.is_aliased(qualifier) {
            return false;
        }
        if self.is_repo_type(qualifier) {
            return true;
        }
        if reason != REASON_EXTERNAL_CALLEE {
            return false;
        }
        match &self.trait_name {
            None => true,
            Some(_) => CLOSED_STD_TYPES.contains(&qualifier),
        }
    }

    /// `x.name` with `x` of the written type `hint`: another owner unless the
    /// hint is empty, unreadable, the owner, an alias, or a type that may
    /// dereference to the owner.
    fn receiver_is_another_type(&mut self, hint: &str) -> bool {
        let Some(name) = hint_type_name(hint) else {
            return false;
        };
        if self.is_self_or_owner(name) || self.is_aliased(name) {
            return false;
        }
        if self.is_repo_type(name) {
            return !self.type_derefs(name);
        }
        CLOSED_STD_TYPES.contains(&name)
    }

    fn is_self_or_owner(&self, name: &str) -> bool {
        matches!(name, "Self" | "self") || name == self.owner
    }

    /// True when the repository defines a struct or enum called `name`.
    fn is_repo_type(&mut self, name: &str) -> bool {
        let store = self.store;
        *self.repo_type.entry(name.to_string()).or_insert_with(|| {
            [NODE_STRUCT, NODE_ENUM].iter().any(|label| {
                exists(
                    store,
                    &format!(
                        "MATCH (n:{label}) WHERE n.name = {} RETURN n.id LIMIT 1",
                        cypher_str(name)
                    ),
                ) == Some(true)
            })
        })
    }

    /// True when `name` is bound to another type by a `type` alias or a
    /// `use .. as name`; a query that fails reads as aliased (the site stays).
    fn is_aliased(&mut self, name: &str) -> bool {
        let store = self.store;
        *self.aliased.entry(name.to_string()).or_insert_with(|| {
            ["TypeAlias", "Import"].iter().any(|label| {
                let column = if *label == "Import" { "alias" } else { "name" };
                exists(
                    store,
                    &format!(
                        "MATCH (n:{label}) WHERE n.{column} = {} RETURN n.id LIMIT 1",
                        cypher_str(name)
                    ),
                ) != Some(false)
            })
        })
    }

    /// True when some impl of a `Deref` trait is written for a type called
    /// `name`; a query that fails reads as dereferencing (the site stays).
    fn type_derefs(&mut self, name: &str) -> bool {
        let store = self.store;
        *self.derefs.entry(name.to_string()).or_insert_with(|| {
            exists(
                store,
                &format!(
                    "MATCH (m:{NODE_METHOD}) WHERE m.trait_name CONTAINS 'Deref' \
                     AND m.receiver_type CONTAINS {} RETURN m.id LIMIT 1",
                    cypher_str(&format!("::{name}"))
                ),
            ) != Some(false)
        })
    }
}

/// Splits the open sites naming the target into those that could still be it and
/// the count of those that provably name another owner. A row is
/// `[id, unresolved_reason, callee_name, language, receiver_hint?]`.
///
/// postcondition: kept rows are a sub-sequence of `rows` in order; `kept.len() +
/// excluded == rows.len()`; when `for_target` gives no filter, nothing is dropped.
pub(super) fn drop_other_owner_sites(
    store: &GraphStore,
    esc: &str,
    rows: Vec<Vec<String>>,
) -> (Vec<Vec<String>>, u64) {
    let Some(mut filter) = OtherOwnerFilter::for_target(store, esc) else {
        return (rows, 0);
    };
    let total = rows.len();
    let cell = |row: &[String], i: usize| row.get(i).cloned().unwrap_or_default();
    let kept: Vec<Vec<String>> = rows
        .into_iter()
        .filter(|row| {
            let (reason, callee, language, hint) =
                (cell(row, 1), cell(row, 2), cell(row, 3), cell(row, 4));
            !filter.excludes(&SiteSpelling {
                callee: &callee,
                receiver_hint: &hint,
                reason: &reason,
                language: &language,
            })
        })
        .collect();
    let excluded = (total - kept.len()) as u64;
    (kept, excluded)
}

/// `Some(true)` when the query returns a row, `Some(false)` when it returns
/// none, `None` when it fails.
fn exists(store: &GraphStore, cypher: &str) -> Option<bool> {
    let rows = store.execute_query(cypher).ok()?.rows;
    Some(rows.iter().any(|r| !r.is_empty()))
}

/// The type a receiver hint names, or `None` for a hint that is not a plain
/// (possibly qualified, borrowed, generic) type name: `&mut b::Set<T>` gives
/// `Set`; `dyn Tr`, `[u8; 4]`, `(A, B)` and `` give `None`.
fn hint_type_name(hint: &str) -> Option<&str> {
    let mut rest = hint.trim();
    loop {
        let trimmed = rest
            .strip_prefix('&')
            .or_else(|| rest.strip_prefix("mut "))
            .map(str::trim_start);
        match trimmed {
            Some(next) => rest = next,
            None => break,
        }
    }
    let plain = strip_generics(rest);
    let plain_path = !plain.is_empty()
        && plain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':');
    plain_path
        .then(|| last_segment(plain))
        .filter(|s| !s.is_empty())
}

fn last_segment(s: &str) -> &str {
    s.rsplit("::").next().unwrap_or(s)
}

#[cfg(test)]
#[path = "impact_other_owner_tests.rs"]
mod tests;
