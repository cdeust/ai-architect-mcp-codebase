# Native bot CI configuration

Generated from zetetic-team-subagents commit df381f830ed2cf40b204002a15fe2a695f860dd2 using render-ci --native-review. The existing ci.json remains byte-identical (SHA256 45801944978e2093c212192c576ad7f8c250705d9b215851ceb79a3bced2ed38). Product Rust files and Cargo dependencies are unchanged.

The native profile consumes authenticated Codex code/security completion, runs the official Anthropic review command and five reviewers, and preserves protected merge. Claude App authentication uses pull_request and OIDC; no GitHub token override or required Claude installation gate is added. The CLI installation now uses a committed npm lockfile with package integrity hashes.

Six workflow actionlint checks and the actual Python reader --help pass. The canonical renderer passed 48 affected tests and seven legacy shell checks; an isolated installation returned Claude Code 2.1.306 without invoking a model. Hosted tests and actual App-auth publication remain to verify after protected workflow installation.

Owner selection: both Claude workflows use claude-opus-5-5; all five upstream reviewer definitions inherit that model. Model reference: https://platform.claude.com/docs/en/models/opus-5-5/overview.
