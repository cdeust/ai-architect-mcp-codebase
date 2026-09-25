// resolver::calls::variant_guard: `Kind::A(1)` names the variant `A` of the enum
// `Kind`, which the symbol index does not hold (issue #356).
//
// Without this, the by-name lookup of `A` finds a lone struct `A` elsewhere in
// the repository and resolves the call to it: a wrong `Uses_*_Struct` edge, and
// now a wrong per-site row. The qualifier decides: when every symbol called
// `Kind` is an enum, the call cannot construct a struct.

use super::*;
use std::borrow::Cow;

/// Whether the segment before the last `::` names only enums in the index.
/// A module, struct or trait of the same name keeps the lookup open, because
/// then the qualifier may be that item.
fn qualifier_names_only_enums(ctx: &ResolveContext, callee: &str) -> bool {
    let mut segments = callee.rsplit("::");
    let (Some(_last), Some(qualifier)) = (segments.next(), segments.next()) else {
        return false;
    };
    ctx.idx
        .by_name
        .get(qualifier)
        .is_some_and(|entries| !entries.is_empty() && entries.iter().all(|e| e.label == "Enum"))
}

/// `candidates` without the structs, when the callee's qualifier is an enum.
/// Methods and functions stay: `Kind::new(..)` is an associated function.
pub(super) fn drop_struct_targets<'a>(
    ctx: &ResolveContext,
    callee: &str,
    candidates: Cow<'a, [SymbolEntry]>,
) -> Cow<'a, [SymbolEntry]> {
    if !qualifier_names_only_enums(ctx, callee) {
        return candidates;
    }
    Cow::Owned(
        candidates
            .iter()
            .filter(|c| c.label != "Struct")
            .cloned()
            .collect(),
    )
}
