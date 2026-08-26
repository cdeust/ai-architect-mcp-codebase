//! Shared, stateless helpers used across the finding, verification, and
//! indexing/query handler modules: time/id formatting, safe-ID validation,
//! atomic file writes, and the stage error type. Pure functions, no
//! module-specific state (coding-standards.md §2.1 Shared/Common layer).
//! Extracted from `main.rs` per issue #151 (Fowler: Extract Class).

use serde_json::{Map, Value};
use std::path::{Component, Path, PathBuf};

use crate::indexer;
use crate::parser;

// ---------------------------------------------------------------------------
// Stage 1 constants — every value traces to a spec section.
// ---------------------------------------------------------------------------

// source: stages/stage-1.md §9.3 Q6 — compile-time version of the extractor.
pub(crate) const EXTRACTOR_VERSION: &str = "1.0.0";
// source: stages/stage-1.md §9.3 Q6 — compile-time version of the refinement
// schema the Rust tool accepts from the agent layer.
pub(crate) const ORCHESTRATOR_CONTRACT_VERSION: &str = "1.0.0";

// source: stages/stage-1.md §9.3 Q5 — 6 chars, follows git short-hash (7 chars)
// convention trimmed to 6 for readability, collision ~ N²/(2·36⁶).
pub(crate) const RUN_ID_RANDOM_LEN: usize = 6;

// source: stages/stage-1.md §5.1.4, §9.3 Q4 — safe-ID regex `^[A-Za-z0-9._-]+$`,
// no leading `.`, no `..`. 128 chosen as the cap: long enough for any realistic
// upstream ID (e.g. "FID-2026-04-11-behavior-change-001" ≈ 40 chars) while short
// enough to keep filesystem paths well under POSIX PATH_MAX (1024 on macOS, see
// <sys/syslimits.h>). If we ever need more we bump it with a source comment.
pub(crate) const SAFE_ID_MAX_LEN: usize = 128;

// source: stages/stage-1.md §4.4 — canonical on-disk layout.
pub(crate) const RUNS_DIR_NAME: &str = "runs";
pub(crate) const FINDINGS_DIR_NAME: &str = "findings";
pub(crate) const INDEX_FILE_NAME: &str = "index.json";
pub(crate) const EXTRACTED_FILE_NAME: &str = "stage-1.extracted.json";
pub(crate) const SOURCE_FILE_NAME: &str = "stage-1.source.json";
pub(crate) const REFINED_FILE_NAME: &str = "stage-1.refined.json";

// ---------------------------------------------------------------------------
// Stage 2 constants — every value traces to a spec section.
// ---------------------------------------------------------------------------

// source: stages/stage-2.md §11 + §12.5 item 6 — compile-time version of the
// verifier. "1.0.0" = first release shipping the four-tool set (including
// abort_verification, locked in §12.1).
pub(crate) const VERIFIER_VERSION: &str = "1.0.0";

// source: stages/stage-2.md §12.3 — single-file session replacing the §5
// two-file split.
pub(crate) const SESSION_FILE_NAME: &str = "stage-2.session.json";
// source: stages/stage-2.md §5.3 — verified receipt filename.
pub(crate) const VERIFIED_FILE_NAME: &str = "stage-2.verified.json";

// source: stages/stage-2.md §12.3 "transcript_digest change" — the sha256
// algorithm identifier is a named constant, not a scattered string literal
// (zetetic standard in the stage-2 brief).
pub(crate) const DIGEST_ALGORITHM: &str = "sha256";
// ---------------------------------------------------------------------------
// Stage 1 — time + randomness (no external crates)
// ---------------------------------------------------------------------------

// source: stages/stage-1.md §4.1 — `extracted_at` and §5.2 `started_at`.
// Reuses the format from parse_findings.py:127 (`%Y-%m-%dT%H:%M:%SZ`).
// Pure-stdlib UTC conversion via the civil-from-days algorithm by Howard
// Hinnant, "date algorithms", http://howardhinnant.github.io/date_algorithms.html
// (public-domain reference implementation; reproduced in §civil_from_days).
pub(crate) fn format_iso8601_utc(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

// source: stages/stage-1.md §9.3 Q5 — compact `YYYYMMDD-HHMMSS` for the
// run_id prefix. Matches SKILL.md:44 (`date +%Y%m%d-%H%M%S`).
pub(crate) fn format_compact_utc(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", y, mo, d, h, mi, s)
}

// source: Howard Hinnant, "chrono-Compatible Low-Level Date Algorithms"
// http://howardhinnant.github.io/date_algorithms.html — civil_from_days().
// Public-domain. Valid for the full proleptic Gregorian range we care about.
pub(crate) fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let h = rem / 3600;
    let mi = (rem % 3600) / 60;
    let s = rem % 60;

    // Shift so day 0 is 0000-03-01 (Hinnant's "era" anchor).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if mo <= 2 { y + 1 } else { y };

    (y, mo, d, h, mi, s)
}

pub(crate) fn generate_run_id() -> String {
    let (secs, _) = now_unix_seconds_nanos();
    format!(
        "{}-{}",
        format_compact_utc(secs),
        random_suffix(RUN_ID_RANDOM_LEN)
    )
}

// ---------------------------------------------------------------------------
// Stage 1 — safe-ID validation (spec §5.1.4, §9.3 Q4)
// ---------------------------------------------------------------------------

pub(crate) fn validate_safe_id(kind: &str, id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!(
            "unsafe {} (spec §5.1.4, §9.3 Q4): must be non-empty",
            kind
        ));
    }
    if id.len() > SAFE_ID_MAX_LEN {
        return Err(format!(
            "unsafe {} (spec §5.1.4, §9.3 Q4): length {} exceeds max {}",
            kind,
            id.len(),
            SAFE_ID_MAX_LEN
        ));
    }
    if id.starts_with('.') {
        return Err(format!(
            "unsafe {} (spec §5.1.4, §9.3 Q4): must not start with '.'",
            kind
        ));
    }
    if id.contains("..") {
        return Err(format!(
            "unsafe {} (spec §5.1.4, §9.3 Q4): must not contain '..'",
            kind
        ));
    }
    for b in id.bytes() {
        let ok = b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-';
        if !ok {
            return Err(format!(
                "unsafe {} (spec §5.1.4, §9.3 Q4): must match [A-Za-z0-9._-]+",
                kind
            ));
        }
    }
    Ok(())
}

pub(crate) fn require_absolute(path: &str, field: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!(
            "{} must be an absolute path (spec §3.1): got {:?}",
            field, path
        ));
    }
    // Reject `..` components outright — spec §5.1.4 safety applies to paths
    // the caller passes in. output_dir may still *resolve* to whatever the
    // user wants, but we do not silently consume `..`.
    for comp in p.components() {
        if matches!(comp, Component::ParentDir) {
            return Err(format!(
                "{} must not contain '..' (spec §5.1.4): got {:?}",
                field, path
            ));
        }
    }
    Ok(p.to_path_buf())
}

/// Parses the `language` field from tool arguments into an optional Language filter.
/// "auto" or absent -> None (detect per-file). Named language -> Some(Language).
pub(crate) fn parse_language_filter(
    args: &Map<String, Value>,
) -> Result<Option<parser::Language>, String> {
    match args.get("language").and_then(|v| v.as_str()) {
        None | Some("auto") => Ok(None),
        Some(lang_str) => parser::Language::from_str_opt(lang_str)
            .map(Some)
            .ok_or_else(|| format!("unsupported language: {lang_str}")),
    }
}

/// Resolves the tri-tier `dependency_scope` contract, honoring the deprecated
/// `include_dependencies: bool` alias (`true` -> Full, `false` -> None). If
/// both fields are present, `dependency_scope` wins. Emits a deprecation
/// warning to stderr whenever the alias is present at all.
/// source: ADR-4253701 §Decision 1.
pub(crate) fn parse_dependency_scope(
    args: &Map<String, Value>,
) -> Result<indexer::DependencyScope, String> {
    let legacy_include_deps = args.get("include_dependencies").and_then(|v| v.as_bool());
    if legacy_include_deps.is_some() {
        eprintln!(
            "deprecation warning: 'include_dependencies' is deprecated, use 'dependency_scope' \
             (\"none\" | \"public_api\" | \"full\") instead"
        );
    }
    match args.get("dependency_scope").and_then(|v| v.as_str()) {
        Some(s) => indexer::DependencyScope::from_str_opt(s)
            .ok_or_else(|| format!("unsupported dependency_scope: {s}")),
        None => Ok(match legacy_include_deps {
            Some(true) => indexer::DependencyScope::Full,
            Some(false) | None => indexer::DependencyScope::None,
        }),
    }
}

/// Parses and validates the `exclude_dirs` argument (issue #249): each entry
/// is either a bare directory name (matched anywhere in the tree, like the
/// built-in build/dependency skip list) or a path relative to `path` (matched
/// as exactly one subtree) — see `indexer::ExcludeSet::new` for how the split
/// is decided. Absent/`null` -> an empty set (no exclusions). Rejects a
/// non-array value, a non-string entry, an absolute path, or any `..`
/// component: this is the security boundary for what a caller may exclude,
/// mirroring `require_absolute`'s `..`-rejection (spec §5.1.4).
pub(crate) fn parse_exclude_dirs(args: &Map<String, Value>) -> Result<indexer::ExcludeSet, String> {
    let raw = match args.get("exclude_dirs") {
        None | Some(Value::Null) => return Ok(indexer::ExcludeSet::default()),
        Some(Value::Array(items)) => items,
        Some(other) => {
            return Err(format!(
                "field 'exclude_dirs' must be an array of strings, got {other}"
            ))
        }
    };
    let mut entries = Vec::with_capacity(raw.len());
    for item in raw {
        let s = item
            .as_str()
            .ok_or_else(|| format!("field 'exclude_dirs' entries must be strings, got {item}"))?;
        let p = Path::new(s);
        if p.is_absolute() {
            return Err(format!(
                "exclude_dirs entry must be a bare name or a path relative to 'path' \
                 (spec §5.1.4): got absolute path {s:?}"
            ));
        }
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(format!(
                "exclude_dirs entry must not contain '..' (spec §5.1.4): got {s:?}"
            ));
        }
        entries.push(s.to_string());
    }
    Ok(indexer::ExcludeSet::new(&entries))
}

/// Parses an optional boolean argument, defaulting when absent and rejecting a
/// present-but-non-boolean value (rather than silently coercing it).
pub(crate) fn parse_bool_arg(
    args: &Map<String, Value>,
    key: &str,
    default: bool,
) -> Result<bool, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(format!("field '{key}' must be a boolean, got {other}")),
    }
}

// Re-exported from `atomic_file`, which now owns these (review round 6). The
// paths stay valid so the handler modules that call them are untouched.
pub(crate) use crate::atomic_file::{
    atomic_write, now_unix_seconds_nanos, random_suffix, write_json_atomic,
};

pub(crate) type StageErr = (String, String);

pub(crate) fn bad_request<E: std::fmt::Display>(msg: E) -> StageErr {
    ("bad_request".to_string(), msg.to_string())
}
pub(crate) fn io_err<E: std::fmt::Display>(msg: E) -> StageErr {
    ("io_error".to_string(), msg.to_string())
}
pub(crate) fn unsafe_id_err(msg: String) -> StageErr {
    ("unsafe_id".to_string(), msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Negative-path coverage for parse_exclude_dirs — PR #250 review, MAJOR
    // finding: the function is the security boundary for what a caller may
    // exclude (spec §5.1.4), so every rejection branch is pinned here.

    fn args_with(v: Value) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("exclude_dirs".to_string(), v);
        m
    }

    #[test]
    fn absent_or_null_exclude_dirs_is_the_empty_set() {
        assert!(parse_exclude_dirs(&Map::new()).unwrap().is_empty());
        assert!(parse_exclude_dirs(&args_with(Value::Null))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn absolute_path_entry_is_rejected() {
        let err = parse_exclude_dirs(&args_with(json!(["/etc/secrets"]))).unwrap_err();
        assert!(err.contains("absolute"), "{err}");
    }

    #[test]
    fn parent_dir_component_is_rejected() {
        let err = parse_exclude_dirs(&args_with(json!(["config/../secrets"]))).unwrap_err();
        assert!(err.contains(".."), "{err}");
    }

    #[test]
    fn non_array_value_is_rejected() {
        let err = parse_exclude_dirs(&args_with(json!("secrets"))).unwrap_err();
        assert!(err.contains("array"), "{err}");
    }

    #[test]
    fn non_string_entry_is_rejected() {
        let err = parse_exclude_dirs(&args_with(json!([42]))).unwrap_err();
        assert!(err.contains("strings"), "{err}");
    }
}
