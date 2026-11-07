// wiki::checks — the path rule and the duplicate-key map reader used by the
// skeleton (issue #196). Split out of skeleton.rs to keep it under the §4.1
// 500-line cap.

use super::error::SkeletonError;
use std::collections::BTreeMap;

/// Refuse anything a later slice could turn into a path outside the repo
/// (CWE-22): an empty path, an absolute or drive/UNC-prefixed one, a `..`
/// component, a backslash or a control character. Empty and `.` components
/// are refused too, so one file has one spelling (`src/a.rs`, never
/// `./src/a.rs` or `src//a.rs`) and `source_files` cannot list it twice.
/// Callers that open a file must still join it to the repo root and check
/// the canonical prefix.
pub(super) fn repo_relative(file: &str) -> Result<(), SkeletonError> {
    if file.is_empty() {
        return Err(SkeletonError::EmptyFile);
    }
    let bytes = file.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    // `starts_with('/')` is also caught by the empty first segment below; it
    // stays as defence in depth, so the absolute-path rule is explicit.
    let unsafe_path = file.starts_with('/')
        || drive
        || file.contains('\\')
        || file.chars().any(char::is_control)
        || file
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    if unsafe_path {
        return Err(SkeletonError::UnsafePath(file.to_owned()));
    }
    Ok(())
}

/// A JSON object whose keys are all distinct: serde's `BTreeMap` would keep
/// the last of two equal keys, so the same entries in another order would
/// read back differently.
pub(super) fn unique_community_keys<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, u64>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a map of community ids to edge counts")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some((key, count)) = map.next_entry::<String, u64>()? {
                // Refused even when both values are equal: one key, one entry.
                match out.entry(key) {
                    std::collections::btree_map::Entry::Occupied(e) => {
                        let msg = format!("cross_community_edges repeats key {:?}", e.key());
                        return Err(serde::de::Error::custom(msg));
                    }
                    std::collections::btree_map::Entry::Vacant(e) => {
                        e.insert(count);
                    }
                }
            }
            Ok(out)
        }
    }
    deserializer.deserialize_map(Unique)
}

/// A value read only from a JSON object. serde's derived `Deserialize` also
/// accepts a JSON array (`visit_seq`), which would give every struct a second
/// spelling (`["src/a.rs", 3]` for a location); this wrapper refuses it.
pub(super) struct MapOnly<T>(pub(super) T);

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for MapOnly<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Object<T>(std::marker::PhantomData<T>);
        impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for Object<T> {
            type Value = T;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map))
            }
        }
        deserializer
            .deserialize_map(Object(std::marker::PhantomData))
            .map(MapOnly)
    }
}

/// `deserialize_with` for a struct field that must be a JSON object.
pub(super) fn object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <MapOnly<T> as serde::Deserialize>::deserialize(deserializer).map(|m| m.0)
}

/// `deserialize_with` for a list whose every item must be a JSON object.
pub(super) fn objects<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    let items: Vec<MapOnly<T>> = serde::Deserialize::deserialize(deserializer)?;
    Ok(items.into_iter().map(|m| m.0).collect())
}
