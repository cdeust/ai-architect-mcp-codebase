#!/usr/bin/env python3
"""Fail when lbug's prebuilt archive and its locally compiled glue use different GCC majors.

The rationale and the measurements are in the CXX_*_linux_gnu entries of
.cargo/config.toml. When the two sides come from different GCC majors, they
carry two incompatible copies of the inline std::format templates. The linker
keeps one of them, so the result is heap corruption at run time and no build
error. This check makes that mismatch a named CI failure.

Usage: check_lbug_glue_toolchain.py TARGET_DIR [TARGET_DIR ...]

Every lbug build directory found under the given cargo target directories is
checked: <target>/**/build/lbug-*/out. A build directory without a prebuilt
liblbug.a was built from source by one compiler, so it is consistent and is
reported. Finding no lbug build directory at all is an error, because the check
would then have proved nothing.
Source for the .comment section: readelf(1), `-p .comment`.
"""

from __future__ import annotations

from collections.abc import Callable, Iterable
from pathlib import Path
import re
import subprocess
import sys

GCC_COMMENT = re.compile(r"GCC: \([^)]*\) (\d+)\.\d+")


def gcc_majors(comment: str) -> set[int]:
    """Return every GCC major named in a readelf `.comment` dump."""
    return {int(major) for major in GCC_COMMENT.findall(comment)}


def read_comment(path: Path) -> str:
    """Return the `.comment` dump of an object file or of every archive member."""
    result = subprocess.run(
        ["readelf", "-p", ".comment", str(path)],
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout


def check_build(out_dir: Path, comment: Callable[[Path], str]) -> str | None:
    """Return the failure for one lbug build directory, or None when it is consistent."""
    archives = sorted(out_dir.glob("lbug-prebuilt/*/lib/liblbug.a"))
    glue = sorted(out_dir.glob("*.o"))
    if not archives:
        print(f"{out_dir}: no prebuilt liblbug.a (source build), consistent by construction")
        return None
    if not glue:
        return f"{out_dir}: prebuilt archive present but no compiled glue objects"
    archive_majors = set().union(*(gcc_majors(comment(path)) for path in archives))
    if len(archive_majors) != 1:
        return f"{out_dir}: prebuilt archive GCC majors {sorted(archive_majors)}, expected one"
    for path in glue:
        majors = gcc_majors(comment(path))
        if majors != archive_majors:
            return (
                f"{path}: glue compiled by GCC {sorted(majors) or 'unidentified'}, "
                f"prebuilt liblbug.a by GCC {sorted(archive_majors)}; "
                "set CXX_<target> in .cargo/config.toml to that major"
            )
    print(f"{out_dir}: archive and {len(glue)} glue objects all GCC {sorted(archive_majors)}")
    return None


def check(targets: Iterable[Path], comment: Callable[[Path], str] = read_comment) -> list[str]:
    """Check every lbug build directory under the targets; return the failures."""
    builds = sorted({path for target in targets for path in target.glob("**/build/lbug-*/out")})
    if not builds:
        return ["no lbug build directory found; run this after a cargo build that links lbug"]
    return [failure for out in builds if (failure := check_build(out, comment)) is not None]


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__, file=sys.stderr)
        return 2
    failures = check(Path(arg) for arg in argv)
    for failure in failures:
        print(f"lbug glue toolchain: {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
