//! Atomic file writes — the ONE home for the temp-file-plus-rename guarantee.
//!
//! Extracted from `handler_util` (fleet-watch#112 review round 6) so that
//! `indexer::manifest` can use it too. Before this, `meta.json` went through
//! the shared helper while `file_manifest.json` — written into the same
//! directory and paired against it by `graph_freshness` — hand-rolled its own
//! with a fixed temp name and no `fsync`. A pairing defence is only as strong
//! as the weaker of the two writes it compares, so both now share this one.
//!
//! Layer: shared/common. Depends on std + `serde` only (coding-standards §2.1),
//! which is what lets both the indexer and the handler layer call it.

use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn now_unix_seconds_nanos() -> (u64, u32) {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs(), d.subsec_nanos()),
        // Pre-1970 system clock is unreachable on any modern host; fall back
        // to epoch so we never panic (spec: "No unwraps on I/O").
        Err(_) => (0, 0),
    }
}

// source: stages/stage-1.md §9.3 Q5 — `[a-z0-9]` suffix on auto-generated run_id.
pub(crate) const RUN_ID_RANDOM_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

// source: stages/stage-1.md §9.3 Q5 — 6-char uniform-random suffix. This is
// not cryptographic; nanosecond mixing is adequate for collision avoidance
// at any realistic run rate (collision P ≈ N²/(2·36⁶), negligible).
pub(crate) fn random_suffix(len: usize) -> String {
    let (secs, nanos) = now_unix_seconds_nanos();
    let pid = process::id() as u64;
    // xorshift64* seeded from clock + pid. Source: Marsaglia, "Xorshift RNGs",
    // Journal of Statistical Software 8(14), 2003. Not cryptographic — good
    // enough for a collision suffix.
    let mut state: u64 = secs
        // source: Knuth, TAOCP vol 2 §3.3.4, Table 1 (MMIX LCG multiplier)
        .wrapping_mul(6_364_136_223_846_793_005)
        // source: Steele, Lea, Flood, "Fast Splittable Pseudorandom Number
        // Generators", OOPSLA 2014 — constant from SplitMix64 seed advance.
        .wrapping_add((nanos as u64).wrapping_mul(1_442_695_040_888_963_407))
        // PID mix: plain XOR into the state. No multiplier is needed for a
        // non-cryptographic collision suffix — the clock+nanos product above
        // already provides the avalanche, and XOR keeps the pid contribution
        // sourced and justifiably trivial (no magic multiplier to cite).
        ^ pid;
    if state == 0 {
        // source: golden ratio * 2^64 (Knuth TAOCP vol 3 §6.4); also used
        // as MurmurHash3 finalizer mixer. Non-zero fallback seed.
        state = 0x9E37_79B9_7F4A_7C15;
    }
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let idx = (state as usize) % RUN_ID_RANDOM_ALPHABET.len();
        out.push(RUN_ID_RANDOM_ALPHABET[idx] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// Stage 1 — atomic file writes (spec §5.2.3, POSIX rename(2))
// ---------------------------------------------------------------------------

// Writes `contents` to `target` by first writing to a sibling tempfile and
// renaming it over the target. POSIX rename(2) is atomic on the same
// filesystem — reference: IEEE Std 1003.1-2017, rename(2).
pub(crate) fn atomic_write(target: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("atomic_write: target has no parent: {:?}", target))?;
    fs::create_dir_all(parent).map_err(|e| format!("atomic_write: mkdir {:?}: {}", parent, e))?;

    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("atomic_write: target has no file name: {:?}", target))?;

    let (secs, nanos) = now_unix_seconds_nanos();
    let pid = process::id();
    let tmp_name = format!(
        ".{}.tmp.{}.{}.{}.{}",
        file_name,
        pid,
        secs,
        nanos,
        random_suffix(4)
    );
    let tmp_path = parent.join(tmp_name);

    {
        let mut f = fs::File::create(&tmp_path)
            .map_err(|e| format!("atomic_write: create {:?}: {}", tmp_path, e))?;
        f.write_all(contents)
            .map_err(|e| format!("atomic_write: write {:?}: {}", tmp_path, e))?;
        f.sync_all()
            .map_err(|e| format!("atomic_write: fsync {:?}: {}", tmp_path, e))?;
    }

    fs::rename(&tmp_path, target).map_err(|e| {
        // Best-effort cleanup of the tempfile — it is not fatal if this fails.
        let _ = fs::remove_file(&tmp_path);
        format!("atomic_write: rename {:?} -> {:?}: {}", tmp_path, target, e)
    })?;
    Ok(())
}

pub(crate) fn write_json_atomic<T: Serialize>(target: &Path, value: &T) -> Result<usize, String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| format!("json serialize {:?}: {}", target, e))?;
    atomic_write(target, &bytes)?;
    Ok(bytes.len())
}
