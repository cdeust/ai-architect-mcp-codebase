// lsp_client::uri — `file:` URI construction and parsing.
//
// Split from `lsp_client` when that file crossed the §4.1 500-line cap, along
// the seam the module already had: these are pure string functions with no
// process, no socket and no filesystem access, so they are unit-testable
// without spawning a language server.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// file:// URI construction with percent-encoding.
// source: RFC 3986 §2.3 — unreserved chars are ALPHA / DIGIT / `-` / `.` / `_` / `~`.
// Path separator `/` is preserved since it is reserved for path structure.
// Everything else is percent-encoded per byte (UTF-8).
// ---------------------------------------------------------------------------

/// Converts an absolute filesystem path to a `file://` URI, percent-encoding
/// all bytes that are not RFC 3986 unreserved or `/`.
pub fn path_to_file_uri(path: &Path) -> String {
    let s = path.display().to_string();
    let mut out = String::from("file://");
    for b in s.as_bytes() {
        let c = *b;
        let keep = c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~' | b'/');
        if keep {
            out.push(c as char);
        } else {
            out.push('%');
            out.push_str(&format!("{c:02X}"));
        }
    }
    out
}

/// Inverse of `path_to_file_uri`: strips the `file://` scheme and authority,
/// then percent-decodes every `%XX` escape back to its byte (UTF-8 lossy on
/// invalid sequences — a server-sent URI for a real file decodes cleanly).
/// Returns None when `uri` is not a local-file URI. A malformed escape
/// (truncated or non-hex `%XX`) is kept verbatim rather than dropped, so the
/// resulting path simply fails the caller's existence/prefix checks instead
/// of silently pointing somewhere else.
///
/// Authority handling (review finding 6). RFC 8089 §2 gives `file:` an
/// authority component, and both `file:///path` (empty) and
/// `file://localhost/path` denote a file on this machine. Stripping a fixed
/// `file://` prefix left `localhost/path` — a RELATIVE path, so
/// `uri_to_relative_path`'s `strip_prefix(root)` failed, the node-index lookup
/// missed, and the LSP pass inserted zero edges: the exact symptom
/// fleet-watch#18 was opened to remove, reachable through a spelling the LSP
/// specification permits any server to use. Any other authority names a remote
/// host, which is not a local path, and yields None.
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let encoded = strip_file_scheme_and_authority(uri)?;
    let bytes = encoded.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let is_escape = bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit();
        if is_escape {
            let hex = &encoded[i + 1..i + 3];
            // provably in-range: both chars checked is_ascii_hexdigit above
            let byte = u8::from_str_radix(hex, 16).unwrap_or(b'%');
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8_lossy(&out).into_owned()))
}

/// Returns the still-percent-encoded path of a `file:` URI that denotes a file
/// on this machine, or None for a non-`file:` scheme or a remote authority.
/// source: RFC 8089 §2 (`file:` authority), §3 (`localhost` means this machine).
fn strip_file_scheme_and_authority(uri: &str) -> Option<&str> {
    let rest = uri.strip_prefix("file://")?;
    // The first `/` after the scheme opens the path; everything before it is
    // the authority. `file:///p` therefore has an empty authority (the `/` is
    // at index 0) and `file://localhost/p` has `localhost`.
    let path_start = rest.find('/')?;
    let authority = &rest[..path_start];
    if authority.is_empty() || authority.eq_ignore_ascii_case("localhost") {
        Some(&rest[path_start..])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_to_file_uri_percent_encodes() {
        // source: M2 fix — spaces and unicode must be percent-encoded.
        let uri = path_to_file_uri(Path::new("/tmp/a b/c"));
        assert_eq!(uri, "file:///tmp/a%20b/c");
        // `/` and unreserved chars are preserved.
        let uri = path_to_file_uri(Path::new("/Users/x/foo-bar_baz.ts"));
        assert_eq!(uri, "file:///Users/x/foo-bar_baz.ts");
        // `?` and `#` must be encoded (URL-reserved).
        let uri = path_to_file_uri(Path::new("/tmp/a?b#c"));
        assert_eq!(uri, "file:///tmp/a%3Fb%23c");
    }

    #[test]
    fn test_file_uri_to_path_round_trips_and_rejects_non_file() {
        // fleet-watch#18 — the decoder must invert path_to_file_uri exactly,
        // for every byte class the encoder escapes.
        for p in [
            "/tmp/a b/c",
            "/Users/x/foo-bar_baz.ts",
            "/tmp/a?b#c",
            "/tmp/héllo/ä.rs",
        ] {
            let uri = path_to_file_uri(Path::new(p));
            assert_eq!(
                file_uri_to_path(&uri),
                Some(PathBuf::from(p)),
                "round-trip failed for {p}"
            );
        }
        // Non-file schemes are refused.
        assert_eq!(file_uri_to_path("https://example.com/x"), None);
        // A malformed escape is kept verbatim, not dropped or misparsed.
        assert_eq!(
            file_uri_to_path("file:///tmp/a%zz"),
            Some(PathBuf::from("/tmp/a%zz"))
        );
    }
}
