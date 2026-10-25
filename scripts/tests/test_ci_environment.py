"""Exercise environment setup without compiling Rust or claiming native OS proof."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

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


if __name__ == "__main__":
    unittest.main()
