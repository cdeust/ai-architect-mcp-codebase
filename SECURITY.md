# Security Policy

## Reporting a Vulnerability

If you discover a security issue in this project, **do not** open a public
issue. Instead, send a private report to the maintainer.

**Preferred channel:** open a [private security advisory on
GitHub](https://github.com/cdeust/ai-architect-mcp-codebase/security/advisories/new).
It keeps the report, the discussion and the eventual CVE in one place, and it
is private to you and the maintainer.

**Fallback channel:** email **hello@ai-architect.tools**.

Use the fallback whenever the advisory form does not work for you — you are
not signed in to GitHub, you do not have a GitHub account, or the form is
simply unavailable. The advisory form is a GitHub feature that a repository
setting can switch off; this document previously named it as the *only* way
in, so turning that setting off silently left reporters with no private
channel at all (issue #159). The mailbox does not depend on that setting, and
the SLA below applies identically to both channels.

Please do not send encrypted mail unless you have agreed a key with the
maintainer first — no OpenPGP key is advertised here, because publishing one
the maintainer cannot reliably decrypt with would be worse than plaintext.

Include:

- Affected version (or commit SHA)
- Reproduction steps or proof of concept
- Impact assessment (what does an exploit accomplish?)
- Suggested fix, if you have one

## Response SLA

| Severity | First response | Patch / mitigation |
|---|---|---|
| Critical (RCE, data exfiltration, auth bypass) | 24 hours | 7 days |
| High | 3 days | 14 days |
| Medium / Low | 7 days | Best effort |

## Supported Versions

Only the latest minor release on `main` receives security patches.

## Disclosure Timeline

1. Reporter sends a private advisory, or emails the fallback address.
2. Maintainer acknowledges receipt within the first-response SLA.
3. Maintainer + reporter agree on a coordinated disclosure date (default
   30 days from the patched release).
4. Patched release ships; reporter is credited unless they prefer
   anonymity.
5. Public advisory published on the agreed date.

## What this tool accesses, and what assurance is offered

Being plain about this is the point: `ai-architect-mcp-codebase` **reads your entire
source tree** to build its graph. That is what it is for. It is also exactly
the access an attacker would want, so the honest question is not whether it
reads your code but whether the binary you ran is the one we built.

**Access.** It reads every file under the indexed path (source, config, docs,
binaries as `File` nodes), reads git history when co-change mining is enabled,
and writes a graph database under the output directory you name. It makes no
network calls during indexing; all processing is local.

**Assurance, as of the release that includes issue #66:**

| Property | How you check it |
|---|---|
| The artifact was built by our release workflow and stable tag | `gh attestation verify <file> --repo cdeust/ai-architect-mcp-codebase --signer-workflow cdeust/ai-architect-mcp-codebase/.github/workflows/release.yml --source-ref refs/tags/v0.10.0 --bundle <file>.sigstore.json` |
| The bytes were not altered in transit | `sha256sum -c <file>.sha256` |
| The release tag was cut by the maintainer, not by whoever can push | **not claimed** — tags are unsigned; artifact provenance answers this instead (see below) |
| What is inside the binary | the CycloneDX SBOM asset, `ai-architect-mcp-codebase.cdx.json` |
| Dependencies carry no known advisory | `cargo audit` / `cargo deny`, run daily in CI |
| Repo-level supply-chain posture | OpenSSF Scorecard, published weekly |

Verify a downloaded release before running it:

```bash
gh attestation verify ai-architect-mcp-codebase-macos-aarch64.tar.gz \
  --repo cdeust/ai-architect-mcp-codebase \
  --signer-workflow cdeust/ai-architect-mcp-codebase/.github/workflows/release.yml \
  --source-ref refs/tags/v0.10.0 \
  --bundle ai-architect-mcp-codebase-macos-aarch64.tar.gz.sigstore.json
sha256sum -c ai-architect-mcp-codebase-macos-aarch64.tar.gz.sha256
```

Every attested asset also ships its Sigstore bundle as a sibling
`<asset>.sigstore.json` on the release page. The bundle keeps the signed
statement with the artifact and avoids a Rekor transparency-log lookup.
`gh attestation verify` can still fetch Sigstore's TUF trust root, so a cold
cache requires network access even when `--bundle` is supplied.

### Verifying the release tag

**Release tags are annotated but not signed, by design.** The tampering risk
this project defends against lands on the *artifact* a user downloads and runs,
not on the tag object, and that is where the assurance is spent: every asset
carries a SHA-256 companion, a Sigstore build-provenance attestation, and the
attestation bundle published beside it. A signed tag would add a second,
weaker statement about the same release — weaker because a tag signature says
who typed `git tag`, while provenance says which workflow, from which commit,
produced these exact bytes.

The signing machinery is nonetheless committed and ready, so a future maintainer
can turn it on without re-deriving it: [`.github/allowed_signers`](.github/allowed_signers)
carries the authorized identity `cdeust@icloud.com` bound to an `ssh-ed25519`
key scoped to the `git` namespace, and the repository sets `gpg.format=ssh`,
`tag.gpgsign=true` and `gpg.ssh.allowedSignersFile`. Do not read the presence of
that file as a signed release. Once a tag is signed, this is how you check it:

```bash
git config gpg.ssh.allowedSignersFile .github/allowed_signers
git tag -v v0.8.3     # → "Good \"git\" signature for cdeust@icloud.com"
```

Trusting a key that ships in the repository you are verifying is circular on
its own: it proves every tag was signed by one consistent key, not that the key
is the maintainer's. Cross-check it against the same file in an independently
obtained clone, or against the key published on the GitHub account, before
treating it as an identity rather than a continuity guarantee.

**No tag is signed today.** `v0.8.0` is annotated but carries no signature,
and `v0.8.1`/`v0.8.2` are lightweight tags with no object to sign. From `v0.8.3`
onward tags are at least annotated objects rather than bare refs. Earlier tags
will not be retro-signed — re-tagging would change objects users have already
fetched.

**Limits, stated rather than implied.** Provenance proves *who built it and
from which commit*; it does not prove the source is free of defects, and it is
worthless if you never run the verification. Binaries are **not** yet
Apple-notarized, so macOS Gatekeeper will still prompt: that work is tracked in
cdeust/enterprise-backlog#15 and is not claimed here.

### Developer escape hatch (`AI_ARCHITECT_SOURCE_CHECKOUT=1`)

The release-digest pin described above can be opted out of for a local dev
build — plain source checkout or a live-mounted dev-symlink montage over a
marketplace cache — via `AI_ARCHITECT_SOURCE_CHECKOUT=1`. Full mechanics in
[README §Developer escape
hatch](README.md#developer-escape-hatch-running-a-local-dev-build-in-place-of-the-release).
The relevant security properties:

- **Explicit and local only.** The flag is a shell environment variable the
  developer sets; it is never read from a manifest, config file, or anything
  a package can ship, so a malicious plugin release cannot trigger it.
- **Scoped bypass.** It skips only the digest/provenance check on the
  binary for that launch. It does not weaken the `Cargo.toml` /
  `plugin.json` presence checks, and it does not touch the release download
  path at all when unset (the default, which stays a hard `fatal` on any
  mismatch).
- **Not a new attack surface.** Anyone with write access to the plugin cache
  — the precondition for the montage shape to even exist — already has write
  access to `bin/ensure-binary.sh` and `bin/launch-plugin.sh` themselves, so
  they could disable the pin entirely without this flag. The digest pin's
  actual job is defending the *unmodified* default install path (an
  untampered cache, flag unset) against a corrupted or substituted release
  download, and that job is unaffected by the existence of this opt-in.
- **Always announced.** Every accepted bypass is written to stderr — even
  when the launcher requested quiet mode — naming the resolved dev path, so
  a bypass in effect is never silent to whoever is watching the process.

## Out of Scope

- Vulnerabilities in third-party dependencies that have not been patched
  upstream — please report those upstream first.
- Issues that require an attacker to already have control of the host
  process (in-process supply-chain attacks).
- Self-inflicted misconfigurations of your own MCP server registration.

## Recognition

Reporters who follow this disclosure process are credited in the release
notes for the patched version, unless they explicitly request anonymity.
