//! `full_manifest` refuses a tree over the byte cap before hashing it (#478).
use super::*;

#[test]
fn a_tree_over_the_cap_is_refused_before_the_snapshot_hashes_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.rs"), "pub fn a() {}\n").expect("write");
    let opts = IndexOptions::default();
    let err = full_manifest_within(dir.path(), &opts, 1).expect_err("14 bytes > 1");
    assert!(err.starts_with("total_bytes_exceeded"), "{err}");
    let m = full_manifest_within(dir.path(), &opts, 1 << 20).expect("under the cap");
    assert_eq!(m.files.len(), 1);
    assert!(!m.files["a.rs"].content_hash.is_empty());
}
