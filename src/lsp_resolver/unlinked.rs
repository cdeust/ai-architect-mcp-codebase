// lsp_resolver::unlinked — the per-file `unlinked-file` cross-check (issue #292).
//
// rust-analyzer's own verdict is set beside the `cargo metadata` attribution;
// a disagreement is recorded, never resolved one way. source: ADR-9845.

use crate::indexer::cargo_targets::TargetMap;
use crate::lsp_client::{
    self, CargoAttribution, LspClient, UnlinkedFileCheck, UnlinkedFileFinding,
};
use std::path::Path;

/// What the cargo target map says about `rel`.
pub(super) fn cargo_attribution(map: &TargetMap, rel: &str) -> CargoAttribution {
    if matches!(map, TargetMap::Unknown) {
        CargoAttribution::Unknown
    } else if map.is_outside_targets(Path::new(rel)) {
        CargoAttribution::OutsideBuildTargets
    } else {
        CargoAttribution::InsideBuildTargets
    }
}

/// Pulls the diagnostics of the already-opened `file_uri` and folds the
/// verdict into `check`. A failed pull checks nothing and fails nothing: an
/// unlinked file describes the build configuration, not an LSP error.
pub(super) fn check_file(
    client: &mut LspClient,
    check: &mut UnlinkedFileCheck,
    file_uri: &str,
    rel: &str,
    attribution: CargoAttribution,
) {
    let Ok(diagnostics) = client.pull_diagnostics(file_uri) else {
        return;
    };
    check.files_checked += 1;
    match lsp_client::unlinked_file_message(&diagnostics) {
        Some(message) => check.unlinked.push(UnlinkedFileFinding {
            rel_path: rel.to_string(),
            message: message.to_string(),
            cargo_attribution: attribution,
        }),
        None if attribution == CargoAttribution::OutsideBuildTargets => {
            check.linked_despite_outside_targets.push(rel.to_string());
        }
        None => {}
    }
}
