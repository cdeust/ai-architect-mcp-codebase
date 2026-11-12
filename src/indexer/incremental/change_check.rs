//! The total-size refusal the incremental pass and the full manifest share
//! (issue #443). Split out of `mod.rs` for the §4.1 file cap (review of
//! #478).

use super::Discovered;

/// `Err(total_bytes_exceeded…)` when the discovered files sum past `cap` —
/// the same refusal, and wording, as the full index's walk.
pub(super) fn check_total_bytes(current: &[Discovered], cap: u64) -> Result<(), String> {
    let total = current
        .iter()
        .fold(0u64, |acc, d| acc.saturating_add(d.size));
    if total > cap {
        return Err(format!(
            "{}: {total} bytes discovered (MAX_TOTAL_BYTES {cap})",
            super::super::TOTAL_BYTES_EXCEEDED
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file(size: u64) -> Discovered {
        Discovered {
            rel: format!("f{size}"),
            abs: PathBuf::from(format!("/nonexistent/f{size}")),
            mtime_ns: 0,
            size,
        }
    }

    #[test]
    fn a_tree_over_the_cap_is_refused_before_any_file_is_read() {
        // The paths do not exist: a refusal that tried to read them would
        // surface an I/O error instead of the cap.
        let tree = [file(6), file(5)];
        let err = check_total_bytes(&tree, 10).expect_err("11 bytes > 10");
        assert!(err.starts_with("total_bytes_exceeded"), "{err}");
        assert!(
            check_total_bytes(&tree, 11).is_ok(),
            "the cap itself is allowed"
        );
    }
}
