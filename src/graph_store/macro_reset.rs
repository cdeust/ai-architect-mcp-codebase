// macro_reset — undo what a previous macro-expansion pass wrote (issue #339).
//
// The macro layer derives its rows from the CallSite nodes alone, so a run
// re-derives all of them. Rows written by an older build can name targets the
// current rules would not (`writeln!` on an `io::Write` also pointing at
// `fmt::Write::write_fmt`), and `EdgeBuffer` treats an existing row as already
// resolved, so they would survive every re-run. Clearing them first makes the
// graph a function of the current rules and the source, not of what ran before.

use super::cypher_str;
use super::schema::{NODE_STDLIB_SYMBOL, REL_TABLES};
use super::GraphStore;

// source: resolver_layers — every macro-expansion method label starts with it.
const MACRO_METHOD_PREFIX: &str = "macro-expansion";

// source: `parser::spec::rust` `call_callee` appends `!` for a
// `macro_invocation`; Ruby keeps the `!` of `user.save!` / `user.update!` in
// its callee name (measured), so the language decides, not the `!`.
const MACRO_LANGUAGE: &str = "rust";
const MACRO_SUFFIX: &str = "!";

/// True for a `CallSite` that is a Rust macro invocation. The one Rust-side
/// statement of the rule; `rust_macro_site_predicate` is the same rule as
/// Cypher, and a test ties the two.
pub(crate) fn is_rust_macro_site(callee: &str, language: &str) -> bool {
    language == MACRO_LANGUAGE && callee.ends_with(MACRO_SUFFIX)
}

/// The `WHERE` predicate over a `cs:CallSite` for `is_rust_macro_site`.
pub(crate) fn rust_macro_site_predicate() -> String {
    format!(
        "cs.language = {} AND cs.callee_name ENDS WITH {}",
        cypher_str(MACRO_LANGUAGE),
        cypher_str(MACRO_SUFFIX)
    )
}

impl GraphStore {
    /// Deletes every macro-expansion row of every `Calls_*_StdlibSymbol`
    /// table, and clears `is_resolved` on every Rust macro `CallSite`
    /// (`rust_macro_site_predicate`) so the pass decides it again.
    pub(crate) fn reset_macro_expansion(&self) -> Result<(), String> {
        self.ensure_node_column("CallSite", "is_resolved", "BOOLEAN DEFAULT false")?;
        for &(rel, _, to) in REL_TABLES {
            if to != NODE_STDLIB_SYMBOL || !rel.starts_with("Calls_") {
                continue;
            }
            self.run(&format!(
                "MATCH ()-[r:{rel}]->() WHERE r.resolution_method STARTS WITH {} DELETE r",
                cypher_str(MACRO_METHOD_PREFIX)
            ))?;
        }
        let pred = rust_macro_site_predicate();
        self.run(&format!(
            "MATCH (cs:CallSite) WHERE {pred} SET cs.is_resolved = false"
        ))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Cypher predicate and the Rust one must select the same sites.
    #[test]
    fn the_cypher_predicate_and_the_rust_predicate_select_the_same_sites() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open");
        store.create_schema().expect("schema");
        let sites = [
            ("a", "write!", "rust"),
            ("b", "save!", "ruby"),
            ("c", "helper", "rust"),
            ("d", "save", "ruby"),
        ];
        for (id, callee, lang) in sites {
            let props = [
                ("id", format!("'{id}'")),
                ("callee_name", format!("'{callee}'")),
                ("line", "1".to_string()),
                ("col", "0".to_string()),
                ("language", format!("'{lang}'")),
            ];
            let borrowed: Vec<(&str, &str)> = props.iter().map(|(k, v)| (*k, v.as_str())).collect();
            store.insert_node("CallSite", &borrowed).expect("insert");
        }
        let pred = rust_macro_site_predicate();
        let qr = store
            .execute_query(&format!(
                "MATCH (cs:CallSite) WHERE {pred} RETURN cs.id ORDER BY cs.id"
            ))
            .expect("query");
        let by_cypher: Vec<&str> = qr.rows.iter().map(|r| r[0].as_str()).collect();
        let by_rust: Vec<&str> = sites
            .iter()
            .filter(|(_, c, l)| is_rust_macro_site(c, l))
            .map(|(id, _, _)| *id)
            .collect();
        assert_eq!(by_cypher, by_rust);
        assert_eq!(by_rust, vec!["a"]);
    }
}
