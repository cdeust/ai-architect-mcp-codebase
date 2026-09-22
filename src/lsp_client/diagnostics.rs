// lsp_client::diagnostics — LSP 3.17 pull diagnostics (`textDocument/diagnostic`)
// and the data the resolution pass reports from them (issue #292).
//
// Pull, not push: rust-analyzer 1.95.0 pushed `"diagnostics": []` for files
// its pull request reported as `unlinked-file` (measured 2026-09-22).
// source: ADR-9845; LSP Specification 3.17 §Pull Diagnostics.

use super::LspClient;
use serde_json::{json, Value};

/// rust-analyzer's diagnostic code for a file in no crate of its crate graph.
/// source: https://rust-analyzer.github.io/book/diagnostics.html#unlinked-file
/// (the `codeDescription.href` rust-analyzer 1.95.0 returns with the code).
pub(crate) const UNLINKED_FILE_CODE: &str = "unlinked-file";

/// One entry of a pulled diagnostic report — only the two fields read here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledDiagnostic {
    /// `Diagnostic.code`, stringified (the spec allows `integer | string`).
    pub code: Option<String>,
    pub message: String,
}

/// What `cargo metadata`'s directory-level attribution (`cargo_targets`) said
/// about a file, set beside rust-analyzer's own verdict so a disagreement is
/// reported rather than resolved (ADR-9845).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CargoAttribution {
    InsideBuildTargets,
    OutsideBuildTargets,
    /// No usable target map (no manifest, `cargo` missing, load failure).
    Unknown,
}

impl CargoAttribution {
    pub fn as_str(&self) -> &'static str {
        match self {
            CargoAttribution::InsideBuildTargets => "inside_build_targets",
            CargoAttribution::OutsideBuildTargets => "outside_build_targets",
            CargoAttribution::Unknown => "unknown",
        }
    }
}

/// A file rust-analyzer answered `unlinked-file` for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlinkedFileFinding {
    pub rel_path: String,
    /// rust-analyzer's own message for the diagnostic.
    pub message: String,
    pub cargo_attribution: CargoAttribution,
}

/// The pass's unlinked-file cross-check. `pull_supported: false` means the
/// server offered no pull diagnostics, so NOTHING was checked — distinct from
/// "checked, none unlinked".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnlinkedFileCheck {
    pub pull_supported: bool,
    pub files_checked: u64,
    pub unlinked: Vec<UnlinkedFileFinding>,
    /// Files `cargo metadata` puts outside every target that rust-analyzer
    /// did NOT flag as unlinked — the #284 attribution contradicted.
    pub linked_despite_outside_targets: Vec<String>,
}

impl LspClient {
    /// True when the server's `initialize` result advertised
    /// `diagnosticProvider`; `false` before `initialize` runs.
    pub fn supports_pull_diagnostics(&self) -> bool {
        self.pull_diagnostics
    }

    /// Sends `textDocument/diagnostic` for an already-opened document and
    /// returns its diagnostics.
    pub fn pull_diagnostics(&mut self, file_uri: &str) -> Result<Vec<PulledDiagnostic>, String> {
        let id = self.next_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/diagnostic",
            "params": { "textDocument": { "uri": file_uri } }
        });
        self.send_request(&req)?;
        let resp = self.read_response_for_id(id)?;
        parse_document_diagnostic_report(&resp)
    }
}

/// True when an `initialize` response advertises `capabilities.diagnosticProvider`
/// (LSP 3.17 §Pull Diagnostics, server capability).
pub(super) fn advertises_pull_diagnostics(init_response: &Value) -> bool {
    init_response
        .pointer("/result/capabilities/diagnosticProvider")
        .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
}

/// The message of the first `unlinked-file` diagnostic in `diagnostics`.
pub(crate) fn unlinked_file_message(diagnostics: &[PulledDiagnostic]) -> Option<&str> {
    diagnostics
        .iter()
        .find(|d| d.code.as_deref() == Some(UNLINKED_FILE_CODE))
        .map(|d| d.message.as_str())
}

/// Parses a `DocumentDiagnosticReport`. `kind: "unchanged"` carries no items;
/// this client never sends `previousResultId`, so it is read as "no items".
fn parse_document_diagnostic_report(resp: &Value) -> Result<Vec<PulledDiagnostic>, String> {
    if let Some(err) = resp.get("error") {
        return Err(format!("LSP error: {err}"));
    }
    let Some(items) = resp.pointer("/result/items").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    Ok(items.iter().map(pulled_diagnostic).collect())
}

fn pulled_diagnostic(item: &Value) -> PulledDiagnostic {
    let code = match item.get("code") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    PulledDiagnostic {
        code,
        message: item
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact `textDocument/diagnostic` answer rust-analyzer 1.95.0 gave
    /// for `kani/response_bounds.rs` (probe 2026-09-22, message trimmed).
    #[test]
    fn rust_analyzer_unlinked_file_report_is_recognised() {
        let resp = json!({"jsonrpc": "2.0", "id": 102, "result": {
        "kind": "full", "resultId": "rust-analyzer", "items": [{
            "range": {"start": {"line": 0, "character": 0},
                      "end": {"line": 0, "character": 2}},
            "severity": 4, "code": "unlinked-file", "source": "rust-analyzer",
            "message": "This file is not included in any crates, so rust-analyzer can't offer IDE services."
        }]}});
        let diags = parse_document_diagnostic_report(&resp).expect("parses");
        assert_eq!(
            unlinked_file_message(&diags),
            Some("This file is not included in any crates, so rust-analyzer can't offer IDE services.")
        );
    }

    #[test]
    fn other_codes_and_numeric_codes_are_not_unlinked() {
        let resp = json!({"jsonrpc": "2.0", "id": 1, "result": {"kind": "full", "items": [
            {"code": "E0425", "message": "no such value in this scope"},
            {"code": 42, "message": "numeric"}
        ]}});
        let diags = parse_document_diagnostic_report(&resp).expect("parses");
        assert_eq!(diags[1].code.as_deref(), Some("42"));
        assert_eq!(unlinked_file_message(&diags), None);
    }

    #[test]
    fn an_unchanged_report_has_no_items_and_an_error_is_an_error() {
        let unchanged = json!({"jsonrpc": "2.0", "id": 1,
            "result": {"kind": "unchanged", "resultId": "x"}});
        assert!(parse_document_diagnostic_report(&unchanged)
            .expect("ok")
            .is_empty());
        let error = json!({"jsonrpc": "2.0", "id": 1,
            "error": {"code": -32601, "message": "method not found"}});
        assert!(parse_document_diagnostic_report(&error).is_err());
    }

    #[test]
    fn only_an_advertised_diagnostic_provider_enables_the_pull() {
        let ra = json!({"result": {"capabilities": {"diagnosticProvider": {
            "identifier": "rust-analyzer", "interFileDependencies": true,
            "workspaceDiagnostics": false}}}});
        assert!(advertises_pull_diagnostics(&ra));
        assert!(!advertises_pull_diagnostics(
            &json!({"result": {"capabilities": {"definitionProvider": true}}})
        ));
        assert!(!advertises_pull_diagnostics(
            &json!({"result": {"capabilities": {"diagnosticProvider": null}}})
        ));
    }
}
