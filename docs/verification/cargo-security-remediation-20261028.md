# Cargo security remediation, 2026-10-28

The actual Supply-chain audit run 40623301381/job 122602203599 reported seven vulnerable package/advisory combinations and 22 unsound warnings. The original lockfile independently reproduces those findings against RustSec database 99670cfc0bc574433034b75876a4b791ccaa2efb.

Tantivy 0.27.0 replaces its vulnerable lru 0.16 and tantivy-sstable 0.7/zstd 0.13 branches. The resolved graph contains lru 0.18.5, one zstd 0.14.3, zstd-safe 8.2.0 and zstd-sys 2.1.2. Allowed cxx/cxx-build dependencies move from 1.0.138 to 1.0.205. Ladybug remains lbug 0.21.2; no authoritative graph store format or application algorithm changes. The obsolete cxx advisory exception is removed from deny.toml.

Sources: [zstd](https://rustsec.org/advisories/RUSTSEC-2026-0391.html), [zstd-safe](https://rustsec.org/advisories/RUSTSEC-2026-0398.html), [legacy-frame fix](https://rustsec.org/advisories/RUSTSEC-2026-0403.html), [zstd-sys](https://rustsec.org/advisories/RUSTSEC-2026-0410.html), [cxx](https://rustsec.org/advisories/RUSTSEC-2026-0202.html), [lru](https://rustsec.org/advisories/RUSTSEC-2026-0253.html). Official crates.io dependency metadata confirms Tantivy 0.27 uses lru ^0.18.2 and sstable 0.27/zstd 0.14; lbug 0.21.2 already permits patched cxx through its caret requirement. PR454 fixes only the direct zstd branch and leaves vulnerable sstable 0.7; its unrelated parser/serde changes were not copied.

Validation:

- Real cargo audit with --deny warnings: zero vulnerabilities and no warning classes, without ignores or target filters.
- Existing cargo deny --all-features check: advisories, licenses, bans and sources pass. Existing duplicate-version warning policy remains unchanged.
- Actual ZERA crate: 19 tests pass against patched zstd.
- Standalone consumer extracts query_index, decode_hits, field_text and tokenize_symbol verbatim from current source, includes the actual bm25_schema.rs, and extracts the actual snapshot write_archive function. Both official Tantivy versions compile it. Actual archive writer plus decoder/tar roundtrips preserve graph payload and sidecar at existing compression levels 3 and 9 before/after.
- Tantivy 0.27 opens a persisted 0.26.2 index and builds a fresh same-corpus index. All result keys/ranks and every f32::to_bits score match exactly, with no rounding or tolerance. Three fixture queries have 3/3 top1 hits and mean reciprocal rank 1.0 before/after. This is fixture compatibility evidence, not a broad retrieval/performance claim.
- Both official sources declare index format 7 and oldest supported 4. Their release notes do not require API changes to the query/schema APIs used here. No index deletion or authoritative data migration is introduced.
- 96 existing Codex CI contracts pass; dependency-only bootstrap pins still match; Ruff and source/craft checks pass.

Reproduce in a registered disposable directory, with its private CARGO_HOME and CARGO_TARGET_DIR and CARGO_BUILD_JOBS=2. Generate the baseline consumer with `python3 scripts/verify_security_consumer.py <scratch>/consumer 0.26.2 --zstd-version 0.14.0`; run its manifest with `create <scratch>/old-index`. Generate 0.27.0 with zstd 0.14.3, then run `read <scratch>/old-index` and `create <scratch>/new-index`. Compare standard output byte-for-byte. Run `cargo test -p zera --locked`, `cargo audit --deny warnings` with the private advisory database, and `cargo deny --all-features check`. The adjacent JSON preserves exact hashes, database revision and lossless query output.

No full workspace/native Ladybug build was run on the shared Mac. Hosted CI must validate that complete consumer before merge. All local compilation used registered private scratch; no user/global Cargo cache was altered.

The same push includes only the independently approved explicit checks: read permission on scheduled discovery, copied byte-for-byte from the canonical workflow. No reviewer/model job is changed. Root independently reviewed and explicitly approved the complete patch before publication.
