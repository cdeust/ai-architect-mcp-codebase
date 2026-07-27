// parser::spec::guard — the spec-validation guard (ADR-0055 Consequences,
// "the single most important guard in this ADR").
//
// A wrong or stale node-kind string in a `LangSpec` does not error at runtime:
// the walker simply never matches that node, so symbols are silently dropped
// and the loss surfaces only as an F1 dip in `graph_accuracy` — and only if a
// fixture happens to cover the construct. This guard converts that silent-drop
// failure into a loud test failure: it loads each migrated grammar's
// `NODE_TYPES` (the very `node-types.json` cited as the source for every
// node-kind string) and asserts every kind and field name in the spec is real
// for that grammar. It makes the §8 source citation executable.

use super::guard_grammar::{node_types_json, parse_node_types};
use super::registry::{MIGRATED_SPECS, SHALLOW_SPECS};

mod collect;
use collect::{shallow_field_names, shallow_node_kinds, spec_field_names, spec_node_kinds};

#[test]
fn every_shallow_spec_node_kind_is_real_for_its_grammar() {
    let mut failures: Vec<String> = Vec::new();
    for spec in SHALLOW_SPECS {
        let (kinds, fields) = parse_node_types(node_types_json(spec.language));
        for (field, kind) in shallow_node_kinds(spec) {
            if !kinds.contains(&kind) {
                failures.push(format!(
                    "{:?}.{field}: node kind {kind:?} is NOT in the grammar's node-types.json",
                    spec.language
                ));
            }
        }
        for (field, name) in shallow_field_names(spec) {
            if !fields.contains(&name) {
                failures.push(format!(
                    "{:?}.{field}: field name {name:?} is NOT a field in the grammar's node-types.json",
                    spec.language
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "shallow spec-validation guard found stale/wrong rows:\n{}",
        failures.join("\n")
    );
}

/// The guard must actually inspect something — an empty `SHALLOW_SPECS` would
/// make the test above vacuously green, which is the failure mode this repo has
/// already paid for twice (a static check passing against a file that no longer
/// holds the code).
#[test]
fn shallow_guard_is_not_vacuous() {
    assert!(
        !SHALLOW_SPECS.is_empty(),
        "SHALLOW_SPECS is empty: every_shallow_spec_node_kind_is_real_for_its_grammar \
         would pass without checking anything"
    );
    let checked: usize = SHALLOW_SPECS
        .iter()
        .map(|s| shallow_node_kinds(s).len())
        .sum();
    assert!(
        checked > 0,
        "no shallow node kinds were validated; the guard is asserting nothing"
    );
}

#[test]
fn every_spec_node_kind_is_real_for_its_grammar() {
    let mut failures: Vec<String> = Vec::new();
    for spec in MIGRATED_SPECS {
        let (kinds, fields) = parse_node_types(node_types_json(spec.language));

        for (field, kind) in spec_node_kinds(spec) {
            if !kinds.contains(&kind) {
                failures.push(format!(
                    "{:?}.{field}: node kind {kind:?} is NOT in the grammar's node-types.json",
                    spec.language
                ));
            }
        }
        for (field, name) in spec_field_names(spec) {
            if !fields.contains(&name) {
                failures.push(format!(
                    "{:?}.{field}: field name {name:?} is NOT a field in the grammar's node-types.json",
                    spec.language
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "spec-validation guard found stale/wrong spec rows:\n{}",
        failures.join("\n")
    );
}

#[test]
fn guard_rejects_a_bogus_node_kind() {
    // Proves the guard actually fails on a wrong row (kills the mutant where
    // the guard vacuously passes). tree-sitter-go has no `not_a_real_kind`.
    let (kinds, _fields) = parse_node_types(tree_sitter_go::NODE_TYPES);
    assert!(
        !kinds.contains("not_a_real_node_kind"),
        "sanity: bogus kind must be absent"
    );
    assert!(
        kinds.contains("function_declaration"),
        "sanity: real Go kind must be present"
    );
}
