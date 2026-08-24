// lsp_client::commands — which language server to run, and whether it is
// there.
//
// Split from `lsp_client` when that file crossed the §4.1 500-line cap. This
// is the security-critical half of the module's input handling: `lsp_command`
// is a caller-supplied MCP tool argument that reaches `Command::new`.

use std::process::{Command, Stdio};

// ---------------------------------------------------------------------------
// LSP command auto-detection
// ---------------------------------------------------------------------------

pub fn detect_lsp_command(language: &str) -> Option<(&'static str, &'static [&'static str])> {
    match language {
        "rust" => Some(("rust-analyzer", &[])),
        "python" => Some(("pyright-langserver", &["--stdio"])),
        "typescript" => Some(("typescript-language-server", &["--stdio"])),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// LSP command allowlist — security-critical.
//
// source: C3 fix — `lsp_command` is caller-controlled (MCP tool argument)
// and is passed to `Command::new`. Without an allowlist this is pure RCE:
// `lsp_command: "rm"` or `lsp_command: "/tmp/evil"` would execute anything.
//
// Only bare binary names from a fixed list may be used. No absolute paths
// (anything containing `/`), no relative paths with `..`, no shell metachars.
// ---------------------------------------------------------------------------

/// Names approved for caller-provided `lsp_command` overrides.
/// Callers may NOT pass absolute paths; they must rely on PATH lookup.
pub const LSP_COMMAND_ALLOWLIST: &[&str] = &[
    "rust-analyzer",
    "pyright",
    "pyright-langserver",
    "typescript-language-server",
];

/// Validates that `cmd` is an allowed bare binary name. Returns Err with
/// the reason code expected by callers on rejection.
pub fn validate_lsp_command(cmd: &str) -> Result<(), String> {
    if cmd.contains('/') || cmd.contains('\\') {
        return Err(format!(
            "lsp_command_not_allowed: path separators are forbidden (got {cmd:?})"
        ));
    }
    if !LSP_COMMAND_ALLOWLIST.contains(&cmd) {
        return Err(format!(
            "lsp_command_not_allowed: {cmd:?} not in allowlist {:?}",
            LSP_COMMAND_ALLOWLIST
        ));
    }
    Ok(())
}

pub fn is_command_available(command: &str) -> bool {
    Command::new("which")
        .arg(command)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_lsp_command() {
        assert!(detect_lsp_command("rust").is_some());
        assert!(detect_lsp_command("python").is_some());
        assert!(detect_lsp_command("typescript").is_some());
        assert!(detect_lsp_command("cobol").is_none());
    }

    #[test]
    fn test_lsp_command_allowlist() {
        // source: C3 fix — only bare names from LSP_COMMAND_ALLOWLIST are OK.
        // Arbitrary binaries, absolute paths, and relative paths are rejected.

        // Rejected: bare command NOT in allowlist.
        let err = validate_lsp_command("rm").expect_err("rm must be rejected");
        assert!(err.contains("lsp_command_not_allowed"), "got: {err}");

        // Rejected: absolute path (contains '/').
        let err = validate_lsp_command("/tmp/evil").expect_err("absolute path must be rejected");
        assert!(err.contains("lsp_command_not_allowed"), "got: {err}");

        // Rejected: relative path with separator.
        let err = validate_lsp_command("./evil").expect_err("relative path must be rejected");
        assert!(err.contains("lsp_command_not_allowed"), "got: {err}");

        // Rejected: path with backslash (Windows-style).
        let err = validate_lsp_command("evil\\bin").expect_err("backslash path must be rejected");
        assert!(err.contains("lsp_command_not_allowed"), "got: {err}");

        // Accepted: allowlisted bare names.
        assert!(validate_lsp_command("rust-analyzer").is_ok());
        assert!(validate_lsp_command("pyright").is_ok());
        assert!(validate_lsp_command("pyright-langserver").is_ok());
        assert!(validate_lsp_command("typescript-language-server").is_ok());
    }
}
