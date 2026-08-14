// graph_store::config — lbug max_db_size resolution (env + defaults).
//
// Extracted from graph_store.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared store
// vocabulary exactly as when this lived in one module.

use super::*;

/// Environment variable that bounds lbug's `max_db_size` (bytes; must be a
/// power of two — see `BufferManager::verifySizeParams` in the vendored
/// `lbug-0.15.4/lbug-src/src/storage/buffer_manager/buffer_manager.cpp`).
/// Unset in production; set once in `.cargo/config.toml`'s `[env]` table so
/// every `cargo test` process (unit tests AND every integration-test binary
/// under `tests/`) picks it up automatically without any test file needing
/// to opt in. Takes priority over `PROD_MAX_DB_SIZE_ENV` (see
/// `system_config`'s doc comment for the precedence rule) so this test-only
/// knob keeps controlling `cargo test` behavior unchanged by issue #25.
///
/// Root cause this bounds: `SystemConfig::default()` leaves `max_db_size`
/// at its sentinel value `u64::from(u32::MAX)` (lbug-0.15.4/src/database.rs:71).
/// lbug's C++ core treats that exact sentinel as "unset" and substitutes
/// `DEFAULT_VM_REGION_MAX_SIZE = 1 << 43` (8 TiB) — see
/// `lbug-0.15.4/lbug-src/src/main/database.cpp:82-83` and the doc comment
/// on `maxDBSize` in `lbug-0.15.4/lbug-src/src/include/main/database.h:50-54`.
/// Each `Database::new`/`GraphStore::open_or_create` call therefore reserves
/// 8 TiB of virtual address space; dozens of concurrent test binaries under
/// `cargo test --workspace` exhaust the process's address space
/// probabilistically (observed: `lbug database open failed: Mmap for size
/// 8796093022208 failed` in `tests/stage6_integration.rs`, run 5 of a 10-run
/// soak, 2026-07-15).
pub const TEST_MAX_DB_SIZE_ENV: &str = "AP_LBUG_TEST_MAX_DB_SIZE";

/// Environment variable that bounds lbug's `max_db_size` for production
/// (non-`cargo test`) processes — bytes, must be a power of two, same
/// `verifySizeParams` constraint as `TEST_MAX_DB_SIZE_ENV`. Unset by default,
/// in which case `DEFAULT_PROD_MAX_DB_SIZE_BYTES` applies (issue #25: the
/// prior behavior left lbug's own 8 TiB sentinel default in place for every
/// production `GraphStore::open_or_create`/`GraphCache` open — up to
/// `graph_cache::MAX_CACHED_GRAPHS` (8) concurrently cached stores × 8 TiB =
/// 64 TiB of virtual address space reserved in the worst case).
pub const PROD_MAX_DB_SIZE_ENV: &str = "AP_LBUG_MAX_DB_SIZE";

/// Minimum accepted `max_db_size` for either env var, mirroring lbug's own
/// floor: `BufferManager::verifySizeParams` rejects
/// `maxDBSize < 2 * LBUG_PAGE_SIZE * PAGE_GROUP_SIZE`
/// (`lbug-0.15.4/lbug-src/src/storage/buffer_manager/buffer_manager.cpp:99-112`)
/// where `LBUG_PAGE_SIZE = 4096` (`PAGE_SIZE_LOG2 = 12`, generated
/// `system_config.h`) and `PAGE_GROUP_SIZE = 1024` (`PAGE_GROUP_SIZE_LOG2 = 10`,
/// `lbug-src/src/include/common/constants.h:74-75`) — `2 * 4096 * 1024` =
/// 8 MiB. Rejecting below this in Rust surfaces an actionable AP-level error
/// message instead of the opaque C++ `BufferManagerException` lbug would
/// otherwise throw from inside `Database::new`.
pub(crate) const MIN_MAX_DB_SIZE_BYTES: u64 = 8 * 1024 * 1024; // 8 MiB — see doc comment above.

/// Production default for `max_db_size` when `PROD_MAX_DB_SIZE_ENV` is unset.
///
/// This is lbug's own architectural ceiling for one database's VM region —
/// `DEFAULT_VM_REGION_MAX_SIZE = 1 << 43` (8 TiB) on every non-32-bit,
/// non-Android platform, i.e. every target this server ships for. source:
/// `lbug-0.19.1/lbug-src/src/include/common/constants.h:57-64` (the
/// `#ifdef __32BIT__` / `#elif defined(__ANDROID__)` / `#else` ladder) and
/// `verifySizeParams` (`buffer_manager.cpp:99-112`), which enforces only the
/// 8 MiB floor and power-of-two — no upper bound. Setting it explicitly to
/// this value is byte-identical to what lbug substitutes on its own when
/// `max_db_size` is left at the "unset" sentinel.
///
/// History: issue #25 capped this at 8 GiB (sized from 75 measured graphs)
/// to bound virtual-address-space reservations. That cap turned out to
/// ABORT any ingestion whose graph outgrew 8 GiB — a hard product bug: an
/// index must complete regardless of the codebase's size (bug report
/// 2026-08-14; multi-TiB corpora are a supported workload). The cap is
/// therefore repealed for production: `max_db_size` is an mmap RESERVATION
/// of address space, not an allocation — disk and memory grow only with
/// real data, and the pre-#25 production behavior (this exact value via
/// lbug's sentinel, single-process server, up to 8 cached stores = 64 TiB
/// of reservations) shipped for months without a recorded failure. The
/// failure #25 actually observed was under DOZENS of concurrent `cargo
/// test` processes, and the fix that holds it is the test-only bound
/// (`TEST_MAX_DB_SIZE_ENV`, set in `.cargo/config.toml`), which this repeal
/// does not touch. Operators who need a bound set `PROD_MAX_DB_SIZE_ENV`.
pub const DEFAULT_PROD_MAX_DB_SIZE_BYTES: u64 = 1 << 43; // 8 TiB — see doc comment above.

/// Parses and validates a `max_db_size` env var value: must be a valid
/// non-negative integer, at least `MIN_MAX_DB_SIZE_BYTES`, and a power of
/// two. `env_name` is the variable name, used only to make the error message
/// actionable (which var to fix, not a generic parse failure).
///
/// precondition: `raw` is the exact string read from `std::env::var`.
/// postcondition: `Ok(bytes)` only for a value lbug's own
/// `BufferManager::verifySizeParams` will accept; `Err` names the specific
/// violation (not-a-number / too small / not-power-of-two) so a misconfigured
/// deployment fails loudly at startup instead of silently falling back to a
/// default the operator did not choose.
pub(crate) fn parse_and_validate_max_db_size(raw: &str, env_name: &str) -> Result<u64, String> {
    let bytes: u64 = raw
        .parse()
        .map_err(|e| format!("{env_name}={raw:?} is not a valid non-negative byte count: {e}"))?;
    if bytes < MIN_MAX_DB_SIZE_BYTES {
        return Err(format!(
            "{env_name}={bytes} is below lbug's minimum max_db_size of \
             {MIN_MAX_DB_SIZE_BYTES} bytes (8 MiB; BufferManager::verifySizeParams)"
        ));
    }
    if bytes & (bytes - 1) != 0 {
        return Err(format!(
            "{env_name}={bytes} is not a power of two, which lbug's \
             BufferManager::verifySizeParams requires"
        ));
    }
    Ok(bytes)
}

/// Builds the `SystemConfig` used by every lbug `Database` this crate opens.
/// The single by-construction choke point for `max_db_size`: both
/// `GraphStore::open_or_create` (used by all production code and by every
/// integration test that goes through `GraphStore`) and the one test file
/// that opens a raw `lbug::Database` directly
/// (`tests/lbug_bulk_investigation.rs`) call this function, so no future
/// call site can reintroduce the unbounded default.
///
/// Precedence: `TEST_MAX_DB_SIZE_ENV` (`AP_LBUG_TEST_MAX_DB_SIZE`), when set,
/// always wins — this preserves the issue #21/#24 test-bounding behavior
/// unchanged. Otherwise `PROD_MAX_DB_SIZE_ENV` (`AP_LBUG_MAX_DB_SIZE`), when
/// set, is used. Otherwise `DEFAULT_PROD_MAX_DB_SIZE_BYTES` (8 TiB — lbug's
/// own VM-region ceiling; see its doc comment for the 2026-08-14 repeal of
/// the 8 GiB cap) applies. The choke point still earns its keep after the
/// repeal: it is where the TEST bound and the operator override are
/// enforced, and where an env var that fails validation (not a power of
/// two, or below lbug's 8 MiB floor) is rejected with an actionable error
/// rather than silently falling back to a default the operator did not
/// choose.
pub fn system_config() -> Result<SystemConfig, String> {
    let config = SystemConfig::default();
    if let Ok(raw) = std::env::var(TEST_MAX_DB_SIZE_ENV) {
        let bytes = parse_and_validate_max_db_size(&raw, TEST_MAX_DB_SIZE_ENV)?;
        return Ok(config.max_db_size(bytes));
    }
    let bytes = match std::env::var(PROD_MAX_DB_SIZE_ENV) {
        Ok(raw) => parse_and_validate_max_db_size(&raw, PROD_MAX_DB_SIZE_ENV)?,
        Err(_) => DEFAULT_PROD_MAX_DB_SIZE_BYTES,
    };
    Ok(config.max_db_size(bytes))
}
