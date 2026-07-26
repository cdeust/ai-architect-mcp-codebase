// parser::spec — table-driven, spec-table extraction (ADR-0055).
//
// A `LangSpec` (pure node-kind data) + a `LanguageConventions` impl (behavior)
// are consumed by generic walkers (`walk_defs`/`walk_calls`/`walk_imports`/
// `walk_embedded`) to produce the EXISTING `ParseResult` contract. Adding a
// language becomes a data row + (at most) a small conventions override, not a
// bespoke ~670-LOC walker (ADR-0055 Context/Decision).
//
// Strangler-fig rollout: `registry::lang_spec` returns `Some` only for
// migrated languages; `parser::parse_file` routes those through
// `parse_with_spec` and leaves the rest on their hand-written walkers until
// each is migrated at parity behind the accuracy gate (ADR-0055 §5).

mod conventions;
mod go;
mod lang_spec;
mod registry;
mod walkers;

#[cfg(test)]
mod embedded_tests;
#[cfg(test)]
mod guard;
#[cfg(test)]
mod parity_tests;

pub(crate) use go::GO_SPEC;
pub(crate) use walkers::parse_with_spec;
