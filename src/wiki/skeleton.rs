// wiki::skeleton — the versioned wiki skeleton artifact (issue #196).
//
// Every acceptance criterion this slice owns is enforced by construction, not
// by a warning:
//
// * criterion 3 — an `Edge` cannot be built without a `Provenance`, and the
//   enum has exactly the two values `extracted` / `inferred`. Deserialising
//   an edge with a missing or unknown tag is an error, so an untagged edge is
//   unrepresentable on both sides of the wire.
// * criterion 4 — `Symbol`, `Edge` and a missed-graph gap each hold a
//   `Location`, whose `line` is a `NonZeroU32` and whose `file` is non-empty.
// * criterion 5 — `Page::coverage` is serialised unconditionally: an empty
//   block is `"coverage": []`, present, never absent.
// * criterion 6 — `WikiSkeleton::new` sorts and deduplicates every list and
//   derives `source_files` from the members, so the same input in any order
//   produces byte-identical JSON. Maps are `BTreeMap`s.
// * criterion 7 — `SCHEMA_VERSION` is embedded in the artifact, and
//   `from_json` refuses any other version.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroU32;

/// Embedded in every artifact. Bump on any change to the serialised shape.
pub const SCHEMA_VERSION: &str = "ai-architect.wiki-skeleton/1";

/// Why a skeleton could not be built or read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkeletonError {
    EmptyFile,
    ZeroLine,
    EmptyId(&'static str),
    DuplicatePage { kind: PageKind, id: String },
    SchemaVersion { found: String },
    Json(String),
}

impl fmt::Display for SkeletonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFile => write!(f, "a location needs a non-empty file path"),
            Self::ZeroLine => write!(f, "a location line is 1-based; 0 is not a line"),
            Self::EmptyId(what) => write!(f, "{what} needs a non-empty id"),
            Self::DuplicatePage { kind, id } => write!(f, "duplicate {kind:?} page {id:?}"),
            Self::SchemaVersion { found } => {
                write!(f, "schema version {found:?} is not {SCHEMA_VERSION:?}")
            }
            Self::Json(message) => write!(f, "skeleton JSON: {message}"),
        }
    }
}

impl std::error::Error for SkeletonError {}

/// How an edge came to be in the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Read directly from the AST (structural edges: Defines, HasMethod, ...).
    Extracted,
    /// Produced by a resolver (imports, calls, LSP, heuristics).
    Inferred,
}

/// A `file:line` anchor. `line` is 1-based and can never be 0.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawLocation")]
pub struct Location {
    file: String,
    line: NonZeroU32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLocation {
    file: String,
    line: u32,
}

impl TryFrom<RawLocation> for Location {
    type Error = SkeletonError;
    fn try_from(raw: RawLocation) -> Result<Self, Self::Error> {
        Location::new(raw.file, raw.line)
    }
}

impl Location {
    pub fn new(file: impl Into<String>, line: u32) -> Result<Self, SkeletonError> {
        let file = file.into();
        if file.is_empty() {
            return Err(SkeletonError::EmptyFile);
        }
        let line = NonZeroU32::new(line).ok_or(SkeletonError::ZeroLine)?;
        Ok(Self { file, line })
    }

    pub fn file(&self) -> &str {
        &self.file
    }

    pub fn line(&self) -> u32 {
        self.line.get()
    }
}

/// A member symbol of a page.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Symbol {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub location: Location,
}

/// An edge between two symbols. Fields are private so `Edge::new` is the only
/// way in, and it takes the provenance as a required argument.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    source: String,
    target: String,
    relation: String,
    provenance: Provenance,
    location: Location,
}

impl Edge {
    /// `ends` is `(source id, target id)`.
    pub fn new(
        ends: (impl Into<String>, impl Into<String>),
        relation: impl Into<String>,
        provenance: Provenance,
        location: Location,
    ) -> Self {
        Self {
            source: ends.0.into(),
            target: ends.1.into(),
            relation: relation.into(),
            provenance,
            location,
        }
    }

    pub fn provenance(&self) -> Provenance {
        self.provenance
    }

    pub fn location(&self) -> &Location {
        &self.location
    }
}

/// Something the parser or resolver could not read, attributed to a page so
/// the gap is visible on the page instead of absent from it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoverageGap {
    /// A missed-graph entry: a reference that was seen but not resolved.
    Missed { name: String, location: Location },
    /// A file whose parse was incomplete (`parse_incomplete`).
    ParseIncomplete { file: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    Community,
    Process,
    GodNode,
}

/// One page stub. `source_files` is derived from `members` by
/// `WikiSkeleton::new`, so the two can never disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub kind: PageKind,
    pub id: String,
    pub title: String,
    pub members: Vec<Symbol>,
    pub edges: Vec<Edge>,
    /// Edge count from this page to each other community, keyed by its id.
    pub cross_community_edges: BTreeMap<String, u64>,
    #[serde(default)]
    pub source_files: Vec<String>,
    /// Always serialised: empty means "no known gap", never "not computed".
    pub coverage: Vec<CoverageGap>,
}

impl Page {
    pub fn new(kind: PageKind, id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            title: title.into(),
            members: Vec::new(),
            edges: Vec::new(),
            cross_community_edges: BTreeMap::new(),
            source_files: Vec::new(),
            coverage: Vec::new(),
        }
    }

    fn validate(&self) -> Result<(), SkeletonError> {
        if self.id.is_empty() {
            return Err(SkeletonError::EmptyId("page"));
        }
        if self.members.iter().any(|s| s.id.is_empty()) {
            return Err(SkeletonError::EmptyId("member symbol"));
        }
        let empty_gap_file = self
            .coverage
            .iter()
            .any(|g| matches!(g, CoverageGap::ParseIncomplete { file } if file.is_empty()));
        if empty_gap_file {
            return Err(SkeletonError::EmptyFile);
        }
        Ok(())
    }

    /// Put every list in its canonical order and derive `source_files`.
    fn normalize(&mut self) {
        sort_dedup(&mut self.members);
        sort_dedup(&mut self.edges);
        sort_dedup(&mut self.coverage);
        let files: BTreeSet<&str> = self.members.iter().map(|s| s.location.file()).collect();
        self.source_files = files.into_iter().map(str::to_owned).collect();
    }
}

fn sort_dedup<T: Ord>(items: &mut Vec<T>) {
    items.sort();
    items.dedup();
}

/// The whole artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiSkeleton {
    schema_version: String,
    pages: Vec<Page>,
}

impl WikiSkeleton {
    /// Validate and canonicalise. Pages are ordered by `(kind, id)`.
    pub fn new(mut pages: Vec<Page>) -> Result<Self, SkeletonError> {
        for page in &mut pages {
            page.validate()?;
            page.normalize();
        }
        pages.sort_by(|a, b| (a.kind, &a.id).cmp(&(b.kind, &b.id)));
        if let Some(pair) = pages
            .windows(2)
            .find(|w| (w[0].kind, &w[0].id) == (w[1].kind, &w[1].id))
        {
            let (kind, id) = (pair[0].kind, pair[0].id.clone());
            return Err(SkeletonError::DuplicatePage { kind, id });
        }
        Ok(Self {
            schema_version: SCHEMA_VERSION.to_owned(),
            pages,
        })
    }

    pub fn schema_version(&self) -> &str {
        &self.schema_version
    }

    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    /// Canonical JSON: same skeleton, same bytes.
    pub fn to_json(&self) -> Result<String, SkeletonError> {
        let mut text =
            serde_json::to_string_pretty(self).map_err(|e| SkeletonError::Json(e.to_string()))?;
        text.push('\n');
        Ok(text)
    }

    /// Read an artifact back. Refuses another schema version, then re-runs
    /// the same validation and canonicalisation as `new`.
    pub fn from_json(text: &str) -> Result<Self, SkeletonError> {
        let raw: Self =
            serde_json::from_str(text).map_err(|e| SkeletonError::Json(e.to_string()))?;
        if raw.schema_version != SCHEMA_VERSION {
            return Err(SkeletonError::SchemaVersion {
                found: raw.schema_version,
            });
        }
        Self::new(raw.pages)
    }
}

#[cfg(test)]
#[path = "skeleton_tests.rs"]
mod tests;
