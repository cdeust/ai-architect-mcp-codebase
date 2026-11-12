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
// block, and the canonical (byte-identical) serialisation. The second slice,
// `communities`, builds the community pages from a resolved + clustered
// `GraphStore`; the third, `processes`, builds one page per detected process
// from the same store; `coverage` attributes the `parse_incomplete` files of
// the index's coverage record to the pages whose members they hold; `missed`
// attributes the call sites the resolver left open to the pages that list
// the symbol whose body holds them; `god_nodes` builds one page per symbol
// whose degree is beyond Tukey's far-out fence of the degree distribution.
// The MCP tool that exports the skeleton is a later slice of #196.

mod checks;
pub mod communities;
pub mod coverage;
mod error;
pub mod god_nodes;
pub mod missed;
pub mod processes;
pub mod skeleton;

pub use communities::{community_pages, CommunityPages};
pub use coverage::{attribute_parse_incomplete, CoverageAttribution};
pub use error::SkeletonError;
pub use god_nodes::{god_node_pages, Fence, GodNodePages};
pub use missed::{attribute_missed, MissedAttribution};
pub use processes::{process_pages, ProcessPages};
pub use skeleton::{
    CoverageGap, Edge, Location, Page, PageKind, Provenance, Symbol, WikiSkeleton, SCHEMA_VERSION,
};
