//! Skipping resolve, cluster and the search index on an `analyze_codebase`
//! whose refresh changed nothing (issue #443).
//!
//! # Why
//!
//! A second `analyze_codebase` on an unchanged tree re-parsed nothing but
//! still resolved and clustered the whole graph: 4.76x faster than the first
//! call on ripgrep and 2.28x on this repository's `src/`, against the issue's
//! 5x (`benchmarks/incremental_speed`, `second_call`, #579). Resolve and
//! cluster were 65% and 98% of those second calls.
//!
//! # When the previous phases are reused
//!
//! A finished analysis writes a seal beside the graph (`graph_store::seal`)
//! recording what it ran over. The next call reads it before it opens the
//! graph, and reuses the resolve, cluster and search-index results only when
//! all of these hold:
//! 1. the seal is there: no tool opened the graph for writing since it was
//!    written (`GraphStore::open_or_create` withdraws it), and the analysis
//!    that wrote it finished every phase (it is written last);
//! 2. it was written by this server version, for the same `resolution_param`,
//!    and neither call is `lsp: true` nor this one `full: true`;
//! 3. `file_manifest.json` is byte for byte the one the seal names;
//! 4. this call's index phase reused the graph and its refresh touched no
//!    file (changed, added, deleted or renamed);
//! 5. that refresh wrote the same Cargo facts the sealed resolve ran against,
//!    and every write landed: a `Cargo.toml` edit moves them without moving a
//!    file the walk lists (`CargoFacts::digest`).
//!
//! Otherwise the phases run as before. The seal is taken (read, then removed)
//! before phase 1, so an analysis that fails or is killed leaves none.

use super::index_phase::IndexPhase;
use super::AnalyzeRequest;
use crate::graph_store::seal::{seal_path, withdraw};
use crate::indexer::manifest::{
    hash_bytes, manifest_path, read_regular_capped, MAX_MANIFEST_BYTES,
};
use serde_json::{json, Value};

/// The seal's format; a seal of another version is not read.
const SCHEMA: u64 = 1;

/// A seal holds three digests and three small blocks of counts: well under a
/// kilobyte. Over this bound it is not read, and the phases run.
const SEAL_MAX_BYTES: u64 = 64 << 10;

/// The results of the three derived phases, as the response reports them.
pub(super) struct Derived {
    pub(super) resolve: Value,
    pub(super) cluster: Value,
    pub(super) search_index: Value,
}

/// A seal that passed conditions 1 to 3; `reuse` checks 4 and 5.
pub(super) struct Prior {
    cargo_facts_sha256: String,
    derived: Derived,
}

/// Reads the seal and removes it, before anything opens the graph. `Ok(None)`
/// when there is none or it does not match this call (conditions 1 to 3).
/// `Err` only when it cannot be removed: it would then outlive this call's
/// writes.
pub(super) fn take(req: &AnalyzeRequest) -> Result<Option<Prior>, String> {
    let bytes = read_regular_capped(&seal_path(&req.graph_dir), SEAL_MAX_BYTES);
    withdraw(&req.graph_dir)?;
    if req.want_full || req.enable_lsp {
        return Ok(None);
    }
    let Some(seal) = bytes.and_then(|(b, _)| serde_json::from_slice::<Value>(&b).ok()) else {
        return Ok(None);
    };
    let matches = seal["schema"].as_u64() == Some(SCHEMA)
        && seal["server_version"].as_str() == Some(env!("CARGO_PKG_VERSION"))
        && seal["resolution_param_bits"].as_u64() == Some(req.gamma.to_bits())
        && seal["manifest_sha256"].as_str().is_some()
        && seal["manifest_sha256"].as_str() == manifest_digest(req).as_deref();
    let facts = seal["cargo_facts_sha256"].as_str();
    let derived = derived_from(&seal);
    Ok(match (matches, facts, derived) {
        (true, Some(facts), Some(derived)) => Some(Prior {
            cargo_facts_sha256: facts.to_string(),
            derived,
        }),
        _ => None,
    })
}

/// Conditions 4 and 5: the sealed results, marked reused, or `None`.
pub(super) fn reuse(prior: Option<Prior>, index: &IndexPhase) -> Option<Derived> {
    let prior = prior?;
    let unchanged = index.touched == Some(0);
    let same_facts = index.cargo_facts_digest.as_deref() == Some(&*prior.cargo_facts_sha256);
    (unchanged && same_facts).then(|| {
        let mut d = prior.derived;
        for block in [&mut d.resolve, &mut d.cluster, &mut d.search_index] {
            block["reused"] = json!(true);
        }
        d
    })
}

/// Writes the seal for a finished analysis. Call it last, once every handle on
/// the graph is closed. It writes nothing when the analysis cannot vouch for
/// what it ran over: an LSP pass, a sidecar that did not land, or Cargo facts
/// that were not all written. A failed write is logged: it only costs the next
/// call its reuse.
pub(super) fn write(
    req: &AnalyzeRequest,
    index: &IndexPhase,
    derived: &Derived,
    wrote_sidecars: bool,
) {
    if req.enable_lsp || !wrote_sidecars {
        return;
    }
    let (Some(facts), Some(manifest)) = (&index.cargo_facts_digest, manifest_digest(req)) else {
        return;
    };
    let strip = |v: &Value| {
        let mut v = v.clone();
        if let Some(o) = v.as_object_mut() {
            o.remove("reused");
        }
        v
    };
    let seal = json!({
        "schema": SCHEMA,
        "server_version": env!("CARGO_PKG_VERSION"),
        "resolution_param_bits": req.gamma.to_bits(),
        "manifest_sha256": manifest,
        "cargo_facts_sha256": facts,
        "resolve": strip(&derived.resolve),
        "cluster": strip(&derived.cluster),
        "search_index": strip(&derived.search_index),
    });
    let path = seal_path(&req.graph_dir);
    if let Err(e) = std::fs::write(&path, seal.to_string()) {
        eprintln!("[ap] analysis seal {} not written: {e}", path.display());
    }
}

/// SHA-256 of `file_manifest.json`, under the bound `manifest::load` applies:
/// a manifest it would not load never seals.
fn manifest_digest(req: &AnalyzeRequest) -> Option<String> {
    read_regular_capped(&manifest_path(&req.output_dir), MAX_MANIFEST_BYTES)
        .map(|(bytes, _)| hash_bytes(&bytes))
}

fn derived_from(seal: &Value) -> Option<Derived> {
    let block = |key: &str| seal.get(key).filter(|v| v.is_object()).cloned();
    Some(Derived {
        resolve: block("resolve")?,
        cluster: block("cluster")?,
        search_index: block("search_index")?,
    })
}

#[cfg(test)]
#[path = "seal_tests.rs"]
mod tests;
