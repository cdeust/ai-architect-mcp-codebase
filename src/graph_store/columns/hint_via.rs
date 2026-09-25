// graph_store::hint_via: the `CallSite.receiver_hint_via` values of a hint the
// parser read off a receiver that spells its own type (issue #355).

/// The value of a hint read off a receiver that is a tuple-struct constructor
/// or a struct literal, written in place or bound by a single `let`. The
/// resolver keeps only candidates defined in the caller's own file for such a
/// hint: the hint is the last path segment, and a namesake type elsewhere would
/// otherwise be a candidate.
/// source: parser::spec::rust_constructed_receiver writes it,
/// resolver::calls::gates reads it.
pub(crate) const RECEIVER_HINT_VIA_CONSTRUCTED: &str = "constructed";

/// The value of a hint read off `Type::assoc(..)` written in place, whose
/// declared return type is `Self` or the type itself: the weaker return-type
/// tier, with the same same-file restriction.
pub(crate) const RECEIVER_HINT_VIA_CONSTRUCTED_RETURN_TYPE: &str = "constructed-return-type";
