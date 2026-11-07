// wiki::skeleton — the versioned wiki skeleton artifact (issue #196).
//
// Every acceptance criterion this slice owns is enforced by construction, not
// by a warning:
//
// * criterion 3 — an `Edge` cannot be built without a `Provenance`, and the
//   enum has exactly the two values `extracted` / `inferred`. Deserialising
//   an edge with a missing or unknown tag is an error, so an untagged edge is
//   unrepresentable on both sides of the wire.
// * edges, members, missed-graph names and cross-community keys refuse empty
//   values; two members that share an id but differ elsewhere are refused
//   (exact repeats are merged). An edge's ends are NOT
//   required to be members of its page: a page lists the edges that leave it
//   too, and their targets live on other pages.
// * criterion 4 — `Symbol`, `Edge` and a missed-graph gap each hold a
//   `Location`, whose `line` is a `NonZeroU32` and whose `file` is a
//   non-empty repo-relative path (no absolute path, drive or UNC prefix, `..`
//   component, backslash or control character). `ParseIncomplete.file`
//   follows the same path rule.
// * criterion 5 — `Page::coverage` is serialised unconditionally: an empty
//   block is `"coverage": []`, present, never absent.
// * criterion 6 — `WikiSkeleton::new` sorts and deduplicates every list and
//   derives `source_files` from the members (a read `source_files` that
//   disagrees is refused, not rewritten), so the same input in any order
//   produces byte-identical JSON. Maps are `BTreeMap`s.
// * criterion 7 — `SCHEMA_VERSION` is embedded in the artifact; every
//   deserialisation path refuses any other version, and `from_json` reports
//   it as `SchemaVersion` whatever the document's shape.

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
    UnsafePath(String),
    StaleSourceFiles {
        page: String,
    },
    EmptyId(&'static str),
    DuplicatePage {
        kind: PageKind,
        id: String,
    },
    DuplicateSymbol {
        page: String,
        id: String,
    },
    DuplicateEdge {
        page: String,
        source: String,
        target: String,
        relation: String,
    },
    SelfCrossCommunity {
        page: String,
    },
    SchemaVersion {
        found: String,
    },
    Json(String),
}

impl fmt::Display for SkeletonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFile => write!(f, "a file path must not be empty"),
            Self::ZeroLine => write!(f, "a location line is 1-based; 0 is not a line"),
            Self::UnsafePath(path) => write!(f, "{path:?} is not a repo-relative path"),
            Self::StaleSourceFiles { page } => {
                write!(
                    f,
                    "page {page:?} lists source_files that are not its members' files"
                )
            }
            Self::EmptyId(what) => write!(f, "{what} must not be empty"),
            Self::DuplicatePage { kind, id } => write!(f, "duplicate {kind:?} page {id:?}"),
            Self::DuplicateSymbol { page, id } => {
                write!(
                    f,
                    "page {page:?} lists member {id:?} twice with different fields"
                )
            }
            Self::DuplicateEdge {
                page,
                source,
                target,
                relation,
            } => write!(
                f,
                "page {page:?} tags edge {source:?} -{relation:?}-> {target:?} twice"
            ),
            Self::SelfCrossCommunity { page } => {
                write!(f, "page {page:?} counts cross-community edges to itself")
            }
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

/// Refuse anything a later slice could turn into a path outside the repo
/// (CWE-22): an empty path, an absolute or drive/UNC-prefixed one, a `..`
/// component, a backslash or a control character. Callers that open a file
/// must still join it to the repo root and check the canonical prefix.
fn repo_relative(file: &str) -> Result<(), SkeletonError> {
    if file.is_empty() {
        return Err(SkeletonError::EmptyFile);
    }
    let drive = file.as_bytes().get(1) == Some(&b':');
    let unsafe_path = file.starts_with('/')
        || drive
        || file.contains('\\')
        || file.chars().any(char::is_control)
        || file.split('/').any(|part| part == "..");
    if unsafe_path {
        return Err(SkeletonError::UnsafePath(file.to_owned()));
    }
    Ok(())
}

impl Location {
    pub fn new(file: impl Into<String>, line: u32) -> Result<Self, SkeletonError> {
        let file = file.into();
        repo_relative(&file)?;
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

/// An edge between two symbols. Its fields are private: an `Edge` is built
/// only by `Edge::new` or by deserialisation, and both require a provenance
/// (`provenance` has no `#[serde(default)]`).
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

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn relation(&self) -> &str {
        &self.relation
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

/// One page stub. `source_files` is private and derived from `members` by
/// `WikiSkeleton::new`, which overwrites it; on read it is required and a
/// value that disagrees with the members' files is refused, so inside a
/// `WikiSkeleton` the two always agree.
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
    /// Required on read and compared there (an explicit `[]` included).
    source_files: Vec<String>,
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

    pub fn source_files(&self) -> &[String] {
        &self.source_files
    }

    fn validate(&self) -> Result<(), SkeletonError> {
        non_empty(&self.id, "page")?;
        for symbol in &self.members {
            non_empty(&symbol.id, "member symbol")?;
        }
        for edge in &self.edges {
            non_empty(&edge.source, "edge source")?;
            non_empty(&edge.target, "edge target")?;
            non_empty(&edge.relation, "edge relation")?;
        }
        for key in self.cross_community_edges.keys() {
            non_empty(key, "cross-community key")?;
            if *key == self.id {
                return Err(SkeletonError::SelfCrossCommunity {
                    page: self.id.clone(),
                });
            }
        }
        for gap in &self.coverage {
            match gap {
                CoverageGap::Missed { name, .. } => non_empty(name, "missed-graph entry")?,
                CoverageGap::ParseIncomplete { file } => repo_relative(file)?,
            }
        }
        Ok(())
    }

    /// After `normalize`, members are sorted by id first, so two members that
    /// share an id but differ elsewhere are adjacent.
    fn check_unique_members(&self) -> Result<(), SkeletonError> {
        match self.members.windows(2).find(|w| w[0].id == w[1].id) {
            Some(pair) => Err(SkeletonError::DuplicateSymbol {
                page: self.id.clone(),
                id: pair[0].id.clone(),
            }),
            None => Ok(()),
        }
    }

    /// One edge, one provenance: two edges with the same ends, relation and
    /// location but different tags are refused. Edges that differ only by
    /// location are two call sites and stay.
    fn check_unique_edges(&self) -> Result<(), SkeletonError> {
        let mut seen = BTreeSet::new();
        for edge in &self.edges {
            let key = (&edge.source, &edge.target, &edge.relation, &edge.location);
            if !seen.insert(key) {
                return Err(SkeletonError::DuplicateEdge {
                    page: self.id.clone(),
                    source: edge.source.clone(),
                    target: edge.target.clone(),
                    relation: edge.relation.clone(),
                });
            }
        }
        Ok(())
    }

    /// Put every list in its canonical order and derive `source_files`,
    /// overwriting any stored value, so pages taken from a skeleton can be
    /// edited and passed to `new` again. The read path compares instead.
    fn normalize(&mut self) {
        sort_dedup(&mut self.members);
        sort_dedup(&mut self.edges);
        sort_dedup(&mut self.coverage);
        let files: BTreeSet<&str> = self.members.iter().map(|s| s.location.file()).collect();
        self.source_files = files.into_iter().map(str::to_owned).collect();
    }
}

fn non_empty(value: &str, what: &'static str) -> Result<(), SkeletonError> {
    if value.is_empty() {
        return Err(SkeletonError::EmptyId(what));
    }
    Ok(())
}

fn sort_dedup<T: Ord>(items: &mut Vec<T>) {
    items.sort_unstable();
    items.dedup();
}

/// The whole artifact. Every deserialisation path goes through
/// `RawSkeleton` and `TryFrom`, so serde cannot build a skeleton that skips
/// the version check, the validation or the canonical ordering of `new`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawSkeleton")]
pub struct WikiSkeleton {
    schema_version: String,
    pages: Vec<Page>,
}

/// Strict v1 shape: the `Deserialize` of `WikiSkeleton` goes through it, so
/// plain serde never accepts another version, an unknown field or an
/// unvalidated page. A document of another version whose shape also differs
/// may be refused for its shape first; `from_json` probes the version before
/// parsing, so it reports `SchemaVersion` whatever the shape.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSkeleton {
    schema_version: String,
    pages: Vec<Page>,
}

/// Reads only `schema_version`; every other key is skipped without being
/// buffered (`IgnoredAny`).
#[derive(Deserialize)]
struct VersionProbe {
    schema_version: String,
}

fn check_version(found: String) -> Result<(), SkeletonError> {
    if found != SCHEMA_VERSION {
        return Err(SkeletonError::SchemaVersion { found });
    }
    Ok(())
}

impl TryFrom<RawSkeleton> for WikiSkeleton {
    type Error = SkeletonError;
    fn try_from(raw: RawSkeleton) -> Result<Self, Self::Error> {
        check_version(raw.schema_version)?;
        let read: BTreeMap<(PageKind, String), Vec<String>> = raw
            .pages
            .iter()
            .map(|p| ((p.kind, p.id.clone()), p.source_files.clone()))
            .collect();
        let skeleton = Self::new(raw.pages)?;
        for page in &skeleton.pages {
            if read.get(&(page.kind, page.id.clone())) != Some(&page.source_files) {
                return Err(SkeletonError::StaleSourceFiles {
                    page: page.id.clone(),
                });
            }
        }
        Ok(skeleton)
    }
}

impl WikiSkeleton {
    /// Validate and canonicalise. Pages are ordered by `(kind, id)`.
    pub fn new(mut pages: Vec<Page>) -> Result<Self, SkeletonError> {
        for page in &mut pages {
            page.validate()?;
            page.normalize();
            page.check_unique_members()?;
            page.check_unique_edges()?;
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
    ///
    /// Typed on this path: `SchemaVersion` and every error `new` returns
    /// (`EmptyId`, `DuplicatePage`, `DuplicateSymbol`, `DuplicateEdge`,
    /// `SelfCrossCommunity`,
    /// `StaleSourceFiles`, and `EmptyFile`/`UnsafePath` for a
    /// `ParseIncomplete` file). A `Location` is validated while serde parses
    /// it, so its errors (line 0, empty or unsafe path) arrive as `Json` with
    /// the same message. `serde_json::from_str::<WikiSkeleton>` applies the
    /// same checks but reports every error as a serde error.
    pub fn from_json(text: &str) -> Result<Self, SkeletonError> {
        let json = |e: serde_json::Error| SkeletonError::Json(e.to_string());
        let probe: VersionProbe = serde_json::from_str(text).map_err(json)?;
        check_version(probe.schema_version)?;
        let raw: RawSkeleton = serde_json::from_str(text).map_err(json)?;
        Self::try_from(raw)
    }
}

#[cfg(test)]
#[path = "skeleton_tests.rs"]
mod tests;
