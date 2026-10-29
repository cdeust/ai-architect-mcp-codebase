"""Tests for scripts/check_lbug_glue_toolchain.py.

A guard is only useful if it can fail. Each case lays out a fake cargo target
with an lbug build directory and assigns every file a readelf `.comment` dump.
The dumps are the strings measured on the archive and the glue objects
(see .cargo/config.toml).
"""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

_SPEC = importlib.util.spec_from_file_location(
    "check_lbug_glue_toolchain",
    Path(__file__).resolve().parents[1] / "check_lbug_glue_toolchain.py",
)
guard = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(guard)

# source: readelf -p .comment on liblbug.a v0.21.2 compat, and on lbug glue objects
# built by g++ 13.4 and g++ 15.2 in ubuntu:26.04 (PR #468 investigation).
ARCHIVE_GCC13 = (
    "File: liblbug.a(copy_to.cpp.o)\n\nString dump of section '.comment':\n"
    "  [     1]  GCC: (GNU) 13.3.1 20240611 (Red Hat 13.3.1-2)\n"
)
GLUE_GCC13 = "String dump of section '.comment':\n  [     1]  GCC: (Ubuntu 13.4.0-10ubuntu1) 13.4.0\n"
GLUE_GCC15 = "String dump of section '.comment':\n  [     1]  GCC: (Ubuntu 15.2.0-16ubuntu1) 15.2.0\n"
# source: shape of a clang-built object's .comment (no "GCC:" line).
GLUE_CLANG = "String dump of section '.comment':\n  [     1]  Ubuntu clang version 20.1.8\n"


def layout(root: Path, comments: dict[str, str]) -> dict[Path, str]:
    out = root / "debug" / "build" / "lbug-0123456789abcdef" / "out"
    files = {}
    for relative, text in comments.items():
        path = out / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.touch()
        files[path] = text
    return files


def run(comments: dict[str, str]) -> list[str]:
    with TemporaryDirectory() as tmp:
        files = layout(Path(tmp), comments)
        return guard.check([Path(tmp)], comment=lambda path: files[path])


# source: lbug 0.21.2 build.rs prebuilt_lib_dir() layout, cache key version-<v>-compat.
ARCHIVE = "lbug-prebuilt/version-0.21.2-compat/lib/liblbug.a"


class GccMajors(unittest.TestCase):
    def test_reads_the_major_of_each_gcc_line(self) -> None:
        self.assertEqual(guard.gcc_majors(ARCHIVE_GCC13 + GLUE_GCC15), {13, 15})

    def test_a_non_gcc_compiler_names_no_major(self) -> None:
        self.assertEqual(guard.gcc_majors(GLUE_CLANG), set())


class Check(unittest.TestCase):
    def test_same_major_on_both_sides_passes(self) -> None:
        self.assertEqual(run({ARCHIVE: ARCHIVE_GCC13, "a-ffi.rs.o": GLUE_GCC13}), [])

    def test_the_measured_ubuntu_26_04_mismatch_fails_by_name(self) -> None:
        failures = run({ARCHIVE: ARCHIVE_GCC13, "a-ffi.rs.o": GLUE_GCC15})
        self.assertEqual(len(failures), 1)
        self.assertIn("glue compiled by GCC [15]", failures[0])
        self.assertIn("prebuilt liblbug.a by GCC [13]", failures[0])

    def test_one_mismatched_object_among_matching_ones_fails(self) -> None:
        failures = run(
            {ARCHIVE: ARCHIVE_GCC13, "a-ffi.rs.o": GLUE_GCC13, "b-lbug_rs.o": GLUE_GCC15}
        )
        self.assertEqual(len(failures), 1)
        self.assertIn("b-lbug_rs.o", failures[0])

    def test_an_unidentified_glue_compiler_fails_closed(self) -> None:
        failures = run({ARCHIVE: ARCHIVE_GCC13, "a-ffi.rs.o": GLUE_CLANG})
        self.assertIn("GCC unidentified", failures[0])

    def test_an_archive_without_glue_fails(self) -> None:
        self.assertIn("no compiled glue objects", run({ARCHIVE: ARCHIVE_GCC13})[0])

    def test_a_source_build_without_prebuilt_archive_passes(self) -> None:
        self.assertEqual(run({"a-ffi.rs.o": GLUE_GCC15}), [])

    def test_no_lbug_build_directory_fails(self) -> None:
        with TemporaryDirectory() as tmp:
            self.assertIn("no lbug build directory", guard.check([Path(tmp)])[0])


if __name__ == "__main__":
    unittest.main()
