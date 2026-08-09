#!/usr/bin/env bash
# scripts/fetch-derivation-corpus.sh — issue #232's rule auto-derivation
# needs a REAL, small, permissively-licensed corpus per language to validate
# derived rules by resolution (see src/parser/spec/structural_derive.rs's
# module doc). Corpora are NOT vendored into this repo (they are third-party
# source under their own licences) — this script re-fetches them into a
# scratch directory, and `structural_derive_tests.rs`'s network tests read
# from `$AP_DERIVE_CORPUS_DIR` (or the default below), skipping cleanly with
# a printed reason when the directory is absent.
#
# Every repository below was verified live via `gh api repos/<owner>/<repo>`
# for its licence and default branch, and the exact commit SHA pinned via
# `gh api repos/<owner>/<repo>/git/refs/heads/<branch>` on 2026-08-09 — see
# each block's comment. Re-running this script re-fetches the SAME commits.
#
# Usage: scripts/fetch-derivation-corpus.sh [target-dir]
set -euo pipefail

DEST="${1:-${AP_DERIVE_CORPUS_DIR:-/tmp/ap-derive-corpus}}"
mkdir -p "$DEST"/{go,python,sql,powershell}

fetch() {
  local repo="$1" path="$2" out="$3"
  gh api "repos/${repo}/contents/${path}" --jq '.content' | base64 -d > "$out"
}

# --- Go control: dustin/go-humanize, MIT (LICENSE verified via gh api),
# commit 4d1d9082551ec085912e7d2253a33ae547fca000 (master, 2026-08-09).
# Chosen: small (~170 LOC across 2 files), pure Go, cross-file calls
# (commaf.go's BigCommaf calls comma.go-adjacent helpers; comma.go defines
# Comma/Commaf/CommafWithDigits/BigComma).
fetch dustin/go-humanize comma.go "$DEST/go/comma.go"
fetch dustin/go-humanize commaf.go "$DEST/go/commaf.go"
# Test files included DELIBERATELY: comma.go/commaf.go's own functions call
# nothing IN this repo (each is a standalone formatter) — without a caller,
# the "definitions are referenced elsewhere" signal has nothing to measure.
# comma_test.go/commaf_test.go are the corpus's own real callers (`Comma(0)`,
# `BigCommaf(big.NewFloat(...))`, etc.), same commit.
fetch dustin/go-humanize comma_test.go "$DEST/go/comma_test.go"
fetch dustin/go-humanize commaf_test.go "$DEST/go/commaf_test.go"
echo "4d1d9082551ec085912e7d2253a33ae547fca000" > "$DEST/go/COMMIT_SHA.txt"

# --- Python control: tartley/colorama, BSD-3-Clause (verified via gh api
# license.spdx_id), commit pinned at fetch time (master, 2026-08-09).
# Chosen: small, pure Python, real cross-function calls (initialise.py's
# init/wrap_stream call into ansi.py/winterm.py definitions).
gh api repos/tartley/colorama/git/refs/heads/master --jq '.object.sha' > "$DEST/python/COMMIT_SHA.txt"
fetch tartley/colorama colorama/ansi.py "$DEST/python/ansi.py"
fetch tartley/colorama colorama/initialise.py "$DEST/python/initialise.py"
fetch tartley/colorama colorama/winterm.py "$DEST/python/winterm.py"

# --- SQL target: chlordk/pg_get_tabledef, MIT (LICENSE file), commit
# f7aaa7b4d8b52be27681a7d340e21459c725e311 (main, 2026-08-09).
# Chosen: tiny (14 KB) plpgsql repo with a real CREATE FUNCTION and a real
# documented SELECT invocation of it (README.rst's own usage example) — the
# exact shape `structural_held_out_sample_tests.rs` already documents as
# reaching neither definitions nor calls generically.
fetch chlordk/pg_get_tabledef pg_get_tabledef.sql "$DEST/sql/pg_get_tabledef.sql"
echo "f7aaa7b4d8b52be27681a7d340e21459c725e311" > "$DEST/sql/COMMIT_SHA.txt"
# The call site is the repo's own documented usage example (README.rst,
# "psql --command=\"SELECT pg_get_tabledef('foo')\"") — not itself a .sql
# file in the repo, so materialized here as one, verbatim, for the corpus.
cat > "$DEST/sql/usage_example.sql" <<'SQL'
-- source: chlordk/pg_get_tabledef README.rst usage example, commit
-- f7aaa7b4d8b52be27681a7d340e21459c725e311
SELECT pg_get_tabledef('foo');
SQL

# --- PowerShell target: dahlbyk/posh-git, MIT, commit
# bbc5ac380018239e0ac70411a59f4e367ca87f2d (master, 2026-08-09).
# Chosen: real, moderately-sized module with genuine same-file AND
# cross-reference function calls (Utils.ps1's Add-PoshGitToProfile/
# Remove-PoshGitFromProfile both call Test-Administrator; Get-PromptPath and
# Get-PromptConnectionInfo both call Get-PathStringComparison).
fetch dahlbyk/posh-git src/Utils.ps1 "$DEST/powershell/Utils.ps1"
fetch dahlbyk/posh-git src/AnsiUtils.ps1 "$DEST/powershell/AnsiUtils.ps1"
echo "bbc5ac380018239e0ac70411a59f4e367ca87f2d" > "$DEST/powershell/COMMIT_SHA.txt"

echo "Corpus fetched into $DEST"
