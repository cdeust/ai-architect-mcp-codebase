// doc_claims::repo_files: confined, cached reads of the files a claim names.
//
// A claim names a file relative to the repository root: the document it is
// anchored in, and, for a count or variant claim, the source files the graph
// points at. Every read goes through `RepoFiles::read`, which refuses a path
// that could leave the root (absolute, `..`, or a symbolic link resolving
// outside it) before touching the file, and caps its size.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::parser::MAX_PARSE_BYTES;

/// Why a file under the root could not be read. The string is what a claim row
/// reports as its reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReadError(pub(super) String);

/// The canonical repository root and the files read so far in one call.
pub(super) struct RepoFiles {
    root: PathBuf,
    cache: BTreeMap<String, Result<String, ReadError>>,
}

impl RepoFiles {
    /// precondition: `root` exists and is a directory.
    pub(super) fn new(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|e| format!("repo_root cannot be resolved: {}: {e}", root.display()))?;
        if !root.is_dir() {
            return Err(format!("repo_root is not a directory: {}", root.display()));
        }
        Ok(Self {
            root,
            cache: BTreeMap::new(),
        })
    }

    /// The text of `rel`, read at most once per call.
    ///
    /// The size cap is the parser's (`MAX_PARSE_BYTES`, 1 MiB, issue #148): the
    /// same bound every other untrusted file read in this server applies.
    pub(super) fn read(&mut self, rel: &str) -> Result<&str, ReadError> {
        if !self.cache.contains_key(rel) {
            let loaded = self.load(rel);
            self.cache.insert(rel.to_string(), loaded);
        }
        match &self.cache[rel] {
            Ok(text) => Ok(text.as_str()),
            Err(e) => Err(e.clone()),
        }
    }

    /// Whether `rel` names a regular file inside the root.
    pub(super) fn exists(&self, rel: &str) -> Result<bool, ReadError> {
        match self.confine(rel) {
            Ok(path) => Ok(path.is_file()),
            Err(e) if e.0 == "file_not_found" => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn load(&self, rel: &str) -> Result<String, ReadError> {
        let path = self.confine(rel)?;
        let size = fs::metadata(&path)
            .map_err(|_| ReadError("file_not_found".into()))?
            .len();
        if size > MAX_PARSE_BYTES {
            return Err(ReadError(format!(
                "file_too_large: {size} bytes > {MAX_PARSE_BYTES}"
            )));
        }
        let bytes = fs::read(&path).map_err(|_| ReadError("file_unreadable".into()))?;
        String::from_utf8(bytes).map_err(|_| ReadError("file_not_utf8".into()))
    }

    /// The canonical path of `rel`, refused unless it stays inside the root.
    fn confine(&self, rel: &str) -> Result<PathBuf, ReadError> {
        if rel.is_empty() || rel.contains('\0') {
            return Err(ReadError("path_empty_or_invalid".into()));
        }
        let path = Path::new(rel);
        if path.is_absolute() {
            return Err(ReadError("path_absolute".into()));
        }
        if path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err(ReadError("path_leaves_repo_root".into()));
        }
        let canonical = self
            .root
            .join(path)
            .canonicalize()
            .map_err(|_| ReadError("file_not_found".into()))?;
        if !canonical.starts_with(&self.root) {
            return Err(ReadError("path_leaves_repo_root".into()));
        }
        Ok(canonical)
    }
}

/// Whether `text` appears verbatim at 1-based `line` of `content`. A text of
/// several lines must start on `line` and may span the lines that follow.
pub(super) fn anchor_holds(content: &str, line: u64, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("text_empty".into());
    }
    let lines: Vec<&str> = content.lines().collect();
    let Some(start) = (line as usize).checked_sub(1).filter(|i| *i < lines.len()) else {
        return Err(format!(
            "line_out_of_range: the file has {} lines",
            lines.len()
        ));
    };
    let span = text.split('\n').count();
    let end = (start + span).min(lines.len());
    let window = lines[start..end].join("\n");
    if window.contains(text) {
        Ok(())
    } else {
        Err("text_not_at_line".into())
    }
}

#[cfg(test)]
#[path = "repo_files_tests.rs"]
mod tests;
