// git_diff::parse — unified diff text to per-file hunks.
//
// Each hunk body is read for exactly the line counts its `@@ -a,b +c,d @@`
// header announces (issue #446 review): a `+++i;` or `--- note` body line is a
// change, never a file header, and whatever follows the last hunk (the `-- `
// trailer of `git format-patch`, the next file of a plain `diff -u` without
// `diff --git` lines) is not read as hunk body.
//
// source: GNU diffutils manual, "Detailed Description of Unified Format" — a
// missing count means 1, and an empty hunk "is considered to end at the line
// that precedes the hunk".

/// Safety limit: `current_line` is u64, but a crafted diff with billions of
/// `+` lines, or a header near `u64::MAX`, could wrap around. Capped at
/// u64::MAX / 2 so there is always headroom.
/// source: M3 fix (previously in `git_diff`).
pub(super) const DIFF_LINE_MAX: u64 = u64::MAX / 2;

const TOO_LARGE: &str = "diff_too_large: line counter would overflow";

// pub(crate): reused by the history module, which parses per-commit
// `git show` diffs into the same hunk shape to attribute changes to entities.
pub(crate) struct FileHunk {
    pub(crate) file_path: String,
    /// New-file positions of every `+` line and of every `-` line (a `-` line
    /// takes the position of the new-file line that follows it, except in a
    /// `+N,0` hunk, where it takes N, the line before: `removal_gaps` corrects
    /// that, this list keeps it). Read by the history module.
    pub(crate) changed_lines: Vec<u64>,
    /// New-file positions of every `+` line.
    pub(crate) added_lines: Vec<u64>,
    /// New-file positions just after a run of `-` lines that no `+` line
    /// replaces (a pure removal between lines p-1 and p).
    pub(crate) removal_gaps: Vec<u64>,
    /// The `added_lines` and `removal_gaps` that change more than blank lines.
    /// A blank-only change still counts inside a symbol (a blank line in a
    /// multiline string changes its value); outside every symbol it is no
    /// coverage gap. Kept as the non-blank set, so a blank line at N never
    /// hides a non-blank one at N from another hunk of the same path.
    pub(crate) nonblank_lines: Vec<u64>,
    pub(crate) nonblank_gaps: Vec<u64>,
    /// The section could not be read in full: an unreadable hunk header, a
    /// hunk cut short or overrun, body lines before any `@@`, file headers
    /// with no hunk, an empty or out-of-place file header, several commits, or
    /// no file path at all. Some changes may be missing.
    pub(crate) malformed: bool,
    /// A `Binary files … differ` or `GIT binary patch` section.
    pub(crate) binary: bool,
    /// A rename or copy with no content hunk (`similarity index 100%`): the
    /// file's symbols moved path.
    pub(crate) renamed: bool,
    pub(crate) is_new: bool,
    pub(crate) is_deleted: bool,
}

/// A `-` run not yet followed by a `+` line: its position, and whether every
/// removed line in it is blank.
#[derive(Clone, Copy)]
struct PendingRemoval {
    at: u64,
    all_blank: bool,
}

/// The state of one file section while its hunks are read.
#[derive(Default)]
struct FileSection {
    path: Option<String>,
    is_new: bool,
    is_deleted: bool,
    changed: Vec<u64>,
    added: Vec<u64>,
    gaps: Vec<u64>,
    nonblank_lines: Vec<u64>,
    nonblank_gaps: Vec<u64>,
    /// The `b/` path of the `diff --git a/X b/Y` line, used when no `---` or
    /// `+++` header names the file.
    git_path: Option<String>,
    binary: bool,
    pending: Option<PendingRemoval>,
    /// 1 inside a hunk whose new range is empty (`+N,0`): its start N names
    /// the line BEFORE the removal, so the line after it is N + 1.
    gap_shift: u64,
    /// Next new-file position.
    line: u64,
    /// Body lines the current hunk still owes, old side and new side.
    old_left: u64,
    new_left: u64,
    had_hunk: bool,
    /// A `+++` header was read: the next `---` starts another file.
    seen_new_header: bool,
    /// A `+` or `-` body line was read. A real unified hunk always has one;
    /// hunks of context only are `--word-diff` (or similar) output.
    saw_change: bool,
    /// `rename from` / `copy from`: the file moved, with or without a hunk.
    renamed: bool,
    /// The last body line was `--- a/…`: if `+++ b/…` follows, the hunk
    /// swallowed the next file's headers (a header count one line too long).
    after_old_header_line: bool,
    malformed: bool,
    /// Past a `git format-patch` signature delimiter (`-- `): no more body.
    in_trailer: bool,
}

impl FileSection {
    fn in_hunk(&self) -> bool {
        self.old_left > 0 || self.new_left > 0
    }

    fn close_removal(&mut self) {
        if let Some(p) = self.pending.take() {
            let gap = p.at.saturating_add(self.gap_shift);
            self.gaps.push(gap);
            if !p.all_blank {
                self.nonblank_gaps.push(gap);
            }
        }
    }

    /// Ends the current hunk; one that still owes lines (a truncated
    /// `diff_text`) is unreadable: its missing lines are unknown.
    fn end_hunk(&mut self) {
        if self.in_hunk() {
            self.malformed = true;
        }
        self.old_left = 0;
        self.new_left = 0;
    }

    fn start_hunk(&mut self, header: &str) -> Result<(), String> {
        self.close_removal();
        self.end_hunk();
        self.in_trailer = false;
        let Some(h) = HunkHeader::parse(header) else {
            self.malformed = true;
            return Ok(());
        };
        if h.new_start >= DIFF_LINE_MAX {
            return Err(TOO_LARGE.to_string());
        }
        self.gap_shift = u64::from(h.new_count == 0);
        self.line = h.new_start;
        self.old_left = h.old_count;
        self.new_left = h.new_count;
        self.had_hunk = true;
        Ok(())
    }

    /// One body line of a hunk: `+` added, `-` removed, `\` metadata,
    /// anything else context.
    fn body_line(&mut self, line: &str) -> Result<(), String> {
        if line.starts_with('\\') {
            // "\ No newline at end of file": no line in either file.
            return Ok(());
        }
        let follows_old_header = std::mem::take(&mut self.after_old_header_line);
        if let Some(body) = line.strip_prefix('-') {
            self.after_old_header_line = line.starts_with("--- a/");
            self.take_count(true, false);
            self.saw_change = true;
            self.changed.push(self.line);
            let blank = is_blank(body);
            let pending = self.pending.get_or_insert(PendingRemoval {
                at: self.line,
                all_blank: true,
            });
            pending.all_blank &= blank;
            return Ok(());
        }
        if self.line >= DIFF_LINE_MAX {
            return Err(TOO_LARGE.to_string());
        }
        if let Some(body) = line.strip_prefix('+') {
            if follows_old_header && line.starts_with("+++ b/") {
                self.malformed = true;
            }
            self.take_count(false, true);
            self.saw_change = true;
            let replaced_code = self.pending.take().is_some_and(|p| !p.all_blank);
            self.changed.push(self.line);
            self.added.push(self.line);
            if !is_blank(body) || replaced_code {
                self.nonblank_lines.push(self.line);
            }
        } else {
            self.take_count(true, true);
            self.close_removal();
        }
        self.line += 1;
        Ok(())
    }

    /// Uses up one line of the old and/or new side; a side whose count is
    /// already 0 means the header understated it (a hunk that swallows the
    /// next file's headers in a plain `diff -u`).
    fn take_count(&mut self, old: bool, new: bool) {
        if (old && self.old_left == 0) || (new && self.new_left == 0) {
            self.malformed = true;
        }
        if old {
            self.old_left = self.old_left.saturating_sub(1);
        }
        if new {
            self.new_left = self.new_left.saturating_sub(1);
        }
    }

    /// A line outside every hunk that is not a header. After a hunk, a body
    /// line (or a combined-diff `@@@` header) means the counts were wrong or
    /// the header unreadable; the `-- ` signature of `git format-patch` and
    /// what follows it are the only exception.
    fn stray_line(&mut self, line: &str) {
        // Several commits (`git log -p`, a patch series) hold line numbers of
        // different revisions: they cannot all be matched against one index.
        // Checked before the trailer test: each patch of a default
        // `git format-patch --stdout` series ends with a `-- ` signature, and
        // the next patch's `From ` line comes inside that trailer. The `From `
        // line before the first patch comes before any hunk and is exempt.
        if self.had_hunk && (line.starts_with("commit ") || line.starts_with("From ")) {
            self.malformed = true;
            self.in_trailer = true;
            return;
        }
        if self.in_trailer {
            return;
        }
        if line.starts_with("@@") {
            // `@@@ … @@@` (a combined diff from `git show <merge>`).
            self.malformed = true;
            return;
        }
        if line.starts_with("rename from ") || line.starts_with("copy from ") {
            self.renamed = true;
            return;
        }
        if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            self.binary = true;
            return;
        }
        if !self.had_hunk {
            // A `+`/`-`/context line after a file header but before any `@@`
            // (a pasted diff whose hunk header was stripped). Diffstat and
            // commit-message lines come before any file header.
            let named = self.path.is_some() || self.git_path.is_some();
            if named && line.starts_with(['+', '-', ' ']) {
                self.malformed = true;
            }
            return;
        }
        if line == "-- " {
            // The format-patch signature: what follows is no diff.
            self.in_trailer = true;
        } else if line.starts_with(['+', '-', ' ']) {
            self.malformed = true;
        }
    }

    fn into_hunk(mut self) -> Option<FileHunk> {
        self.close_removal();
        self.end_hunk();
        // A section whose hunks name no usable path is reported, not dropped.
        // A section with no hunk changes no line (a mode change, an empty
        // new file): it neither adds a file nor changes another section's
        // type, unless it is binary or could not be read.
        // `git diff` prints `---`/`+++` only before a hunk: headers with none
        // mean the `@@` lines were cut off.
        if !self.had_hunk && (self.seen_new_header || self.path.is_some()) {
            self.malformed = true;
        }
        if self.had_hunk && !self.saw_change {
            self.malformed = true;
        }
        let renamed = self.renamed && !self.had_hunk;
        if !self.had_hunk && !self.malformed && !self.binary && !renamed {
            return None;
        }
        let file_path = match self.path.take().or(self.git_path.take()) {
            Some(p) => p,
            None => {
                self.malformed = true;
                "(diff section with no file path)".to_string()
            }
        };
        Some(FileHunk {
            file_path,
            changed_lines: self.changed,
            added_lines: self.added,
            removal_gaps: self.gaps,
            nonblank_lines: self.nonblank_lines,
            nonblank_gaps: self.nonblank_gaps,
            malformed: self.malformed,
            binary: self.binary,
            renamed,
            is_new: self.is_new,
            is_deleted: self.is_deleted,
        })
    }
}

/// Only ASCII spaces and tabs: a line of U+00A0 is code that can break the
/// build (issue #446 review).
fn is_blank(body: &str) -> bool {
    body.bytes().all(|b| b == b' ' || b == b'\t')
}

/// The four numbers of `@@ -old_start[,old_count] +new_start[,new_count] @@`.
struct HunkHeader {
    old_count: u64,
    new_start: u64,
    new_count: u64,
}

impl HunkHeader {
    fn parse(line: &str) -> Option<Self> {
        let mut ranges = line.strip_prefix("@@ ")?.split(' ');
        let old = ranges.next()?.strip_prefix('-')?;
        let new = ranges.next()?.strip_prefix('+')?;
        let (_, old_count) = range(old)?;
        let (new_start, new_count) = range(new)?;
        Some(HunkHeader {
            old_count,
            new_start,
            new_count,
        })
    }
}

/// `start[,count]`, with a missing count meaning 1.
fn range(text: &str) -> Option<(u64, u64)> {
    match text.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

/// The new-file start of a hunk header, or `None` when it is malformed.
#[cfg(test)]
pub(super) fn parse_hunk_header(line: &str) -> Option<u64> {
    HunkHeader::parse(line).map(|h| h.new_start)
}

/// What a `---`/`+++` header names.
enum HeaderPath {
    /// `/dev/null`: no file on that side.
    DevNull,
    /// Nothing usable: the section cannot be attributed.
    Empty,
    Path(String),
}

/// The path of a `---`/`+++` header: cut at the tab that `diff -u` puts
/// before its timestamp, `prefix` (`a/` or `b/`) removed when present.
fn header_path(text: &str, prefix: &str) -> HeaderPath {
    let path = text.split('\t').next().unwrap_or(text).trim_end();
    match path {
        "/dev/null" => HeaderPath::DevNull,
        "" => HeaderPath::Empty,
        _ => HeaderPath::Path(path.strip_prefix(prefix).unwrap_or(path).to_string()),
    }
}

/// The `b/` path of `diff --git a/X b/Y`. Git does not quote spaces, so a
/// path holding ` b/` is ambiguous; when both halves are the same path (no
/// rename), the midpoint split is exact.
fn git_header_path(line: &str) -> Option<String> {
    let rest = line.strip_prefix("diff --git a/")?;
    if rest.len() >= 3 && rest.len() % 2 == 1 {
        let half = (rest.len() - 3) / 2;
        if rest.get(half..half + 3) == Some(" b/") && rest.get(..half) == rest.get(half + 3..) {
            return Some(rest[half + 3..].to_string());
        }
    }
    let (_, new) = rest.rsplit_once(" b/")?;
    Some(new.to_string())
}

/// Whether `diff_text` holds any line that starts a unified-diff section or
/// hunk (`diff --git `, `--- `, `+++ `, `@@`). A diff whose sections all
/// change no line (a mode change, an empty new file) parses to no hunk but
/// is still readable; a colored or reformatted paste has no such line.
pub(crate) fn has_diff_structure(diff_text: &str) -> bool {
    diff_text.lines().any(|l| {
        l.starts_with("diff --git ")
            || l.starts_with("--- ")
            || l.starts_with("+++ ")
            || l.starts_with("@@")
    })
}

/// The `Only in <dir>: <name>` lines of `diff -r` without `-N`: each is a
/// file (or directory) present on one side only, printed with no hunk and no
/// file header, whose changes the diff does not show. No hunk body line can
/// start with `O`, so the count is exact.
pub(crate) fn one_sided_entries(diff_text: &str) -> usize {
    diff_text
        .lines()
        .filter(|l| l.starts_with("Only in "))
        .count()
}

pub(crate) fn parse_unified_diff(diff_text: &str) -> Result<Vec<FileHunk>, String> {
    let mut hunks: Vec<FileHunk> = Vec::new();
    let mut section = FileSection::default();
    for line in diff_text.lines() {
        // `diff --git` and `@@` are never body lines (body lines start with
        // ` `, `+`, `-` or `\`), so they end a hunk whose header overstated
        // its counts, as a hand-trimmed `diff_text` can.
        let header = line.starts_with("diff --git") || line.starts_with("@@ ");
        if section.in_hunk() && !header {
            section.body_line(line)?;
        } else if line.starts_with("diff --git") {
            hunks.extend(std::mem::take(&mut section).into_hunk());
            section.git_path = git_header_path(line);
        } else if line.starts_with("@@ ") {
            section.start_hunk(line)?;
        } else if (line.starts_with("--- ") || line.starts_with("+++ "))
            && section.had_hunk
            && !section.seen_new_header
        {
            // A file header after a hunk with no `+++` before it would rename
            // the section and credit its change to another file.
            section.malformed = true;
        } else if let Some(old) = line.strip_prefix("--- ") {
            // A second file header with no `diff --git` line between them
            // (plain `diff -u`) starts a new section, even after a hunk that
            // could not be read.
            if section.seen_new_header {
                hunks.extend(std::mem::take(&mut section).into_hunk());
            }
            match header_path(old, "a/") {
                // For a deleted file, this is the only place the path appears.
                HeaderPath::Path(path) => {
                    section.path.get_or_insert(path);
                }
                HeaderPath::DevNull => section.is_new = true,
                HeaderPath::Empty => section.malformed = true,
            }
        } else if let Some(new) = line.strip_prefix("+++ ") {
            // A second `+++` with no `---` before it would rename the section
            // and credit its change to another file.
            if section.seen_new_header {
                section.malformed = true;
                continue;
            }
            section.seen_new_header = true;
            match header_path(new, "b/") {
                HeaderPath::Path(path) => section.path = Some(path),
                HeaderPath::DevNull => section.is_deleted = true,
                HeaderPath::Empty => section.malformed = true,
            }
        } else {
            section.stray_line(line);
        }
    }
    hunks.extend(section.into_hunk());
    Ok(hunks)
}
