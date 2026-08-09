use super::*;
use serde_json::json;

#[test]
fn test_load_verified_rejects_false_flag() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    use crate::test_support::TempDirExt;
    let tmp = tempfile::Builder::new()
        .prefix("prd_input_false_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let body = json!({
        "verified": false,
        "finalized_at": "2026-04-11T00:00:00Z",
        "stage1_refined_path": "findings/f/stage-1.refined.json",
    });
    fs::write(
        tmp.join("stage-2.verified.json"),
        serde_json::to_vec_pretty(&body).unwrap(),
    )
    .unwrap();
    let err = load_verified(&tmp).err().unwrap();
    assert!(err.contains("stage_2_not_verified"), "got: {err}");
    let _ = fs::remove_dir_all(&tmp);
}
