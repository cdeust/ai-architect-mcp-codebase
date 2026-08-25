// language_provider::file_prefix — recovering the originating file path from a
// node id or qualified name.
//
// Split from `language_provider/mod.rs` (already past the §4.1 cap) when the
// three copies of the total form were folded into one; the extension table
// and the two functions that read it are one concern.

/// All recognized source-file extensions (without the dot), across every
/// supported language. File-id extraction needs no per-node language: the
/// extension set is effectively disjoint, so the union recognizes any node's
/// originating file. source: parser::Language::from_extension (authoritative).
pub const ALL_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "java", "kt", "kts", "swift", "m", "mm", "c", "h", "cc", "cpp", "cxx",
    "hh", "hpp", "hxx", "go", "js", "jsx", "mjs", "cjs", "rb",
];

/// Extract the file-path prefix from a node id or qualified name of the form
/// `<file_path>.<ext>::<rest>`. Tries every known extension; returns the file
/// path (including extension) when one matches, else None. Replaces the
/// resolver's hardcoded four-extension scan so all languages resolve.
pub fn extract_file_prefix(id: &str) -> Option<String> {
    for ext in ALL_EXTENSIONS {
        let marker = format!(".{ext}::");
        if let Some(i) = id.find(&marker) {
            // keep up to and including the extension, drop the `::` separator.
            return Some(id[..i + marker.len() - 2].to_string());
        }
    }
    None
}

/// Total form of [`extract_file_prefix`]: the originating file path of a node
/// id or qualified name, or the whole input when no known extension marks the
/// boundary (a bare file path already IS the answer; anything else has no file
/// component to recover and keying on the full string is what the callers
/// want).
///
/// Every consumer that turns a `<file>.<ext>::<rest>` key into a file id wants
/// exactly this, and three of them — `resolver`'s qualified-name and import-id
/// helpers and `lsp_resolver::sites` — each wrote the same
/// `unwrap_or_else(|| id.to_string())` adapter. One home, so a change to the
/// fallback cannot land on one and miss the others.
pub fn extract_file_prefix_or_self(id: &str) -> String {
    extract_file_prefix(id).unwrap_or_else(|| id.to_string())
}
