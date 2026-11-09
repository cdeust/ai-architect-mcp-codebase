// epistemic — boundary-honesty primitives shared by impact analysis (get_impact),
// symbol context (get_context), and diff risk (detect_changes).
//
// Every static graph traversal is a LOWER BOUND on true impact whenever the
// target is reachable through dynamic dispatch (an interface/trait method
// resolved at runtime) or via a heuristically-resolved (confidence < 1.0)
// edge. A statically-counted dependent set is exhaustive ONLY when the target
// is a concrete symbol reached exclusively through fully-confident edges.
//
// These helpers let read-side tools report that boundary honestly — emitting
// `epistemic: "lower-bound"` with the carriers of uncertainty — rather than
// presenting a static count as if it were the whole truth. This is the zetetic
// identity of the tool made operational: say what you do not know.

/// Node labels that represent a dynamic-dispatch surface. A call through such a
/// type (`dyn Trait`, a generic bound, an interface reference) binds to an
/// implementor at runtime; static resolution cannot attribute every such call
/// site to the specific target, so the captured caller set is a lower bound.
///
/// Of the labels in `graph_store::NODE_LABELS`, `Trait` is the only
/// dynamic-dispatch surface for the three live languages (Rust trait objects /
/// generic bounds; Python and TypeScript structural-typing call sites are
/// likewise not exhaustively static). The remaining symbol labels (Function,
/// Method, Struct, Enum, Constant, TypeAlias, Module) are concrete.
// source: graph_store::NODE_TRAIT; clustering::SYMBOL_LABELS.
pub const DYNAMIC_DISPATCH_LABELS: &[&str] = &["Trait"];

/// True when a node of `label` is a dynamic-dispatch surface (see above).
pub fn is_dynamic_dispatch_surface(label: &str) -> bool {
    DYNAMIC_DISPATCH_LABELS.contains(&label)
}

/// The epistemic status of a statically-derived dependent/relationship set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// The static set is exhaustive: concrete target, all edges fully confident.
    Exact,
    /// The static set under-counts true impact: target is reachable through
    /// dynamic dispatch, or one or more contributing edges were resolved
    /// heuristically (confidence < 1.0). For `detect_changes`, also: some of
    /// the diff's changes reach no indexed symbol (issue #446).
    LowerBound,
}

impl Boundary {
    /// Wire form emitted on tool responses.
    pub fn as_str(self) -> &'static str {
        match self {
            Boundary::Exact => "exact",
            Boundary::LowerBound => "lower-bound",
        }
    }
}

/// Per-relation-type confidence floor: the inherent reliability of a
/// statically-resolved edge of each kind, used as the confidence value when an
/// edge carries no stored `confidence` property (structural edges, or graphs
/// built before confidence was persisted) so downstream risk math never silently
/// assumes 1.0 for a heuristic relation.
///
/// `rel_or_prefix` may be a full REL_TABLES name (`"Calls_Method_Function"`) or a
/// traversal prefix (`"Calls_"`); matching is by prefix.
// source: GitNexus ScopeResolver measured per-relation resolution-confidence —
// CALLS 0.90, EXTENDS/IMPLEMENTS 0.85, ACCESSES 0.80. AP's `Uses_` edge is the
// field/type-access relation (== GitNexus ACCESSES, 0.80); AP's `Imports_` edge
// is measured at 0.90 by the resolver's import-scope-lookup path
// (resolver.rs:374 `buf.add(..., 0.9, "import-scope-lookup")`). Structural edges
// (Contains/Defines/HasMethod/...) are exact by construction → 1.0.
pub fn relation_confidence_floor(rel_or_prefix: &str) -> f64 {
    if rel_or_prefix.starts_with("Calls_") {
        0.90
    } else if rel_or_prefix.starts_with("Implements_") || rel_or_prefix.starts_with("Extends_") {
        0.85
    } else if rel_or_prefix.starts_with("Uses_") {
        0.80
    } else if rel_or_prefix.starts_with("Imports_") {
        0.90
    } else {
        1.0
    }
}

/// Confidence below which an edge is considered heuristic (a carrier of
/// epistemic uncertainty). Stored resolution confidences are < 1.0 for every
/// heuristic method (0.95 declared-implements, 0.90 derive-macro / unqualified
/// call / import-scope / type-annotation); a fully-qualified static call stores
/// exactly 1.0 (resolver.rs:417). The threshold is therefore "strictly less
/// than fully-confident".
// source: resolver.rs confidence values — qualified call = 1.0, all heuristic
// methods < 1.0.
pub const CONFIDENT_EDGE_FLOOR: f64 = 1.0;

/// True when an edge confidence marks the edge as heuristic (under-counts /
/// may be wrong), i.e. strictly below a fully-confident static resolution.
pub fn is_heuristic_edge(confidence: f64) -> bool {
    confidence < CONFIDENT_EDGE_FLOOR
}
