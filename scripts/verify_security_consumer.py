#!/usr/bin/env python3
"""Generate a bounded crate using real BM25 functions without building Ladybug.

source: docs/verification/cargo-security-remediation-20261028.md.
The generated crate is disposable; caller owns its output directory and Cargo home.
"""

import argparse
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def consumer_source():
    source = (ROOT / "src/search/bm25.rs").read_text()
    first = source.index("pub struct Bm25Result")
    last = source.index("\n}", first) + len("\n}")
    structure = source[first:last]
    first = source.index("pub fn query_index(")
    last = source.index(
        "// ---------------------------------------------------------------------------\n// Helpers",
        first,
    )
    query = source[first:last]
    first = source.index("fn field_text(")
    last = source.index("#[cfg(test)]", first)
    helpers = source[first:last]
    header = """use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Value as _};
use tantivy::{doc, Index, IndexWriter, TantivyDocument};
"""
    schema = json.dumps(str(ROOT / "src/search/bm25_schema.rs"))
    header += f"#[path={schema}]\nmod schema;\nuse schema::OpenedFields;\n"
    fixture = (ROOT / "scripts/tests/bm25_security_fixture.rs").read_text()
    artifact = (ROOT / "src/artifact.rs").read_text()
    first = artifact.index("fn write_archive(")
    last = artifact.index(
        "\n// ---------------------------------------------------------------------------",
        first,
    )
    archive = artifact[first:last]
    header += "use std::fs::{self, File};\nuse std::path::PathBuf;\n"
    return header + structure + "\n" + query + helpers + archive + fixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("version", help="Official Tantivy crate version to compare")
    parser.add_argument("--zstd-version", default="0.14.3")
    args = parser.parse_args()
    (args.output / "src").mkdir(parents=True, exist_ok=True)
    (args.output / "src/main.rs").write_text(consumer_source())
    manifest = """[package]
name="actual-bm25-security-consumer"
version="0.0.0"
edition="2021"
[dependencies]
"""
    manifest += "tantivy=" + json.dumps("=" + args.version) + "\n"
    manifest += "zstd=" + json.dumps("=" + args.zstd_version) + "\n"
    manifest += 'tar="0.4"\n'
    manifest += "[profile.dev]\ndebug=0\nincremental=false\n"
    (args.output / "Cargo.toml").write_text(manifest)


if __name__ == "__main__":
    main()
