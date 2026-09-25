// resolver::calls::variant_guard: `Kind::A(1)` names the variant `A` of the enum
// `Kind`, which the symbol index does not hold (issue #356).
//
// Without this, the by-name lookup of `A` finds a lone struct `A` elsewhere in
// the repository and resolves the call to it: a wrong `Uses_*_Struct` edge, and
// now a wrong per-site row. The qualifier decides: `Self`, or a name every
// symbol of which is an enum or a type alias, cannot construct a struct.

use super::*;
use std::borrow::Cow;

/// Whether the segment before the last `::` can only name an enum: `Self` (a
/// path through `Self` never names a struct, and `Self` is not in the index), or
/// a name every entry of which is an Enum or a TypeAlias (`type K = Kind;
/// K::A(1)`). A module, struct or trait of the same name keeps the lookup open,
/// because then the qualifier may be that item.
fn qualifier_names_only_enums(ctx: &ResolveContext, callee: &str) -> bool {
    let mut segments = callee.rsplit("::");
    let (Some(_last), Some(qualifier)) = (segments.next(), segments.next()) else {
        return false;
    };
    qualifier == "Self"
        || ctx.idx.by_name.get(qualifier).is_some_and(|entries| {
            !entries.is_empty()
                && entries
                    .iter()
                    .all(|e| e.label == "Enum" || e.label == "TypeAlias")
        })
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
