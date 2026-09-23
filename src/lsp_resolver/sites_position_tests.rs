//! Where `lsp_position` aims `textDocument/definition`: the call's own
//! identifier, never its receiver, and on the identifier's own line when
//! the callee expression spans several (issue #317). Split from `sites.rs`
//! to keep that file under the §4.1 500-line cap.

use super::UnresolvedCallSite;

/// Root cause 2 (fix/lsp-receiver-calls). `CallSite.col` (post the
/// `lsp_col` fix) still points at the START of the callee expression —
/// the RECEIVER for a method call — not the method identifier.
/// rust-analyzer resolves that position to the receiver's own binding,
/// not the method: verified 2026-09-03 on `self.response_of(i)`, column
/// 16 (`self`) -> the `self` binding, column 21 (`response_of`) -> the
/// method, same line. `lsp_position` must therefore target the LAST
/// `.`/`::`-separated segment, never the stored column verbatim.
#[test]
fn lsp_definition_targets_method_identifier_not_receiver() {
    let cases: &[(&str, u64, u64, u64)] = &[
        // (callee_name, line, col, expected identifier col)
        ("self.response_of", 5, 8, 8 + "self.".len() as u64),
        ("s.response_of", 5, 8, 8 + "s.".len() as u64),
        ("trial.response_of", 5, 8, 8 + "trial.".len() as u64),
        ("helpers::normalize", 5, 8, 8 + "helpers::".len() as u64),
        // A chained call: the LAST segment is the identifier, not the
        // first receiver nor an intermediate call's parens.
        (
            "input.trim().to_string",
            5,
            8,
            8 + "input.trim().".len() as u64,
        ),
        // A bare function call has no separator: identifier == col.
        ("helper", 5, 8, 8),
    ];
    for (callee_name, line, col, expected_col) in cases {
        let site = UnresolvedCallSite {
            id: "src/a.rs::caller::call@x".to_string(),
            caller_qn: "src/a.rs::caller".to_string(),
            caller_label: "Method".to_string(),
            callee_name: callee_name.to_string(),
            file_path: "src/a.rs".to_string(),
            line: *line,
            col: *col,
        };
        let (lsp_line, lsp_col) = site.lsp_position();
        assert_eq!(
            lsp_line,
            line - 1,
            "line must be converted from the graph's 1-based to LSP's \
             0-based: {callee_name}"
        );
        assert_eq!(
            lsp_col, *expected_col,
            "identifier column for callee_name={callee_name:?}"
        );
    }
}

/// Issue #317. A builder chain split across lines stores its callee_name
/// verbatim, newlines included, anchored at the chain's FIRST line:
/// `Task::new(wcet, period)\n                .deadline` at (48, 12) in
/// dy-wcet's `tests/properties.rs::generate`. The method sits on a later
/// line, so adding the byte offset to the start column aimed at column 53
/// of line 48, past its end. Measured on that corpus: every single-line
/// `Task::new(..).deadline` site resolved through LSP, every multi-line
/// one stayed unresolved.
#[test]
fn lsp_definition_targets_the_method_line_of_a_multi_line_chain() {
    let cases: &[(&str, u64, u64, u64, u64)] = &[
        // (callee_name, line, col, expected 0-based line, expected col)
        (
            "Task::new(wcet, period)\n                .deadline",
            48,
            12,
            48,
            17,
        ),
        (
            "Task::new(wcet, period)\n    .deadline(deadline)\n    .jitter",
            48,
            12,
            49,
            5,
        ),
        // A macro-reconstructed receiver, measured in dy-wcet's
        // `src/lib.rs` at 908:35.
        (
            "(Unbounded::NonConvergent)\n            .to_string",
            908,
            35,
            908,
            13,
        ),
    ];
    for (callee_name, line, col, expected_line, expected_col) in cases {
        let site = UnresolvedCallSite {
            id: "src/a.rs::caller::call@x".to_string(),
            caller_qn: "src/a.rs::caller".to_string(),
            caller_label: "Function".to_string(),
            callee_name: callee_name.to_string(),
            file_path: "src/a.rs".to_string(),
            line: *line,
            col: *col,
        };
        assert_eq!(
            site.lsp_position(),
            (*expected_line, *expected_col),
            "position for callee_name={callee_name:?}"
        );
    }
}
