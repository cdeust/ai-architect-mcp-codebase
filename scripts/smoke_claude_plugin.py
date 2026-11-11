#!/usr/bin/env python3
"""Exercise the shipped Claude launcher against its source or release catalog.

With `--installed` the script first asks GitHub whether the release of the
plugin's version is published (`release_published`). Published: the launcher
must install the attested asset and answer the exact catalog of that tag.
Not published (a release pull request, or its merge before the tag's assets
exist): the launcher must refuse with its release-asset error and install
nothing (`check_unreleased`); the catalog is the `e2e` job's on that commit.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parent.parent
REPOSITORY = "cdeust/ai-architect-mcp-codebase"
# source: docs.github.com/en/rest/releases/releases ("Get a release by tag
# name", "Get the latest release"): a tag with no published release is a 404,
# and the latest release is never a draft or a prerelease.
RELEASES_API = f"https://api.github.com/repos/{REPOSITORY}/releases"
HTTP_NOT_FOUND = 404
# source: bin/ensure-binary.sh: `fatal` writes "<name>: FATAL: <text>" and
# exits 1; the first download failing is "release asset unavailable: <url>".
LAUNCHER_FATAL_EXIT = 1
BINARY = "ai-architect-mcp-codebase"
RELEASE_TARGETS = "macos-aarch64|linux-x86_64|linux-aarch64|windows-x86_64"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


def response(output: str, request_id: int) -> dict:
    for line in output.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict) and value.get("id") == request_id:
            return value
    raise AssertionError(f"missing response {request_id}: {output}")


def require_version(version: str) -> tuple[int, int, int]:
    """Post: `version` is SemVer 2.0.0, so it stays inside a fixed URL path;
    returns its core triple. https://semver.org/spec/v2.0.0.html#spec-item-9"""
    base, has_build, build = version.partition("+")
    core, has_pre, prerelease = base.partition("-")
    valid = bool(re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", core))
    for present, identifiers in ((has_pre, prerelease), (has_build, build)):
        if present:
            valid = valid and all(
                re.fullmatch(r"[0-9A-Za-z-]+", item) for item in identifiers.split(".")
            )
    if has_pre:
        valid = valid and all(
            not (item.isdigit() and len(item) > 1 and item.startswith("0"))
            for item in prerelease.split(".")
        )
    require(bool(valid), "invalid plugin version")
    major, minor, patch = core.split(".")
    return int(major), int(minor), int(patch)


def release_api(path: str) -> dict | None:
    """The JSON object GitHub answers for `releases/<path>`; None on a clean 404.
    Any other answer (rate limit, server error, no network, not an object)
    stops the run: an unknown release state is never read as "not released"."""
    headers = {
        "Accept": "application/vnd.github+json",
        "User-Agent": "ai-architect-mcp-codebase-install-smoke",
    }
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    try:
        with urlopen(Request(f"{RELEASES_API}/{path}", headers=headers)) as reply:
            value = json.loads(reply.read().decode("utf-8"))
    except HTTPError as exc:
        if exc.code == HTTP_NOT_FOUND:
            return None
        raise SystemExit(
            f"release state unavailable ({path}): HTTP {exc.code}"
        ) from exc
    except (OSError, ValueError) as exc:
        raise SystemExit(f"release state unavailable ({path}): {exc}") from exc
    require(isinstance(value, dict), f"release state unreadable ({path})")
    return value


def release_published(version: str) -> bool:
    """True when GitHub serves a published release for tag v<version>; False on
    a 404 or a draft (a draft's assets are not public)."""
    require_version(version)
    release = release_api(f"tags/v{version}")
    if release is None:
        return False
    require(
        release.get("tag_name") == f"v{version}"
        and isinstance(release.get("draft"), bool),
        f"release state unreadable (tags/v{version})",
    )
    return not release["draft"]


def require_ahead_of_latest_release(version: str) -> None:
    """Post: the unreleased `version` is above the latest published release, so
    a version that was skipped, or lowered, never takes the unreleased path."""
    latest = release_api("latest")
    if latest is None:
        return
    tag = latest.get("tag_name")
    match = re.fullmatch(r"v(\d+)\.(\d+)\.(\d+)", tag if isinstance(tag, str) else "")
    require(match is not None, f"latest release tag unreadable: {tag!r}")
    require(
        require_version(version) > tuple(map(int, match.groups())),
        f"v{version} is not released and is not above the latest release {tag}",
    )


def expected_tools(root: Path, version: str, *, installed: bool) -> set[str]:
    # Release installers execute the attested tag's binary, not unreleased HEAD.
    require_version(version)
    if installed:
        url = f"https://raw.githubusercontent.com/cdeust/ai-architect-mcp-codebase/v{version}/src/tool_profile.rs"
        try:
            with urlopen(url) as source:
                text = source.read().decode("utf-8")
        except (OSError, UnicodeError) as exc:
            raise SystemExit(
                f"release catalog unavailable for v{version}: {exc}"
            ) from exc
    else:
        text = (root / "src/tool_profile.rs").read_text(encoding="utf-8")
    match = re.search(
        r"FULL_TOOL_NAMES:\s*\[&str;\s*(\d+)\]\s*=\s*\[(.*?)\];", text, re.DOTALL
    )
    require(match is not None, "canonical FULL_TOOL_NAMES catalog missing")
    names = re.findall(r'"(\w+)"', match.group(2))
    require(
        len(names) == int(match.group(1))
        and len(names) == len(set(names))
        and bool(names),
        "canonical tool catalog is inconsistent",
    )
    return set(names)


def launcher_command(server: dict, root: Path, *, windows: bool) -> list[str]:
    command = [
        value.replace("${CLAUDE_PLUGIN_ROOT}", root.as_posix())
        for value in [server["command"], *server.get("args", [])]
    ]
    if windows and command[0] == "bash":
        # Resolve the manifest's executable, never substitute a different launcher.
        bash = shutil.which("bash")
        require(bash is not None, "Bash is required by the shipped Claude manifest")
        command[0] = bash
    return command


def check_tools(tools: list[dict], expected: set[str]) -> None:
    names = [tool["name"] for tool in tools]
    require(
        len(names) == len(set(names)) and set(names) == expected,
        f"Claude tool surface mismatch: missing={sorted(expected - set(names))}, unexpected={sorted(set(names) - expected)}, duplicates={len(names) - len(set(names))}",
    )


def check_health(called: dict, count: int) -> None:
    require(called.get("error") is None, "Claude health_check RPC failed")
    result = called["result"]
    require(result.get("isError") is not True, "Claude health_check failed")
    health = json.loads(result["content"][0]["text"])
    require(health.get("status") == "ok", "Claude health_check is not healthy")
    require(
        health.get("tools_count") == count, "Claude health_check catalog count mismatch"
    )


def check_unreleased(root: Path, version: str, run) -> None:
    """Post: the launcher, run as the plugin runs it, refused because the
    release asset of v<version> is a 404, and nothing else happened: no answer
    on stdout, no source build, no binary under target/release."""
    require(
        run.returncode == LAUNCHER_FATAL_EXIT,
        f"launcher exit {run.returncode} for unreleased v{version}, expected "
        f"{LAUNCHER_FATAL_EXIT}: {run.stderr}",
    )
    fatal = [line for line in run.stderr.splitlines() if "FATAL:" in line]
    asset = (
        f"https://github.com/{REPOSITORY}/releases/download/v{re.escape(version)}/"
        f"{BINARY}-({RELEASE_TARGETS})\\.tar\\.gz"
    )
    require(
        len(fatal) == 1
        and re.fullmatch(
            f"{BINARY}: FATAL: release asset unavailable: {asset}", fatal[0]
        )
        is not None,
        f"launcher did not refuse with the release-asset error: {run.stderr}",
    )
    # A refused connection or a timeout prints the same FATAL; only curl's own
    # 404 line shows that the asset is absent, not unreachable.
    require(
        re.search(r"^curl: \(22\) .*\b404\b", run.stderr, re.MULTILINE) is not None,
        f"release asset download failed, but not with a 404: {run.stderr}",
    )
    require(run.stdout.strip() == "", "launcher answered with no release to run")
    require("building from source" not in run.stderr, "launcher fell back to a build")
    installed = root / "target" / "release"
    require(
        not installed.exists() or not any(installed.glob(f"{BINARY}*")),
        f"launcher left a binary under {installed} with no release to install",
    )
    print(
        f"INSTALL: v{version} is not released yet; verified that the plugin "
        "installs and that its launcher refuses with the release-asset error "
        f"(exit {LAUNCHER_FATAL_EXIT}, 404 on the asset, no binary installed, no "
        "fallback); the tool catalog and health_check are covered by the e2e job "
        "on this commit, the download path by release.yml's "
        "publish_verified_release and by the next run after the release"
    )


def smoke_requests() -> list[dict]:
    return [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "claude-plugin-smoke", "version": "ci"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
        {
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "health_check", "arguments": {}},
        },
    ]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--installed",
        action="store_true",
        help="verify the attested release installed by the plugin bootstrap",
    )
    args = parser.parse_args()
    version = json.loads(
        (ROOT / ".claude-plugin/plugin.json").read_text(encoding="utf-8")
    )["version"]
    released = not args.installed or release_published(version)
    if released:
        expected = expected_tools(ROOT, version, installed=args.installed)
    else:
        require_ahead_of_latest_release(version)
    server = json.loads((ROOT / ".mcp.json").read_text(encoding="utf-8"))["mcpServers"][
        "ai-architect"
    ]
    requests = smoke_requests()
    environment = {**os.environ, "CLAUDE_PLUGIN_ROOT": ROOT.as_posix()}
    environment.pop("AP_PROFILE", None)
    environment["AI_ARCHITECT_SOURCE_CHECKOUT"] = "0" if args.installed else "1"
    run = subprocess.run(
        launcher_command(server, ROOT, windows=os.name == "nt"),
        env=environment,
        cwd=ROOT,
        input="\n".join(map(json.dumps, requests)) + "\n",
        text=True,
        encoding="utf-8",
        capture_output=True,
        check=False,
    )
    if not released:
        check_unreleased(ROOT, version, run)
        return
    require(run.returncode == 0, run.stderr)
    initialize = response(run.stdout, 1)
    require(initialize.get("error") is None, "Claude initialize failed")
    require(
        initialize["result"]["serverInfo"]["name"] == "ai-architect",
        "Claude server identity mismatch",
    )
    require(
        initialize["result"]["serverInfo"]["version"] == version,
        "Claude server version mismatch",
    )
    check_tools(response(run.stdout, 2)["result"]["tools"], expected)
    check_health(response(run.stdout, 3), len(expected))
    print(
        f"CLAUDE PLUGIN SMOKE OK: shipped launcher, v{version}, exact {len(expected)}-tool catalog, health_check ok"
    )


if __name__ == "__main__":
    main()
