// tool_profile — selects which MCP tools the server registers.
//
// Two profiles:
//   core — the agent-facing code-intelligence surface (8 tools). Hides the
//          internal pipeline stages (finding → PRD vocabulary) that only the
//          ai-architect orchestrator calls.
//   full — every tool. The default: shrinking the default tool surface is a
//          breaking change reserved for the next major version bump.
//
// Selection precedence: `--profile` CLI flag > `AP_PROFILE` env var > Full.

use serde_json::Value;

/// CLI flag selecting the profile (`--profile core` or `--profile=core`).
pub const PROFILE_FLAG: &str = "--profile";

/// `PROFILE_FLAG` in its `--profile=<value>` spelling.
const PROFILE_FLAG_EQ: &str = "--profile=";

/// Environment variable selecting the profile when the flag is absent.
pub const PROFILE_ENV_VAR: &str = "AP_PROFILE";

/// The agent-facing tool set registered by the `core` profile.
///
/// source: README.md "The pipeline" — stages 3a–3e are the read-only
/// code-intelligence surface an outside agent uses; stages 1/2/4/6/8/9 are
/// internal pipeline plumbing. `analyze_codebase` runs index + resolve +
/// cluster in one call, so the manual 3a/3b/3c passes (`index_codebase`,
/// `resolve_graph`, `lsp_resolve`, `cluster_graph`) are not needed here.
pub const CORE_TOOL_NAMES: [&str; 8] = [
    "health_check",
    "analyze_codebase",
    "search_codebase",
    "get_context",
    "get_symbol",
    "get_impact",
    "query_graph",
    "detect_changes",
];

/// Which tool set the server registers. Resolved once at startup and passed
/// down the dispatch chain — never stored in global state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolProfile {
    /// Agent-facing code-intelligence tools only (`CORE_TOOL_NAMES`).
    Core,
    /// Every tool, including internal pipeline stages.
    Full,
}

impl ToolProfile {
    /// Parses a profile name. Accepts exactly `core` and `full`.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "core" => Ok(Self::Core),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "invalid profile '{other}': expected 'core' or 'full'"
            )),
        }
    }

    /// Resolves the active profile from CLI arguments and the environment.
    /// The CLI flag wins over the env var; when both are absent the default
    /// is `Full` (a default change is breaking — see module header).
    pub fn resolve(args: &[String], env_value: Option<&str>) -> Result<Self, String> {
        if let Some(value) = flag_value(args)? {
            return Self::parse(&value);
        }
        match env_value {
            Some(value) => Self::parse(value),
            None => Ok(Self::Full),
        }
    }

    /// Whether `tool_name` is registered under this profile.
    pub fn allows(self, tool_name: &str) -> bool {
        match self {
            Self::Full => true,
            Self::Core => CORE_TOOL_NAMES.contains(&tool_name),
        }
    }

    /// Profile name as spelled on the CLI (used in log lines and errors).
    pub fn name(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Full => "full",
        }
    }

    /// Filters a full `tools/list` payload down to the tools this profile
    /// registers, preserving the original schema order.
    pub fn filter_tools_list(self, mut payload: Value) -> Value {
        if let Some(tools) = payload.get_mut("tools").and_then(Value::as_array_mut) {
            tools.retain(|tool| {
                tool.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| self.allows(name))
            });
        }
        payload
    }
}

/// Extracts the value of the profile flag from raw CLI args (program name
/// already stripped). Supports `--profile <value>` and `--profile=<value>`;
/// the last occurrence wins. A trailing `--profile` with no value is an error.
fn flag_value(args: &[String]) -> Result<Option<String>, String> {
    let mut found = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == PROFILE_FLAG {
            match iter.next() {
                Some(value) => found = Some(value.clone()),
                None => return Err(format!("{PROFILE_FLAG} requires a value: 'core' or 'full'")),
            }
        } else if let Some(value) = arg.strip_prefix(PROFILE_FLAG_EQ) {
            found = Some(value.to_string());
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_schemas;

    /// Every tool the server ships, in `tool_schemas::tools_list()` order.
    /// Kept literal so adding a tool forces a conscious profile decision.
    const FULL_TOOL_NAMES: [&str; 27] = [
        "health_check",
        "extract_finding",
        "refine_finding",
        "start_verification",
        "append_clarification",
        "finalize_verification",
        "abort_verification",
        "index_codebase",
        "index_status",
        "ingest_traces",
        "query_graph",
        "get_symbol",
        "resolve_graph",
        "cluster_graph",
        "get_processes",
        "get_impact",
        "index_history",
        "search_codebase",
        "get_context",
        "analyze_codebase",
        "detect_changes",
        "lsp_resolve",
        "prepare_prd_input",
        "validate_prd_against_graph",
        "check_security_gates",
        "verify_semantic_diff",
        "check_doc_claims",
    ];

    fn registered_names(profile: ToolProfile) -> Vec<String> {
        profile
            .filter_tools_list(tool_schemas::tools_list())
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools_list must return a `tools` array")
            .iter()
            .map(|tool| {
                tool.get("name")
                    .and_then(Value::as_str)
                    .expect("every tool schema must carry a name")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn full_profile_registers_exactly_all_tools() {
        assert_eq!(registered_names(ToolProfile::Full), FULL_TOOL_NAMES);
    }

    #[test]
    fn core_profile_registers_exactly_the_core_tools() {
        let mut registered = registered_names(ToolProfile::Core);
        registered.sort_unstable();
        let mut expected: Vec<String> = CORE_TOOL_NAMES.iter().map(|n| n.to_string()).collect();
        expected.sort_unstable();
        assert_eq!(registered, expected);
    }

    #[test]
    fn core_tool_names_are_a_subset_of_the_full_set() {
        for name in CORE_TOOL_NAMES {
            assert!(
                FULL_TOOL_NAMES.contains(&name),
                "core tool '{name}' is not in tools_list()"
            );
        }
    }

    #[test]
    fn parse_accepts_only_core_and_full() {
        assert_eq!(ToolProfile::parse("core"), Ok(ToolProfile::Core));
        assert_eq!(ToolProfile::parse("full"), Ok(ToolProfile::Full));
        assert!(ToolProfile::parse("Core").is_err());
        assert!(ToolProfile::parse("").is_err());
        assert!(ToolProfile::parse("all").is_err());
    }

    #[test]
    fn resolve_defaults_to_full() {
        assert_eq!(ToolProfile::resolve(&[], None), Ok(ToolProfile::Full));
    }

    #[test]
    fn resolve_reads_env_when_no_flag() {
        assert_eq!(
            ToolProfile::resolve(&[], Some("core")),
            Ok(ToolProfile::Core)
        );
        assert!(ToolProfile::resolve(&[], Some("bogus")).is_err());
    }

    #[test]
    fn resolve_flag_wins_over_env() {
        let args = vec!["--profile".to_string(), "full".to_string()];
        assert_eq!(
            ToolProfile::resolve(&args, Some("core")),
            Ok(ToolProfile::Full)
        );
    }

    #[test]
    fn resolve_supports_equals_spelling() {
        let args = vec!["--profile=core".to_string()];
        assert_eq!(ToolProfile::resolve(&args, None), Ok(ToolProfile::Core));
    }

    #[test]
    fn resolve_rejects_flag_without_value() {
        let args = vec!["--profile".to_string()];
        assert!(ToolProfile::resolve(&args, None).is_err());
    }

    #[test]
    fn allows_matches_the_registered_sets() {
        assert!(ToolProfile::Core.allows("search_codebase"));
        assert!(!ToolProfile::Core.allows("prepare_prd_input"));
        assert!(!ToolProfile::Core.allows("no_such_tool"));
        assert!(ToolProfile::Full.allows("prepare_prd_input"));
    }
}
