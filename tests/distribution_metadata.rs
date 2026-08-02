//! Cross-host distribution metadata stays aligned with the crate and with the
//! deliberately small agent-facing profile.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const PORTABLE_SKILLS: [&str; 3] = [
    "understand-codebase",
    "impact-analysis",
    "validate-change-plan",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn read_json(relative: &str) -> Value {
    let path = root().join(relative);
    serde_json::from_str(&read(&path))
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

#[test]
fn codex_and_gemini_launch_the_core_profile_at_the_crate_version() {
    let plugin = read_json("plugins/ai-architect/.codex-plugin/plugin.json");
    let codex_mcp = read_json("plugins/ai-architect/.mcp.json");
    let gemini = read_json("gemini-extension.json");

    assert_eq!(plugin["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(gemini["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(plugin["mcpServers"], "./.mcp.json");

    let expected_args = json!(["--profile", "core"]);
    assert_eq!(
        codex_mcp["mcpServers"]["ai-architect"]["args"],
        expected_args
    );
    assert_eq!(gemini["mcpServers"]["ai-architect"]["args"], expected_args);
}

#[test]
fn codex_marketplace_points_at_the_isolated_plugin() {
    let marketplace = read_json(".agents/plugins/marketplace.json");
    let entry = marketplace["plugins"]
        .as_array()
        .expect("plugins array")
        .iter()
        .find(|entry| entry["name"] == "ai-architect")
        .expect("ai-architect marketplace entry");

    assert_eq!(marketplace["name"], "automatised-pipeline");
    assert_eq!(entry["source"]["source"], "local");
    assert_eq!(entry["source"]["path"], "./plugins/ai-architect");
    assert_eq!(entry["policy"]["installation"], "AVAILABLE");
    assert_eq!(entry["policy"]["authentication"], "ON_INSTALL");
}

#[test]
fn generated_codex_skills_match_the_canonical_gemini_sources() {
    for skill in PORTABLE_SKILLS {
        let gemini_root = root().join("skills").join(skill);
        let codex_root = root().join("plugins/ai-architect/skills").join(skill);
        let gemini_skill = read(gemini_root.join("SKILL.md"));
        let codex_skill = read(codex_root.join("SKILL.md"));
        let gemini_interface = read(gemini_root.join("agents/openai.yaml"));
        let codex_interface = read(codex_root.join("agents/openai.yaml"));

        assert_eq!(
            gemini_skill, codex_skill,
            "{skill} generated workflow drifted"
        );
        assert_eq!(
            gemini_interface, codex_interface,
            "{skill} generated interface metadata drifted"
        );
        assert!(gemini_skill.starts_with("---\nname: "));
        assert!(!gemini_skill.contains("[TODO:"));
    }
}

#[test]
fn claude_project_manifest_keeps_the_existing_full_profile_default() {
    let claude_project_mcp = read_json(".mcp.json");
    let server = &claude_project_mcp["mcpServers"]["automatised-pipeline"];

    assert_eq!(server["command"], "python3");
    assert!(
        !server.to_string().contains("--profile"),
        "the Claude project manifest must retain the server's full default"
    );
}
