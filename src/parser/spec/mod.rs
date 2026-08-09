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

mod c;
mod c_family;
mod conventions;
mod cpp;
mod declarative;
mod declarative_rules;
mod families;
mod go;
mod java;
mod kotlin;
mod kotlin_conventions;
mod lang_spec;
mod objc;
mod python;
mod registry;
mod ruby;
mod rust;
mod shallow;
mod swift;
mod typescript;
mod walkers;

// The generic grammar-introspection extraction engine (issue #224 + its
// follow-up: declarator-chain descent, a heritage-on-child-node hop, and a
// fieldless-grammar kind-substring fallback classifier), its TIER 2
// extension module, and their shared coverage-proof tests. Not wired into
// any language's production dispatch in this PR (see `structural`'s module
// doc), so all three are `#[cfg(test)]`-gated like the other comparison-only
// modules above — nothing outside `structural_coverage_tests` calls
// `structural`/`structural_fallback` yet. `cargo clippy --all-targets`
// compiles `lib` separately from `unittests`, so leaving any of them
// reachable only from test code but not test-gated would warn there and
// nowhere else.

#[cfg(test)]
mod c_extraction_tests;
#[cfg(test)]
mod c_ground_truth;
#[cfg(test)]
mod c_parity_tests;
#[cfg(test)]
mod cpp_extraction_tests;
#[cfg(test)]
mod cpp_ground_truth;
#[cfg(test)]
mod cpp_member_classification_tests;
#[cfg(test)]
mod cpp_parity_tests;
#[cfg(test)]
mod declarative_java_tests;
#[cfg(test)]
mod declarative_tests;
#[cfg(test)]
mod embedded_tests;
#[cfg(test)]
mod go_parity_tests;
#[cfg(test)]
mod guard;
#[cfg(test)]
mod guard_grammar;
#[cfg(test)]
mod java_declarative_parity_tests;
#[cfg(test)]
mod java_parity_tests;
#[cfg(test)]
mod kotlin_ground_truth;
#[cfg(test)]
mod kotlin_parity_tests;
#[cfg(test)]
mod objc_extraction_tests;
#[cfg(test)]
mod objc_parity_tests;
#[cfg(test)]
mod parity_tests;
#[cfg(test)]
mod python_parity_tests;
#[cfg(test)]
mod rust_parity_corpus;
#[cfg(test)]
mod rust_parity_tests;
#[cfg(test)]
mod rust_walker_tests;
#[cfg(test)]
mod shallow_tests;
#[cfg(test)]
mod swift_ground_truth;
#[cfg(test)]
mod swift_parity_tests;
#[cfg(test)]
mod typescript_parity_tests;

pub(crate) use c::C_SPEC;
pub(crate) use cpp::CPP_SPEC;
pub(crate) use go::GO_SPEC;
pub(crate) use java::JAVA_SPEC;
pub(crate) use kotlin::KOTLIN_SPEC;
pub(crate) use objc::OBJC_SPEC;
pub(crate) use python::PYTHON_SPEC;
pub(crate) use ruby::RUBY_SPEC;
pub(crate) use rust::RUST_SPEC;
pub(crate) use shallow::parse_shallow;
pub(crate) use swift::SWIFT_SPEC;
pub(crate) use typescript::TS_SPEC;
pub(crate) use walkers::parse_with_spec;
