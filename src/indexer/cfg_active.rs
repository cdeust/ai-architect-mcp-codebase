// indexer::cfg_active: says, for every `#[cfg]` twin of a graph, whether the
// default build compiles it (issue #353, part B).
//
// A twin is a node whose id carries a `#cfg(<gate>)` suffix. Its gate (the
// conjunction of the gates in its id) is evaluated under the features
// `cargo metadata` reports for the package that compiles its file, and the
// verdict is stored in the `cfg_active` column: `active` when the predicate is
// true, `inactive` when it is false, `unknown` when it is not decided (a bare
// option the build invocation decides such as `unix` or `test`, a gate that did
// not parse, a file two
// packages compile with different features, a file no crate root reaches, or no
// Cargo map at all). A file reached only through `mod` declarations the default
// features compile out is `inactive`.
//
// The pass rewrites every twin on every index pass, full or incremental: the
// features can change without a file changing (a `Cargo.toml` edit), and a twin
// reparsed by an incremental pass comes back with an empty column.
//
// The default profile fixes the options no plain `cargo build` sets (`kani`,
// `miri`, `doc`, `doctest`) to false (issue #391): the `cfg(not(kani))` twin is
// `active` and the `cfg(kani)` one `inactive`. Without a Cargo map nothing says a
// Cargo build compiles the file, so those stay `unknown` too.
//
// Extension point: a second build profile (a Kani build that sets `kani`) is a
// `BuildProfile` whose `options` map has `kani` true. `decide` takes the profile,
// the resolver reads only the persisted value, so adding a profile changes this
// module and the column set, not the resolver. Only the default profile is
// written today.

use std::collections::BTreeMap;

use super::feature_gated::FileFeatures;
use crate::graph_store::{
    cfg_gates_in, FileCfg, GraphStore, CFG_ACTIVE, CFG_INACTIVE, CFG_UNKNOWN,
};
use crate::parser::cfg_compact::parse_compact;
use crate::parser::cfg_expr::{BuildProfile, CfgPredicate, Truth};

/// The verdict for the twin `id`, whose file is compiled with `features`.
pub(super) fn decide(id: &str, features: Option<&FileFeatures>) -> &'static str {
    match features {
        None | Some(FileFeatures::Disagree) => CFG_UNKNOWN,
        Some(FileFeatures::CompiledOut) => CFG_INACTIVE,
        Some(FileFeatures::Enabled(enabled)) => {
            evaluate(id, &BuildProfile::with_features(enabled.clone()))
        }
    }
}

fn evaluate(id: &str, profile: &BuildProfile) -> &'static str {
    let mut conjuncts = Vec::new();
    for gate in cfg_gates_in(id) {
        match parse_compact(gate) {
            Some(predicate) => conjuncts.push(predicate),
            None => return CFG_UNKNOWN,
        }
    }
    match CfgPredicate::All(conjuncts).eval_in(profile) {
        Truth::True => CFG_ACTIVE,
        Truth::False => CFG_INACTIVE,
        Truth::Unknown => CFG_UNKNOWN,
    }
}

/// Writes `cfg_active` on every twin of `store`; the count of twins written.
///
/// A twin in a file the default profile compiles out (`file_cfg`, issue #420) is
/// `inactive`, whatever its own gate says: `not(kani)` holds in a default build,
/// but the `#[cfg(kani)] mod` that declares the file does not.
pub(super) fn write(
    store: &GraphStore,
    file_features: &BTreeMap<String, FileFeatures>,
    file_cfg: &BTreeMap<String, FileCfg>,
) -> Result<usize, String> {
    let assignments: Vec<(String, String, &'static str)> = store
        .cfg_twin_ids()
        .into_iter()
        .map(|(label, id)| {
            let file = crate::language_provider::extract_file_prefix(&id);
            let features = file.as_deref().and_then(|f| file_features.get(f));
            let compiled_out = file
                .as_deref()
                .and_then(|f| file_cfg.get(f))
                .is_some_and(|cfg| cfg.active == CFG_INACTIVE);
            let verdict = if compiled_out {
                CFG_INACTIVE
            } else {
                decide(&id, features)
            };
            (label, id, verdict)
        })
        .collect();
    store.write_cfg_active(&assignments)?;
    Ok(assignments.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn enabled(features: &[&str]) -> FileFeatures {
        FileFeatures::Enabled(
            features
                .iter()
                .map(|f| f.to_string())
                .collect::<BTreeSet<_>>(),
        )
    }

    const FAST: &str = "src/lib.rs::pick#cfg(feature=fast)";
    const SLOW: &str = "src/lib.rs::pick#cfg(not(feature=fast))";

    #[test]
    fn a_feature_twin_is_decided_by_the_default_features() {
        let off = enabled(&[]);
        assert_eq!(decide(FAST, Some(&off)), CFG_INACTIVE);
        assert_eq!(decide(SLOW, Some(&off)), CFG_ACTIVE);
        let on = enabled(&["fast"]);
        assert_eq!(decide(FAST, Some(&on)), CFG_ACTIVE);
        assert_eq!(decide(SLOW, Some(&on)), CFG_INACTIVE);
    }

    #[test]
    fn an_option_no_plain_build_sets_is_false_under_the_default_profile() {
        let off = enabled(&[]);
        assert_eq!(decide("src/lib.rs::f#cfg(kani)", Some(&off)), CFG_INACTIVE);
        assert_eq!(
            decide("src/lib.rs::f#cfg(not(kani))", Some(&off)),
            CFG_ACTIVE
        );
        assert_eq!(decide("src/lib.rs::f#cfg(miri)", Some(&off)), CFG_INACTIVE);
        assert_eq!(decide("src/lib.rs::f#cfg(doc)", Some(&off)), CFG_INACTIVE);
        assert_eq!(
            decide("src/lib.rs::f#cfg(not(doctest))", Some(&off)),
            CFG_ACTIVE
        );
    }

    #[test]
    fn an_option_the_build_invocation_decides_stays_unknown() {
        let off = enabled(&[]);
        for gate in ["unix", "not(unix)", "test", "target_os=linux", "windows"] {
            let id = format!("src/lib.rs::f#cfg({gate})");
            assert_eq!(decide(&id, Some(&off)), CFG_UNKNOWN, "{gate}");
        }
        assert_eq!(
            decide("src/lib.rs::f#cfg(all(not(kani),unix))", Some(&off)),
            CFG_UNKNOWN
        );
    }

    #[test]
    fn a_kani_profile_inverts_the_twins_of_kani() {
        let mut kani = BuildProfile::with_features(BTreeSet::new());
        kani.options.insert("kani".to_string(), true);
        assert_eq!(evaluate("src/lib.rs::f#cfg(kani)", &kani), CFG_ACTIVE);
        assert_eq!(
            evaluate("src/lib.rs::f#cfg(not(kani))", &kani),
            CFG_INACTIVE
        );
        assert_eq!(evaluate("src/lib.rs::f#cfg(unix)", &kani), CFG_UNKNOWN);
    }

    #[test]
    fn a_file_without_a_cargo_map_leaves_kani_unknown() {
        assert_eq!(decide("src/lib.rs::f#cfg(kani)", None), CFG_UNKNOWN);
        assert_eq!(
            decide("src/lib.rs::f#cfg(kani)", Some(&FileFeatures::Disagree)),
            CFG_UNKNOWN
        );
    }

    #[test]
    fn a_known_false_feature_decides_a_conjunction_beside_an_unknown_option() {
        let off = enabled(&[]);
        assert_eq!(
            decide("src/lib.rs::f#cfg(all(feature=fast,kani))", Some(&off)),
            CFG_INACTIVE
        );
    }

    #[test]
    fn a_member_of_a_twin_module_takes_the_gates_of_its_whole_path() {
        let off = enabled(&[]);
        assert_eq!(
            decide("src/lib.rs::m#cfg(not(feature=fast))::f", Some(&off)),
            CFG_ACTIVE
        );
        assert_eq!(
            decide(
                "src/lib.rs::m#cfg(not(feature=fast))::f#cfg(unix)",
                Some(&off)
            ),
            CFG_UNKNOWN
        );
    }

    #[test]
    fn nothing_is_decided_without_features_or_when_packages_disagree() {
        assert_eq!(decide(SLOW, None), CFG_UNKNOWN);
        assert_eq!(decide(SLOW, Some(&FileFeatures::Disagree)), CFG_UNKNOWN);
    }

    #[test]
    fn a_file_the_default_features_compile_out_holds_only_inactive_items() {
        assert_eq!(decide(SLOW, Some(&FileFeatures::CompiledOut)), CFG_INACTIVE);
    }

    #[test]
    fn a_gate_that_did_not_parse_is_unknown() {
        let off = enabled(&[]);
        assert_eq!(decide("src/lib.rs::f#cfg(a::b)", Some(&off)), CFG_UNKNOWN);
    }
}
