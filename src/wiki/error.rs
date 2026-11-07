// wiki::error — why a wiki skeleton could not be built or read (issue #196).
// Split out of skeleton.rs to keep that file under the §4.1 500-line cap.

use super::skeleton::{PageKind, SCHEMA_VERSION};
use std::fmt;

/// Why a skeleton could not be built or read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkeletonError {
    EmptyFile,
    ZeroLine,
    UnsafePath(String),
    StaleSourceFiles {
        kind: PageKind,
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
            Self::StaleSourceFiles { kind, page } => {
                write!(
                    f,
                    "{kind:?} page {page:?} has source_files that do not match its members' files"
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
                write!(
                    f,
                    "community page {page:?} counts cross-community edges to itself"
                )
            }
            Self::SchemaVersion { found } => {
                write!(f, "schema version {found:?} is not {SCHEMA_VERSION:?}")
            }
            Self::Json(message) => write!(f, "skeleton JSON: {message}"),
        }
    }
}

impl std::error::Error for SkeletonError {}
