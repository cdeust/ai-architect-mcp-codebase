// cargo_features — the feature set a package enables when built with default
// features (issue #291).
//
// Layer: pure, no I/O. Input is one package's `features` object from
// `cargo metadata --format-version 1`, which lists every `[features]` entry
// plus one implicit feature per optional dependency not referenced through
// `dep:` (measured 2026-09-23, cargo 1.95.0: `dep1 = { optional = true }`
// appears as `"dep1": ["dep:dep1"]`; one only named as `dep:dep2` does not).
// source: The Cargo Book, "Features" (`default`, `dep:` syntax, `pkg/feat`
// and `pkg?/feat` dependency-feature syntax).

use std::collections::{BTreeMap, BTreeSet};

/// The features enabled by `default`, transitively. An entry names a feature
/// of this package when it is a key of `features`; `dep:x` enables a
/// dependency, not a feature; `x/y` also enables the feature `x` when `x` is
/// one (an optional dependency's implicit feature); `x?/y` never does.
/// A missing `default` key enables nothing.
pub(crate) fn default_closure(features: &BTreeMap<String, Vec<String>>) -> BTreeSet<String> {
    let mut enabled = BTreeSet::new();
    let mut pending: Vec<&str> = features
        .get("default")
        .map(|entries| entries.iter().map(String::as_str).collect())
        .unwrap_or_default();
    if features.contains_key("default") {
        enabled.insert("default".to_string());
    }
    while let Some(entry) = pending.pop() {
        let Some(name) = enabled_feature(entry).filter(|n| features.contains_key(*n)) else {
            continue;
        };
        if enabled.insert(name.to_string()) {
            pending.extend(features[name].iter().map(String::as_str));
        }
    }
    enabled
}

/// The feature of this package an entry of a feature's list turns on, if any.
fn enabled_feature(entry: &str) -> Option<&str> {
    if entry.starts_with("dep:") {
        return None;
    }
    match entry.split_once('/') {
        Some((dep, _)) if dep.ends_with('?') => None,
        Some((dep, _)) => Some(dep),
        None => Some(entry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(entries: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
            .collect()
    }

    fn names(set: &BTreeSet<String>) -> Vec<&str> {
        set.iter().map(String::as_str).collect()
    }

    /// The `features` object `cargo metadata` printed for the probe package
    /// (2026-09-23): `default = ["x", "dep1/std"]`, `x = ["y"]`,
    /// `z = ["dep:dep2"]`, optional `dep1` (implicit) and `dep2` (`dep:` only).
    #[test]
    fn the_default_closure_follows_features_and_implicit_optional_deps() {
        let features = table(&[
            ("default", &["x", "dep1/std"]),
            ("dep1", &["dep:dep1"]),
            ("x", &["y"]),
            ("y", &[]),
            ("z", &["dep:dep2"]),
        ]);
        assert_eq!(
            names(&default_closure(&features)),
            ["default", "dep1", "x", "y"]
        );
    }

    #[test]
    fn a_weak_dependency_feature_does_not_enable_the_dependency() {
        let features = table(&[("default", &["dep1?/std"]), ("dep1", &["dep:dep1"])]);
        assert_eq!(names(&default_closure(&features)), ["default"]);
    }

    #[test]
    fn no_default_key_enables_nothing() {
        let features = table(&[("extra", &[])]);
        assert!(default_closure(&features).is_empty());
    }
}
