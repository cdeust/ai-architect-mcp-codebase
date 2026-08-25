// lsp_resolver::pass — the running tally of one LSP resolution pass.
//
// Split from `lsp_resolver` when that file crossed the §4.1 500-line cap.
// Owning the counters in one type is what lets `skipped` be derived from an
// invariant rather than accumulated across branches — see `LspPass`.

use super::edges::{try_add_lsp_edge, SiteContext};
use super::sites::UnresolvedCallSite;
use crate::graph_store::GraphStore;
use crate::lsp_client::{self, LspResolutionResult};

/// Running tally of one LSP resolution pass.
///
/// `skipped` is DERIVED — `total - resolved - failed` — rather than
/// accumulated. Why (review finding 7): the previous accounting added, on
/// budget exhaustion, `sites.len() - (resolved_count + failed_count)`, mixing
/// the CURRENT file's site count with counters accumulated across ALL files.
/// Once a second file was reached the subtraction saturated to zero, so a
/// truncated pass reported the same `skipped_count: 0` as an exhaustive one
/// and a caller could not tell a complete answer from a partial one. The
/// identity holds by construction: a site is skipped exactly when the pass
/// never obtained an answer for it, whatever the reason — file unreadable,
/// `didOpen` refused, request timed out, or budget exhausted before its turn.
pub(super) struct LspPass {
    total: u64,
    resolved: u64,
    failed: u64,
    /// §10.4 invariant: `is_resolved` flips when the callee resolved to a
    /// graph target, whichever pass found it — mirrors `resolver::calls`.
    /// Without this a rerun re-queries every LSP-resolved site.
    newly_resolved: Vec<String>,
}

impl LspPass {
    pub(super) fn new(total_sites: usize) -> Self {
        LspPass {
            total: total_sites as u64,
            resolved: 0,
            failed: 0,
            newly_resolved: Vec::new(),
        }
    }

    /// Folds one `textDocument/definition` outcome into the tally, inserting
    /// the edge when the definition maps onto a graph node.
    pub(super) fn record(
        &mut self,
        store: &GraphStore,
        site: &UnresolvedCallSite,
        definition: Result<Option<lsp_client::DefinitionResult>, String>,
        ctx: &SiteContext<'_>,
    ) {
        match definition {
            Ok(Some(def)) => {
                if try_add_lsp_edge(store, site, &def, ctx) {
                    self.resolved += 1;
                    self.newly_resolved.push(site.id.clone());
                } else {
                    self.failed += 1;
                }
            }
            Ok(None) => self.failed += 1,
            // A timed-out request produced no answer, so it is neither
            // resolved nor failed and falls into `skipped` by the identity.
            // Matched on the sentinel `lsp_client` raises, never on the word
            // "timeout" appearing somewhere in the text: a server error that
            // merely mentions it is a real failure, and counting it as skipped
            // hid it from `failed_count`.
            Err(e) if lsp_client::is_lsp_timeout(&e) => {}
            Err(_) => self.failed += 1,
        }
    }

    pub(super) fn mark_resolved(&self, store: &GraphStore) -> Result<(), String> {
        let ids: Vec<&str> = self.newly_resolved.iter().map(|s| s.as_str()).collect();
        store.mark_nodes_resolved("CallSite", &ids)
    }

    pub(super) fn into_result(self, elapsed_ms: u64) -> LspResolutionResult {
        LspResolutionResult {
            resolved_count: self.resolved,
            failed_count: self.failed,
            skipped_count: self.total.saturating_sub(self.resolved + self.failed),
            elapsed_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_store::NODE_FUNCTION;
    use std::collections::HashMap;

    fn site(id: &str) -> UnresolvedCallSite {
        UnresolvedCallSite {
            id: id.to_string(),
            caller_qn: "src/a.rs::caller".to_string(),
            caller_label: "Function".to_string(),
            callee_name: "x".to_string(),
            file_path: "src/a.rs".to_string(),
            line: 1,
            col: 1,
        }
    }

    #[test]
    fn skipped_accounts_for_every_site_that_got_no_answer() {
        // Review finding 7 (regression). The pass used to ADD, on budget
        // exhaustion, `sites.len() - (resolved + failed)` — the CURRENT
        // file's site count against counters accumulated over ALL files. Once
        // a second file was reached that subtraction saturated to zero, so a
        // truncated pass reported `skipped_count: 0`, indistinguishable from
        // an exhaustive one. A caller had no way to know the answer was
        // partial. `skipped` is now the identity `total - resolved - failed`.
        let dir = tempfile::Builder::new()
            .prefix("lsp_pass_counts")
            .tempdir()
            .expect("tempdir");
        let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open");
        store.create_schema().expect("schema");
        let index = HashMap::new();
        let root = dir.path().to_path_buf();
        let ctx = SiteContext {
            node_index: &index,
            canonical_root: &root,
        };

        // Ten sites in the pass; only four ever get an answer.
        let mut pass = LspPass::new(10);
        pass.record(&store, &site("s1"), Ok(None), &ctx);
        pass.record(&store, &site("s2"), Ok(None), &ctx);
        pass.record(&store, &site("s3"), Err("broken pipe".to_string()), &ctx);
        // A timed-out request produced no answer at all: neither resolved nor
        // failed, so the identity counts it as skipped. Uses the sentinel
        // `lsp_client` actually raises rather than a hand-written string that
        // merely contains the word.
        pass.record(
            &store,
            &site("s4"),
            Err(format!(
                "{} reading LSP header",
                lsp_client::LSP_TIMEOUT_PREFIX
            )),
            &ctx,
        );

        let out = pass.into_result(0);
        assert_eq!(out.resolved_count, 0);
        assert_eq!(out.failed_count, 3, "three sites were answered negatively");
        assert_eq!(
            out.skipped_count, 7,
            "the six sites never reached plus the timed-out one are skipped"
        );
        assert_eq!(
            out.resolved_count + out.failed_count + out.skipped_count,
            10,
            "the three counters must partition the pass"
        );
    }

    /// A store holding one caller Function and one unresolved CallSite on it.
    fn store_with_one_unresolved_site(root: &std::path::Path) -> GraphStore {
        let store = GraphStore::open_or_create(&root.join("db")).expect("open");
        store.create_schema().expect("schema");
        store
            .insert_node(
                NODE_FUNCTION,
                &[
                    ("id", "'src/a.rs::caller'"),
                    ("name", "'caller'"),
                    ("qualified_name", "'src/a.rs::caller'"),
                    ("start_line", "1"),
                    ("end_line", "3"),
                    ("visibility", "'pub'"),
                    ("is_async", "false"),
                ],
            )
            .expect("insert caller");
        store
            .insert_node(
                crate::graph_store::NODE_CALL_SITE,
                &[
                    ("id", "'src/a.rs::caller::call@1:1'"),
                    ("callee_name", "'x'"),
                    ("line", "1"),
                    ("col", "1"),
                    ("is_resolved", "false"),
                    ("language", "'rust'"),
                ],
            )
            .expect("insert site");
        store
    }

    #[test]
    fn an_unmapped_definition_leaves_the_site_unresolved() {
        // §10.4: `is_resolved` flips for whichever pass found the callee, and
        // only then — a definition that maps onto no indexed node is a
        // failure, so a later pass must still see the site as open.
        let dir = tempfile::Builder::new()
            .prefix("lsp_pass_mark")
            .tempdir()
            .expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        std::fs::write(root.join("src/b.rs"), "pub fn target() {}").expect("write");
        let store = store_with_one_unresolved_site(&root);

        let index = super::super::sites::build_node_position_index(&store).expect("index");
        let ctx = SiteContext {
            node_index: &index,
            canonical_root: &root,
        };
        let mut pass = LspPass::new(1);
        let def = lsp_client::DefinitionResult {
            uri: lsp_client::path_to_file_uri(&root.join("src/b.rs")),
            start_line: 0,
            start_col: 0,
        };
        pass.record(
            &store,
            &site("src/a.rs::caller::call@1:1"),
            Ok(Some(def)),
            &ctx,
        );
        pass.mark_resolved(&store).expect("mark");
        let still_unresolved = store
            .execute_query("MATCH (cs:CallSite) WHERE cs.is_resolved = false RETURN cs.id")
            .expect("query");
        assert_eq!(
            still_unresolved.rows.len(),
            1,
            "an unmapped definition is a failure, not a resolution"
        );
    }

    /// B.6. `skipped` used to be decided by `e.contains("timeout")`, so any
    /// server error whose text merely mentioned the word — a symbol named
    /// `timeout`, a message quoting one — was moved out of `failed_count` and
    /// into `skipped`, hiding a real failure. Classification is now an exact
    /// match on the sentinel `lsp_client` raises.
    #[test]
    fn only_a_real_timeout_counts_as_skipped() {
        let dir = tempfile::Builder::new()
            .prefix("lsp_pass_timeout_classification")
            .tempdir()
            .expect("tempdir");
        let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open");
        store.create_schema().expect("schema");
        let index = HashMap::new();
        let root = dir.path().to_path_buf();
        let ctx = SiteContext {
            node_index: &index,
            canonical_root: &root,
        };

        let mut pass = LspPass::new(2);
        // Mentions the word, is NOT a timeout: a genuine failure.
        pass.record(
            &store,
            &site("s1"),
            Err("no definition for symbol `timeout`".to_string()),
            &ctx,
        );
        // The real thing.
        pass.record(
            &store,
            &site("s2"),
            Err(format!(
                "{} reading LSP header",
                lsp_client::LSP_TIMEOUT_PREFIX
            )),
            &ctx,
        );

        let out = pass.into_result(0);
        assert_eq!(
            out.failed_count, 1,
            "an error that merely mentions the word must stay a failure"
        );
        assert_eq!(out.skipped_count, 1, "only the sentinel counts as skipped");
    }
}
