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
    let plugin = read_json("plugins/ai-architect-mcp-codebase/.codex-plugin/plugin.json");
    let codex_mcp = read_json("plugins/ai-architect-mcp-codebase/.mcp.json");
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
        .find(|entry| entry["name"] == "ai-architect-mcp-codebase")
        .expect("ai-architect-mcp-codebase marketplace entry");

    assert_eq!(marketplace["name"], "ai-architect-mcp-codebase");
    assert_eq!(entry["source"]["source"], "local");
    assert_eq!(
        entry["source"]["path"],
        "./plugins/ai-architect-mcp-codebase"
    );
    assert_eq!(entry["policy"]["installation"], "AVAILABLE");
    assert_eq!(entry["policy"]["authentication"], "ON_INSTALL");
}

#[test]
fn generated_codex_skills_match_the_canonical_gemini_sources() {
    for skill in PORTABLE_SKILLS {
        let gemini_root = root().join("skills").join(skill);
        let codex_root = root()
            .join("plugins/ai-architect-mcp-codebase/skills")
            .join(skill);
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
    let server = &claude_project_mcp["mcpServers"]["ai-architect"];

    assert_eq!(
        server["command"],
        "${CLAUDE_PLUGIN_ROOT}/bin/launch-plugin.sh"
    );
    assert!(
        !server.to_string().contains("--profile"),
        "the Claude project manifest must retain the server's full default"
    );
    assert_eq!(server["args"], json!([]));
}

#[test]
fn canonical_distribution_identity_is_consistent() {
    let contract = read_json("mcp-contract.json");
    let server = read_json("server.json");
    let mcpb = read_json("manifest.json");
    let claude_plugin = read_json(".claude-plugin/plugin.json");
    let claude_marketplace = read_json(".claude-plugin/marketplace.json");

    let distribution = contract["distribution"].as_str().unwrap();
    let plugin_name = contract["claude_plugin"].as_str().unwrap();
    let marketplace_name = contract["claude_marketplace"].as_str().unwrap();
    let mcp_server = contract["mcp_server"].as_str().unwrap();
    let actual_mcp = read_json(".mcp.json");
    let actual_server_keys: Vec<_> = actual_mcp["mcpServers"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(actual_server_keys, vec![mcp_server]);
    let derived_prefix = format!("mcp__plugin_{plugin_name}_{mcp_server}__");

    assert_eq!(env!("CARGO_PKG_NAME"), distribution);
    assert_eq!(server["name"], format!("io.github.cdeust/{distribution}"));
    assert_eq!(mcpb["name"], distribution);
    assert_eq!(claude_plugin["name"], plugin_name);
    assert_eq!(claude_marketplace["name"], marketplace_name);
    assert_eq!(claude_marketplace["plugins"][0]["name"], plugin_name);
    assert_eq!(
        read_json(".agents/plugins/marketplace.json")["plugins"][0]["name"],
        distribution
    );
    assert_eq!(
        read_json("plugins/ai-architect-mcp-codebase/.codex-plugin/plugin.json")["name"],
        distribution
    );
    assert_eq!(read_json("gemini-extension.json")["name"], distribution);
    assert_eq!(contract["claude_tool_prefix"], derived_prefix);
    assert_eq!(contract["schema_version"], 1);

    for version in [
        &server["version"],
        &server["packages"][0]["version"],
        &mcpb["version"],
        &claude_plugin["version"],
        &claude_marketplace["metadata"]["version"],
        &claude_marketplace["plugins"][0]["version"],
    ] {
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
    }

    for document in [server, mcpb, claude_plugin, claude_marketplace] {
        let serialized = document.to_string();
        assert!(
            !serialized.contains("github.com/cdeust/automatised-pipeline"),
            "public distribution metadata contains the retired repository URL"
        );
    }
}
