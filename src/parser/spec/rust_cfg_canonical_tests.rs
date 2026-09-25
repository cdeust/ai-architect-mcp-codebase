// parser::spec::rust_cfg_canonical_tests: the canonical form of a `#[cfg]` gate
// is what twin ids are made of, and a graph records which form wrote it
// (`graph_store::CANONICAL_FORM_VERSION`, issue #353). This test pins the form:
// the spellings below give the ids below on every build that carries the current
// version. Changing any of them changes twin ids, so the pinned pair (version,
// digest) must change TOGETHER: bump `CANONICAL_FORM_VERSION`, then update the
// digest. An old graph is then refused instead of silently mixing two forms.

use crate::graph_store::CANONICAL_FORM_VERSION;
use crate::parser::{parse_file, Language};
use sha2::{Digest, Sha256};

/// (spelling of the gate on the first twin, the id suffix it must give).
const GOLDEN: &[(&str, &str)] = &[
    ("feature = \"fast\"", "#cfg(feature=fast)"),
    ("feature=\"a\\x62\"", "#cfg(feature=ab)"),
    ("feature = \"a\\\"b\"", "#cfg(feature=a%22b)"),
    ("feature = \"a\" /* c */", "#cfg(feature=a)"),
    ("all(unix, feature = \"x\")", "#cfg(all(feature=x,unix))"),
    ("any(a, any(b, a))", "#cfg(any(a,b))"),
    ("not(not(kani))", "#cfg(kani)"),
    ("target_os = \"a.b\"", "#cfg(target_os=a%2Eb)"),
];

fn id_suffix(gate: &str) -> String {
    let source =
        format!("#[cfg({gate})]\nfn f() {{}}\n#[cfg(not(cfg_marker_twin))]\nfn f() {{}}\n");
    let parsed = parse_file(&source, "lib.rs", Language::Rust).expect("parse");
    let first = parsed
        .nodes
        .iter()
        .find(|n| n.name == "f" && n.start_line == 2)
        .expect("first twin");
    first
        .qualified_name
        .strip_prefix("lib.rs::f")
        .unwrap()
        .to_string()
}

#[test]
fn every_golden_spelling_gives_its_pinned_id() {
    for (gate, suffix) in GOLDEN {
        assert_eq!(&id_suffix(gate), suffix, "gate {gate}");
    }
}

/// A comment, a decoded escape and a plain spelling of one gate are ONE id.
#[test]
fn the_escaped_and_the_plain_spelling_of_a_feature_give_one_id() {
    assert_eq!(
        id_suffix("feature = \"a\\x62\""),
        id_suffix("feature = \"ab\"")
    );
    assert_eq!(
        id_suffix("feature = \"a\" /* c */"),
        id_suffix("feature = \"a\"")
    );
}

/// The pair that ties the form to its version: edit the goldens, bump the
/// version, and put the printed digest here.
#[test]
fn the_golden_forms_are_pinned_to_the_canonical_form_version() {
    let mut hasher = Sha256::new();
    for (gate, suffix) in GOLDEN {
        hasher.update(format!("{gate}=>{suffix}\n").as_bytes());
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        (CANONICAL_FORM_VERSION, digest.as_str()),
        (
            2,
            "171ede3cd0cd2f48769c3d8012faa8ee8110c91471cac48dbc82a3c3503d1397"
        ),
        "the canonical form changed: bump graph_store::CANONICAL_FORM_VERSION and update this pair"
    );
}
