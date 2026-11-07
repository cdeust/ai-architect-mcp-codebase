// wiki — deterministic wiki skeleton export (issue #196).
//
// The deterministic half of the codebase-wiki split: structure computed from
// the resolved + clustered graph, with no LLM and no network at index time.
// The prose half lives downstream (cdeust/Cortex#358).
//
// This first slice is the artifact itself: the versioned schema, the
// type-level provenance tag on every edge, the mandatory `file:line` on every
// member, edge and missed-graph gap (a `parse_incomplete` gap is file-level
// and a cross-community count has no line), the always-present coverage
// block, and the canonical (byte-identical) serialisation. Building pages
// from a `GraphStore` and the MCP tool that exports them are later slices of
// #196.

pub mod error;
pub mod skeleton;

pub use skeleton::{
    CoverageGap, Edge, Location, Page, PageKind, Provenance, SkeletonError, Symbol, WikiSkeleton,
    SCHEMA_VERSION,
};
