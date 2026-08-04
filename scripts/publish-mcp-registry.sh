#!/bin/sh
# scripts/publish-mcp-registry.sh
#
# PUBLISH RUNBOOK — ai-architect-mcp-codebase MCP Registry submission
# =============================================================
#
# PURPOSE
#   After CI builds and uploads ai-architect-mcp-codebase.mcpb to a GitHub
#   Release, this script:
#     1. Downloads the released .mcpb bundle.
#     2. Computes its sha256.
#     3. Writes the sha256 into server.json .packages[0].fileSha256.
#     4. Prints the exact mcp-publisher commands for the maintainer to run.
#
#   It does NOT run mcp-publisher automatically. The maintainer reviews
#   the updated server.json, then runs the printed commands.
#
# PREREQUISITES
#   - curl (download the release asset)
#   - jq >= 1.6 (rewrite server.json in-place)
#   - mcp-publisher CLI (npm install -g @mcpmarket/publisher OR equivalent)
#   - GitHub CLI (gh) — optional, used for release URL verification
#
# USAGE
#   ./scripts/publish-mcp-registry.sh v0.5.0
#
# FULL PUBLISH RUNBOOK (for the maintainer)
# -----------------------------------------
# Step 1 — Bump version
#   Edit Cargo.toml: version = "X.Y.Z"
#   Edit plugin.json (if present): "version": "X.Y.Z"
#   Edit manifest.json: "version": "X.Y.Z"
#   Edit server.json: "version"/"identifier" (the v* in the URL)
#   Commit: git commit -m "chore: bump version to vX.Y.Z"
#
# Step 2 — Tag and push
#   git tag vX.Y.Z
#   git push origin main vX.Y.Z
#   → CI fires: builds 3 platform binaries + .mcpb → attaches to release
#
# Step 3 — Run this script (fills in sha256)
#   ./scripts/publish-mcp-registry.sh vX.Y.Z
#   Review the updated server.json (git diff server.json).
#
# Step 4 — Publish to MCP Registry
#   mcp-publisher login github
#   mcp-publisher publish
#   (or the exact commands printed by this script)
#
# Step 5 — Glama auto-index
#   Glama polls GitHub repos with glama.json present. No manual action.
#   Verify at https://glama.ai/mcp/servers after ~24 hours.
#
# Step 6 — Anthropic Directory (Claude Desktop bundle)
#   Submit via the Anthropic MCP Directory portal:
#   https://mcpindex.net/submit  (or the current Anthropic-maintained URL)
#   Attach server.json. The directory ingests the .mcpb via the identifier URL.
#
# -----------------------------------------

set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SERVER_JSON="$REPO_ROOT/server.json"

# --- Argument parsing ---
TAG="${1:-}"
if [ -z "$TAG" ]; then
    echo "Usage: $0 <tag>  (e.g. $0 v0.5.0)" >&2
    exit 1
fi

# Normalise: strip leading 'v' for version comparisons, keep full tag for URLs.
VERSION="${TAG#v}"
MCPB_URL="https://github.com/cdeust/ai-architect-mcp-codebase/releases/download/${TAG}/ai-architect-mcp-codebase.mcpb"

echo "==> Downloading ai-architect-mcp-codebase.mcpb from:"
echo "    ${MCPB_URL}"

TMP_DIR="$(mktemp -d)"
MCPB_PATH="$TMP_DIR/ai-architect-mcp-codebase.mcpb"

curl -fsSL --progress-bar "$MCPB_URL" -o "$MCPB_PATH"

echo "==> Computing sha256..."
SHA256="$(shasum -a 256 "$MCPB_PATH" | awk '{print $1}')"
echo "    sha256: ${SHA256}"

echo "==> Writing sha256 into server.json..."
TMP_JSON="$TMP_DIR/server.json"
# The 2025-12-11 server schema names this field `fileSha256` (camelCase);
# a snake_case `file_sha256` is not in Package.properties, so the registry
# ignored it and served whatever stale `fileSha256` the file already carried.
# source: definitions.Package.properties in
#   https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json
#   (measured 2026-08-02; v0.8.4 shipped sha 662b2619… against the real bf9cb117…)
jq --arg sha "$SHA256" \
   '.packages[0].fileSha256 = $sha | del(.packages[0].file_sha256)' \
   "$SERVER_JSON" > "$TMP_JSON"
mv "$TMP_JSON" "$SERVER_JSON"

echo "==> server.json updated. Review:"
jq '.packages[0]' "$SERVER_JSON"

echo ""
echo "==> Run the following commands to publish:"
echo ""
echo "    mcp-publisher login github"
echo "    mcp-publisher publish"
echo ""
echo "==> After publishing:"
echo "    - MCP Registry: https://registry.modelcontextprotocol.io/servers/io.github.cdeust/ai-architect-mcp-codebase"
echo "    - Glama: auto-indexed within ~24h (no action required)"
echo "    - Anthropic Directory: submit server.json at the MCP Directory portal"
echo ""

rm -rf "$TMP_DIR"
