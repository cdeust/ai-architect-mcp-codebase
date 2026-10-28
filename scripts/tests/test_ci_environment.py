"""Exercise environment setup without compiling Rust or claiming native OS proof."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

# unittest discovers imported TestCases through the existing CI entry point.
from ci_codex_current_findings_contract import CurrentFindingResolutionTests  # noqa: F401
from ci_codex_editor_contract import SummaryEditorTests, SummaryGraphQLTests  # noqa: F401

SCRIPT = Path(__file__).resolve().parents[1] / "ci_environment.sh"


class EnvironmentTests(unittest.TestCase):
    def run_shell(self, command, *arguments):
        environment = os.environ.copy()
        environment.pop("ZSTD_SYS_USE_PKG_CONFIG", None)
        return subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; shift; ' + command,
                "test",
                str(SCRIPT),
                *arguments,
            ],
            capture_output=True,
            text=True,
            env=environment,
        )

    def test_windows_copies_both_names_and_preserves_flags(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "OpenSSL libraries"
            source.mkdir()
            (source / "libssl.lib").write_bytes(b"ssl-import-library")
            (source / "libcrypto.lib").write_bytes(b"crypto-import-library")
            result = self.run_shell(
                'cygpath() { printf "%s" "$2"; }; '
                'export RUSTFLAGS="--cfg existing"; '
                'prepare_windows_openssl "$1/shim" "$1/missing" "$1/OpenSSL libraries"; '
                'printf "%s" "$RUSTFLAGS"',
                directory,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f"--cfg existing -L native={directory}/shim", result.stdout)
            aliases = (
                ("ssl", "ssl"),
                ("ssl", "libssl"),
                ("crypto", "crypto"),
                ("crypto", "libcrypto"),
            )
            for name, alias in aliases:
                expected = (source / f"lib{name}.lib").read_bytes()
                self.assertEqual(
                    (root / "shim" / f"{alias}.lib").read_bytes(), expected
                )

    def test_missing_windows_libraries_fail_before_cargo(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_shell(
                'prepare_windows_openssl "$1/shim" "$1/missing"; echo UNREACHABLE',
                directory,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("no libssl.lib/libcrypto.lib", result.stderr)
            self.assertNotIn("UNREACHABLE", result.stdout)

    def test_linux_exports_zstd_to_the_exact_cargo_probe(self):
        result = self.run_shell(
            "CI_OS=linux; CI_WORK=/unused; "
            'cargo() { printf "%s\\n" "$ZSTD_SYS_USE_PKG_CONFIG" "$*"; }; main'
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout.splitlines(),
            [
                "1",
                "run --release --locked --bin ai-architect-mcp-codebase -- install --dry-run",
            ],
        )

    def test_macos_preserves_the_cargo_failure(self):
        result = self.run_shell(
            "CI_OS=macos; CI_WORK=/unused; "
            'cargo() { test -z "${ZSTD_SYS_USE_PKG_CONFIG:-}"; return 42; }; main'
        )
        self.assertEqual(result.returncode, 42)

    def test_unknown_platform_never_runs_cargo(self):
        result = self.run_shell(
            "CI_OS=unknown; CI_WORK=/unused; cargo() { echo UNREACHABLE; }; main"
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("UNREACHABLE", result.stdout)


class UnitCommandTests(unittest.TestCase):
    def run_unit(self, inherited_incremental, coverage_exit=0):
        environment = os.environ.copy()
        environment["CARGO_INCREMENTAL"] = inherited_incremental
        environment["COVERAGE_EXIT"] = str(coverage_exit)
        command = json.loads((SCRIPT.parents[1] / "ci.json").read_text())["unit"]
        # Probe the actual adapter's environment and failure propagation without
        # installing packages or compiling the workspace in a Python unit test.
        probes = """
        sudo() { :; }
        rustup() { :; }
        cargo() {
            printf '%s:%s\\n' "$CARGO_INCREMENTAL" "$*"
            if [ "$1" = llvm-cov ]; then return "$COVERAGE_EXIT"; fi
        }
        """
        return subprocess.run(
            ["bash", "-eo", "pipefail", "-c", probes + command],
            capture_output=True,
            text=True,
            env=environment,
        )

    def test_coverage_receives_nonincremental_build_without_losing_suite_scope(self):
        result = self.run_unit("1")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = result.stdout.splitlines()
        self.assertTrue(calls[0].startswith("0:install cargo-llvm-cov "), calls)
        self.assertEqual(
            calls[1:],
            ["0:llvm-cov --workspace --locked --cobertura --output-path coverage.xml"],
        )

    def test_coverage_failure_is_not_hidden_by_the_adapter(self):
        result = self.run_unit("0", coverage_exit=42)
        self.assertEqual(result.returncode, 42, result.stderr)


if __name__ == "__main__":
    unittest.main()
