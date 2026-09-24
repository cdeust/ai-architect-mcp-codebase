# Installing and registering the server

The [README](../README.md#install-and-register) has the short path. This page
has every host, the automatic installer, the Claude Code plugin's release
verification, and the developer escape hatch.

The server is a self-contained stdio binary, so any MCP host can launch it.
The examples use the `core` profile, which registers the eight read-only
code-intelligence tools. See [Tool profiles](../README.md#tool-profiles).

## Contents

- [Build from source](#build-from-source)
- [Install the binary](#install-the-binary)
- [Automatic host configuration](#automatic-host-configuration)
- [Claude Code plugin](#claude-code-plugin)
- [Developer escape hatch: running a local dev build in place of the release](#developer-escape-hatch-running-a-local-dev-build-in-place-of-the-release)
- [Migrating from the Automatised Pipeline plugin](#migrating-from-the-automatised-pipeline-plugin)
- [Configuring a host by hand](#configuring-a-host-by-hand)

## Build from source

Rust 1.95.0 is pinned by [`rust-toolchain.toml`](../rust-toolchain.toml), so
`rustup` installs and selects it for you. CI and the releases use the same
compiler. CMake is also required, because LadybugDB builds its C++ core from
source.

```bash
git clone https://github.com/cdeust/ai-architect-mcp-codebase.git
cd ai-architect-mcp-codebase
cargo build --release
```

The first build takes about five minutes while it compiles the LadybugDB C++
core. Later builds are incremental.

To check the handshake, pipe three JSON-RPC requests into the binary:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"health_check","arguments":{}}}' \
  | ./target/release/ai-architect-mcp-codebase
```

## Install the binary

```bash
cargo install ai-architect-mcp-codebase   # installs into ~/.cargo/bin
```

The commands below assume `~/.cargo/bin` is on your `PATH`. GUI hosts
(Cursor, Windsurf, VS Code) may not inherit your shell `PATH`; in their JSON
configs, replace `ai-architect-mcp-codebase` with the output of
`which ai-architect-mcp-codebase`.

## Automatic host configuration

One command detects the installed hosts and writes the MCP entry for each:

```bash
ai-architect-mcp-codebase install
```

It configures six hosts when it finds them: Claude Code (`~/.claude.json`),
Codex CLI (`~/.codex/config.toml`), Gemini CLI (`~/.gemini/settings.json`),
Cursor (`~/.cursor/mcp.json`), VS Code (`Code/User/mcp.json`) and Zed
(`~/.config/zed/settings.json`).

The installer parses the existing file and adds or updates only the
`ai-architect` entry, so every other server in the file is kept. A file it
cannot parse safely is never overwritten; the installer prints the exact entry
to paste by hand instead. Zed's `settings.json` allows comments, which strict
JSON editing would destroy, so the installer never edits it in place and
prints the snippet with instructions. Codex's TOML is edited with `toml_edit`,
which keeps comments and formatting.

Flags: `--dry-run` prints the planned changes and writes nothing.
`--only <host>` and `--skip <host>` filter the host list, and `--only` forces a
host even when it was not detected. `--with-hooks` also registers the
Grep/Glob hook described below. A second run reports "no change".
`ai-architect-mcp-codebase uninstall` removes exactly the entries (and the
hook) that `install` added.

```bash
ai-architect-mcp-codebase install --dry-run                 # preview
ai-architect-mcp-codebase install --only cursor --only zed  # just these
ai-architect-mcp-codebase install --with-hooks              # add the grep-to-graph hook
ai-architect-mcp-codebase uninstall                         # remove our entries
```

Measured on the maintainer's machine in July 2026: `install` completed in
about 1.3 s, most of it process and database startup. `analyze_codebase` on
this repository's own `src/` (114 files, 16.5k nodes, 16.3k edges; index,
resolve and cluster) took about 12 s wall time, and the first
`search_codebase` returned at once. The one-time `cargo build --release` comes
before that and is not included.

### Fail-open grep-to-graph hook

`ai-architect-mcp-codebase install --with-hooks` registers a Claude Code
`PreToolUse` hook (matcher `Grep|Glob`) that runs
`ai-architect-mcp-codebase hook-augment`. Before a Grep or Glob in a project
that has an ai-architect graph, the hook injects a one-line suggestion to try
`search_codebase` or `query_graph` first. It never blocks the tool call: with
no graph, an unparseable payload or any error, it prints nothing and exits 0.
The hook is registered only when you pass `--with-hooks`.

## Claude Code plugin

```bash
claude plugin marketplace add cdeust/ai-architect-mcp-codebase
claude plugin install ai-architect-mcp-codebase@ai-architect-mcp-codebase-marketplace
```

The plugin's `.mcp.json` starts `bin/launch-plugin.sh` with no arguments, so
the plugin runs the `full` profile unless `AP_PROFILE=core` is set in the
environment Claude Code launches it from.

Fresh marketplace installs require GitHub CLI 2.68 or newer. The bootstrap
(`bin/ensure-binary.sh`) verifies the release's attached Sigstore bundle
against the fixed
`cdeust/ai-architect-mcp-codebase/.github/workflows/release.yml` signer before
it installs any executable, and it never accepts a trust anchor supplied by a
manifest. The bundle avoids a Rekor transparency-log lookup, but `gh` can
still need the network to refresh Sigstore's TUF trust root on a cold cache.

This protects the official package, and a minimal-diff fork that changes only
metadata fails closed. It cannot make arbitrary code from a hostile fork
trustworthy, because such a fork can also replace the bootstrap itself. Check
that the marketplace slug is exactly `cdeust/ai-architect-mcp-codebase`.

## Developer escape hatch: running a local dev build in place of the release

`bin/ensure-binary.sh` pins the installed binary to a verified release digest
(see [security](security.md#release-verification)). The pin rejects any binary
the bootstrap did not download and verify itself, including one you rebuilt
from source on purpose. Set `AI_ARCHITECT_SOURCE_CHECKOUT=1` to opt out of the
pin for a local dev build. The bootstrap accepts two layouts under this flag,
and both need the explicit opt-in; the flag is never inferred from metadata.

In a plain source checkout, `$CLAUDE_PLUGIN_ROOT` itself contains `.git`,
because you registered a clone directly as the plugin root.

In a live-mount layout, the installed binary at
`target/release/ai-architect-mcp-codebase` is a symlink whose fully resolved
target lies outside `$CLAUDE_PLUGIN_ROOT`, inside its own `.git`-bearing
checkout. An example is a marketplace cache whose binary was replaced with a
symlink into a separate dev clone, so you can iterate without reinstalling the
plugin after every rebuild. This layout was added in
[#208](https://github.com/cdeust/ai-architect-mcp-codebase/pull/208). A plain
`.git`-at-root check cannot see it, because a marketplace cache has no `.git`
of its own.

The flag skips only the release-binary digest verification (`sha256sum`
against the cached or pinned digest) and, for a fresh install, the download
and Sigstore provenance check, for that one launch. The `Cargo.toml` and
`plugin.json` presence checks still run and still fail if either file is
missing. A plain source checkout still gets the freshness rebuild
(`cargo build --release` when `src/` is newer than the binary). For the
live-mount layout nothing rebuilds the binary; the bootstrap uses the
already-built binary the symlink resolves to, as it is.

The flag is an explicit opt-in that the user sets, and packaged
metadata cannot trigger it. An attacker who can already write to your plugin
cache, and could replace the installed binary with a symlink to force this
path, could just as easily replace `bin/ensure-binary.sh` or
`bin/launch-plugin.sh`. The digest pin was never a defense against that
attacker. It defends the default path (flag unset), where the bootstrap is
what stands between a marketplace download and your shell. This escape hatch
leaves the default path unchanged: any digest mismatch there is still fatal.
Every accepted bypass is announced on `stderr`, even in quiet mode:

```
ai-architect-mcp-codebase: bootstrap verification skipped (source-checkout mode)
ai-architect-mcp-codebase: live-mounted dev symlink: <plugin-cache>/target/release/ai-architect-mcp-codebase -> <resolved dev path> (source checkout at <resolved .git root>)
```

If a marketplace-cache binary is
replaced by a live-mount symlink and `AI_ARCHITECT_SOURCE_CHECKOUT` is not
set, Claude Code shows only `MCP error -32000: Connection closed`. The real
cause is on stderr, which Claude Code does not surface for a failed MCP
launch. Run the launcher by hand with `CLAUDE_PLUGIN_ROOT` set to the plugin
cache directory to see it:

```bash
CLAUDE_PLUGIN_ROOT=/path/to/plugin/cache bin/launch-plugin.sh
# ai-architect-mcp-codebase: FATAL: cached binary digest mismatch; reinstall the plugin
```

An `export AI_ARCHITECT_SOURCE_CHECKOUT=1` in `~/.zshrc` alone is not enough.
`~/.zshrc` is read only by interactive shells, and the Claude Code plugin
launcher and its hooks run in non-interactive ones. Put the export in
`~/.zshenv`, or your shell's equivalent non-interactive startup file.

## Migrating from the Automatised Pipeline plugin

If the former Automatised Pipeline plugin is installed, remove it before you
install the current package:

```bash
claude plugin uninstall automatised-pipeline@automatised-pipeline-marketplace
claude plugin marketplace remove automatised-pipeline-marketplace
```

Claude MCP allowlists and permissions must also replace every prefix listed in
`revoked_claude_tool_prefixes` in the contract with
`mcp__plugin_ai-architect-mcp-codebase_ai-architect__<tool>`. The final
`ai-architect` segment is stable on purpose: it is the MCP server key, and the
plugin's distribution name is a separate field. The machine-readable source of
truth is [`mcp-contract.json`](../mcp-contract.json); consumer repositories
validate their allowlists against its derived `claude_tool_prefix` and do not
keep a spelling of their own.

Contract schema 1 requires `distribution`, `claude_plugin`,
`claude_marketplace`, `mcp_server`, `claude_tool_prefix` and
`revoked_claude_tool_prefixes`. Consumers must pin the raw contract URL to the
full commit SHA (tags can be moved), check that the prefix equals
`mcp__plugin_<claude_plugin>_<mcp_server>__`, and remove revoked prefixes from
allowlists instead of keeping them as aliases. Consumer PRs record the full
producer commit in their contract URL. The same contract ships in the crate,
the MCPB bundle and the signed release assets.

For Gemini CLI, uninstall the former extension identity before you reinstall
from the renamed repository:

```bash
gemini extensions uninstall ai-architect
gemini extensions install https://github.com/cdeust/ai-architect-mcp-codebase
```

## Configuring a host by hand

### Gemini CLI

```bash
gemini mcp add -e AP_PROFILE=core ai-architect ai-architect-mcp-codebase
```

Or install it as an extension (the repository ships a `gemini-extension.json`):

```bash
gemini extensions install https://github.com/cdeust/ai-architect-mcp-codebase
```

The extension also exposes three workflows from `skills/`:
`understand-codebase`, `impact-analysis` and `validate-change-plan`. They use
only the eight tools of the `core` profile and surface index coverage gaps
before they accept a negative graph result.

### OpenAI Codex CLI

The ChatGPT desktop app and the Codex IDE extension read the same
`~/.codex/config.toml`.

```bash
codex mcp add ai-architect -- ai-architect-mcp-codebase --profile core
```

Or in `~/.codex/config.toml`:

```toml
[mcp_servers.ai-architect]
command = "ai-architect-mcp-codebase"
args = ["--profile", "core"]
```

Or install the packaged Codex plugin and its three matching skills from this
repository's marketplace:

```bash
cargo install ai-architect-mcp-codebase
codex plugin marketplace add cdeust/ai-architect-mcp-codebase
codex plugin add ai-architect-mcp-codebase@ai-architect-mcp-codebase
```

The Codex package lives under `plugins/ai-architect-mcp-codebase/`, with its
own `.mcp.json` fixed to `--profile core`. The root `.mcp.json` stays the Claude
plugin configuration and keeps the server's backward-compatible `full`
default.

### Cursor

`.cursor/mcp.json` (project) or `~/.cursor/mcp.json` (global):

```json
{
  "mcpServers": {
    "ai-architect": {
      "command": "ai-architect-mcp-codebase",
      "args": ["--profile", "core"]
    }
  }
}
```

### Windsurf

`~/.codeium/windsurf/mcp_config.json` takes the same `mcpServers` block as
Cursor.

### VS Code

`.vscode/mcp.json`:

```json
{
  "servers": {
    "ai-architect": {
      "type": "stdio",
      "command": "ai-architect-mcp-codebase",
      "args": ["--profile", "core"]
    }
  }
}
```

### Claude Code without the plugin

```bash
claude mcp add ai-architect -- /absolute/path/to/ai-architect-mcp-codebase --profile core
```

### OpenAI Agents SDK (Python)

```python
from agents.mcp import MCPServerStdio

async with MCPServerStdio(
    name="ai-architect",
    params={"command": "ai-architect-mcp-codebase", "args": ["--profile", "core"]},
) as server:
    agent = Agent(name="Assistant", mcp_servers=[server])
```
