// host_install — never-clobber MCP config merge for the top agent hosts (#59).
//
// Layer: shared/utility. Pure text→text merges (no filesystem, no process): given
// a host's EXISTING config file content (or `None` when absent) and our server
// spec, produce the new content to write — adding or updating ONLY our entry and
// preserving everything else. The CLI layer (`main.rs`) does the I/O, detection,
// and flag handling; these functions are what the tests pin exactly.
//
// Never-clobber contract:
//   * A file we cannot parse is NEVER overwritten — we `Refuse` and the CLI
//     prints the exact entry to add by hand. This covers Zed's JSONC (comments +
//     trailing commas make it invalid strict JSON) and any hand-corrupted file.
//   * Other servers' entries are carried through untouched (deep-equal) — a JSON
//     reserialize preserves their VALUES; a TOML edit (toml_edit) preserves their
//     bytes, comments, and formatting.
//   * Re-running is idempotent: when our entry already equals the desired one we
//     return `NoChange`.
//
// Per-host dialects (verified against DeusData/codebase-memory-mcp src/cli/cli.c):
//   * Claude Code / Cursor / Gemini → JSON, entry under top-level `mcpServers`.
//   * VS Code                        → JSON, entry under `servers`, `type:"stdio"`.
//   * Zed                            → JSON(C), entry under `context_servers`.
//   * Codex                          → TOML, `[mcp_servers.<name>]`.

use serde_json::{json, Value};

/// The MCP server entry to install (or the name to uninstall).
#[derive(Debug, Clone)]
pub struct ServerSpec {
    /// Server key, e.g. "ai-architect".
    pub name: String,
    /// Absolute path to the server binary.
    pub command: String,
    /// Launch args (usually empty — the binary serves MCP on stdio by default).
    pub args: Vec<String>,
}

/// The per-host config dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// JSON, entry under top-level `mcpServers` (Claude Code, Cursor, Gemini CLI).
    JsonMcpServers,
    /// JSON, entry under top-level `servers`, with `"type":"stdio"` (VS Code).
    JsonServersVscode,
    /// JSON, entry under top-level `context_servers` (Zed). JSONC → Refuse.
    JsonContextServersZed,
    /// TOML, `[mcp_servers.<name>]` (Codex), edited comment-preserving.
    TomlCodex,
}

impl Format {
    /// The top-level object key the entry lives under (JSON formats only).
    fn json_container(self) -> Option<&'static str> {
        match self {
            Format::JsonMcpServers => Some("mcpServers"),
            Format::JsonServersVscode => Some("servers"),
            Format::JsonContextServersZed => Some("context_servers"),
            Format::TomlCodex => None,
        }
    }
    fn is_json(self) -> bool {
        self.json_container().is_some()
    }
}

/// Outcome of a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// New whole-file content to write.
    Write(String),
    /// Our entry is already present and identical — nothing to do (idempotent).
    NoChange,
    /// The file cannot be edited safely (unparseable / JSONC). The string is a
    /// human-readable reason + the exact entry to add by hand.
    Refuse(String),
}

/// The desired JSON entry value for `spec` under `format`'s container.
fn desired_json_entry(spec: &ServerSpec, format: Format) -> Value {
    let mut obj = serde_json::Map::new();
    if format == Format::JsonServersVscode {
        // VS Code's mcp.json entries declare the transport explicitly.
        obj.insert("type".into(), json!("stdio"));
    }
    obj.insert("command".into(), json!(spec.command));
    obj.insert("args".into(), json!(spec.args));
    Value::Object(obj)
}

/// Merges our server entry into `existing` (None = file absent) for `format`.
///
/// Postconditions: `Write(content)` carries the whole new file with only our
/// entry added/updated and every other entry preserved; `NoChange` means our
/// entry already equals the desired one; `Refuse(msg)` means the file could not
/// be parsed (never overwritten). Never panics.
pub fn merge_install(existing: Option<&str>, spec: &ServerSpec, format: Format) -> MergeOutcome {
    if format.is_json() {
        merge_install_json(existing, spec, format)
    } else {
        merge_install_toml(existing, spec)
    }
}

/// Removes our entry `name` from `existing` for `format`. `NoChange` when our
/// entry is absent; `Refuse` when the file cannot be parsed.
pub fn merge_uninstall(existing: &str, name: &str, format: Format) -> MergeOutcome {
    if format.is_json() {
        merge_uninstall_json(existing, name, format)
    } else {
        merge_uninstall_toml(existing, name)
    }
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

fn merge_install_json(existing: Option<&str>, spec: &ServerSpec, format: Format) -> MergeOutcome {
    let container = format.json_container().expect("json format");
    let mut root: Value = match existing {
        None => json!({}),
        Some(text) if text.trim().is_empty() => json!({}),
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(v @ Value::Object(_)) => v,
            Ok(_) => {
                return refuse_json(
                    container,
                    spec,
                    format,
                    "the config root is not a JSON object",
                )
            }
            Err(_) => return refuse_json(
                container,
                spec,
                format,
                "the file is not valid strict JSON (e.g. Zed JSONC with comments/trailing commas)",
            ),
        },
    };

    let desired = desired_json_entry(spec, format);
    let obj = root.as_object_mut().expect("root is object");
    let cont = obj
        .entry(container.to_string())
        .or_insert_with(|| json!({}));
    let cont_obj = match cont.as_object_mut() {
        Some(o) => o,
        None => {
            return refuse_json(
                container,
                spec,
                format,
                "the servers container is not an object",
            )
        }
    };
    if cont_obj.get(&spec.name) == Some(&desired) {
        return MergeOutcome::NoChange;
    }
    cont_obj.insert(spec.name.clone(), desired);
    MergeOutcome::Write(pretty_json(&root))
}

fn merge_uninstall_json(existing: &str, name: &str, format: Format) -> MergeOutcome {
    let container = format.json_container().expect("json format");
    let mut root: Value = match serde_json::from_str::<Value>(existing) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            return MergeOutcome::Refuse(format!(
                "cannot parse the config to uninstall from; remove the '{name}' entry under \
                 '{container}' by hand"
            ))
        }
    };
    let removed = root
        .as_object_mut()
        .and_then(|o| o.get_mut(container))
        .and_then(|c| c.as_object_mut())
        .map(|c| c.remove(name).is_some())
        .unwrap_or(false);
    if !removed {
        return MergeOutcome::NoChange;
    }
    MergeOutcome::Write(pretty_json(&root))
}

fn refuse_json(container: &str, spec: &ServerSpec, format: Format, why: &str) -> MergeOutcome {
    let entry = desired_json_entry(spec, format);
    let snippet = pretty_json(&json!({ container: { &spec.name: entry } }));
    MergeOutcome::Refuse(format!(
        "not editing in place ({why}). Add this to the config by hand (merge into any existing \
         '{container}'):\n{snippet}"
    ))
}

fn pretty_json(v: &Value) -> String {
    // Trailing newline: editors and git expect files to end with one.
    let mut s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    s
}

// ---------------------------------------------------------------------------
// TOML (Codex) — comment/format-preserving via toml_edit
// ---------------------------------------------------------------------------

fn merge_install_toml(existing: Option<&str>, spec: &ServerSpec) -> MergeOutcome {
    use toml_edit::{Array, DocumentMut, Item, Value as TomlValue};
    let mut doc: DocumentMut = match existing {
        None => DocumentMut::new(),
        Some(text) if text.trim().is_empty() => DocumentMut::new(),
        Some(text) => match text.parse::<DocumentMut>() {
            Ok(d) => d,
            Err(_) => {
                return MergeOutcome::Refuse(format!(
                    "not editing in place: config.toml is not valid TOML. Add by hand:\n\
                     [mcp_servers.{}]\ncommand = {:?}\nargs = []",
                    spec.name, spec.command
                ))
            }
        },
    };

    // Desired args array.
    let mut args = Array::new();
    for a in &spec.args {
        args.push(a.as_str());
    }

    // Idempotence: if the existing entry already matches, no change.
    let already = doc
        .get("mcp_servers")
        .and_then(|m| m.get(&spec.name))
        .map(|entry| {
            let cmd_ok = entry
                .get("command")
                .and_then(|c| c.as_str())
                .map(|c| c == spec.command)
                .unwrap_or(false);
            let args_ok = entry
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.len() == spec.args.len()
                        && a.iter()
                            .zip(&spec.args)
                            .all(|(x, y)| x.as_str() == Some(y.as_str()))
                })
                .unwrap_or(spec.args.is_empty() && entry.get("args").is_none());
            cmd_ok && args_ok
        })
        .unwrap_or(false);
    if already {
        return MergeOutcome::NoChange;
    }

    // Ensure [mcp_servers] table exists (implicit so it prints as
    // `[mcp_servers.<name>]`), then set our sub-table.
    let servers = doc
        .entry("mcp_servers")
        .or_insert(Item::Table(toml_edit::Table::new()));
    if let Item::Table(t) = servers {
        t.set_implicit(true);
    }
    let mut entry = toml_edit::Table::new();
    entry.insert(
        "command",
        Item::Value(TomlValue::from(spec.command.as_str())),
    );
    entry.insert("args", Item::Value(TomlValue::Array(args)));
    doc["mcp_servers"][&spec.name] = Item::Table(entry);

    MergeOutcome::Write(doc.to_string())
}

fn merge_uninstall_toml(existing: &str, name: &str) -> MergeOutcome {
    use toml_edit::DocumentMut;
    let mut doc: DocumentMut = match existing.parse::<DocumentMut>() {
        Ok(d) => d,
        Err(_) => {
            return MergeOutcome::Refuse(format!(
                "cannot parse config.toml to uninstall; remove [mcp_servers.{name}] by hand"
            ))
        }
    };
    let removed = doc
        .get_mut("mcp_servers")
        .and_then(|m| m.as_table_mut())
        .map(|t| t.remove(name).is_some())
        .unwrap_or(false);
    if !removed {
        return MergeOutcome::NoChange;
    }
    MergeOutcome::Write(doc.to_string())
}

// ---------------------------------------------------------------------------
// Claude Code PreToolUse hook registration (opt-in, --with-hooks)
// ---------------------------------------------------------------------------

/// The matcher our hook registers under `hooks.PreToolUse` in Claude Code's
/// settings.json.
pub const HOOK_MATCHER: &str = "Grep|Glob";

/// True when `command` is one of ours (ends with the `hook-augment` subcommand).
fn is_our_hook_command(command: &str) -> bool {
    command.trim_end().ends_with("hook-augment")
}

/// Registers our PreToolUse `hook-augment` hook into Claude Code's settings.json
/// (never-clobber: preserves every other hook and setting). `existing` is the
/// current settings.json content (None = absent). `command` is the full command
/// to run (e.g. `/path/to/ai-architect-mcp-codebase hook-augment`). Idempotent:
/// `NoChange` when a hook of ours with the same matcher+command already exists.
pub fn merge_hook_install(existing: Option<&str>, command: &str) -> MergeOutcome {
    let mut root: Value = match existing {
        None => json!({}),
        Some(t) if t.trim().is_empty() => json!({}),
        Some(t) => match serde_json::from_str::<Value>(t) {
            Ok(v @ Value::Object(_)) => v,
            _ => {
                return MergeOutcome::Refuse(format!(
                    "not editing Claude settings in place (unparseable). Add a PreToolUse hook by \
                     hand: matcher \"{HOOK_MATCHER}\", command \"{command}\"."
                ))
            }
        },
    };

    let obj = root.as_object_mut().expect("object");
    let hooks = obj.entry("hooks".to_string()).or_insert_with(|| json!({}));
    let hooks_obj = match hooks.as_object_mut() {
        Some(o) => o,
        None => return MergeOutcome::Refuse("'hooks' is not an object".into()),
    };
    let pre = hooks_obj
        .entry("PreToolUse".to_string())
        .or_insert_with(|| json!([]));
    let pre_arr = match pre.as_array_mut() {
        Some(a) => a,
        None => return MergeOutcome::Refuse("'hooks.PreToolUse' is not an array".into()),
    };

    // Idempotence: already have a matcher block containing our command?
    let present = pre_arr.iter().any(|block| {
        block
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hs| {
                hs.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(is_our_hook_command)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });
    if present {
        return MergeOutcome::NoChange;
    }

    pre_arr.push(json!({
        "matcher": HOOK_MATCHER,
        "hooks": [ { "type": "command", "command": command } ]
    }));
    MergeOutcome::Write(pretty_json(&root))
}

/// Removes our PreToolUse hook blocks from Claude Code's settings.json (any block
/// whose hooks include an ai-architect `hook-augment` command). Preserves all
/// other hooks. `NoChange` when none of ours are present.
pub fn merge_hook_uninstall(existing: &str) -> MergeOutcome {
    let mut root: Value = match serde_json::from_str::<Value>(existing) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            return MergeOutcome::Refuse(
                "cannot parse Claude settings to uninstall the hook".into(),
            )
        }
    };
    let pre = root
        .as_object_mut()
        .and_then(|o| o.get_mut("hooks"))
        .and_then(|h| h.as_object_mut())
        .and_then(|h| h.get_mut("PreToolUse"))
        .and_then(|p| p.as_array_mut());
    let arr = match pre {
        Some(a) => a,
        None => return MergeOutcome::NoChange,
    };
    let before = arr.len();
    arr.retain(|block| {
        // Keep a block unless ALL its hooks are ours (or it becomes empty of
        // non-ours hooks). Simpler + safe: drop a block if it contains any of our
        // commands AND no foreign ones; otherwise strip only our commands.
        let hooks = block.get("hooks").and_then(|h| h.as_array());
        match hooks {
            Some(hs) => !hs.iter().all(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(is_our_hook_command)
                    .unwrap_or(false)
            }),
            None => true,
        }
    });
    if arr.len() == before {
        return MergeOutcome::NoChange;
    }
    MergeOutcome::Write(pretty_json(&root))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ServerSpec {
        ServerSpec {
            name: "ai-architect".into(),
            command: "/usr/local/bin/ai-architect-mcp-codebase".into(),
            args: vec![],
        }
    }

    #[test]
    fn install_into_absent_json_creates_container() {
        let out = merge_install(None, &spec(), Format::JsonMcpServers);
        let content = match out {
            MergeOutcome::Write(c) => c,
            other => panic!("expected Write, got {other:?}"),
        };
        let v: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            v["mcpServers"]["ai-architect"]["command"],
            json!(spec().command)
        );
        assert_eq!(v["mcpServers"]["ai-architect"]["args"], json!([]));
    }

    #[test]
    fn install_preserves_other_servers() {
        let existing = r#"{"mcpServers":{"other":{"command":"/bin/other","args":["--x"]}}}"#;
        let out = merge_install(Some(existing), &spec(), Format::JsonMcpServers);
        let content = match out {
            MergeOutcome::Write(c) => c,
            other => panic!("expected Write, got {other:?}"),
        };
        let v: Value = serde_json::from_str(&content).unwrap();
        // The other server survives, deep-equal.
        assert_eq!(
            v["mcpServers"]["other"],
            json!({"command":"/bin/other","args":["--x"]})
        );
        // Ours is added.
        assert_eq!(
            v["mcpServers"]["ai-architect"]["command"],
            json!(spec().command)
        );
    }

    #[test]
    fn install_is_idempotent() {
        let out1 = merge_install(None, &spec(), Format::JsonMcpServers);
        let content = match out1 {
            MergeOutcome::Write(c) => c,
            _ => panic!(),
        };
        let out2 = merge_install(Some(&content), &spec(), Format::JsonMcpServers);
        assert_eq!(
            out2,
            MergeOutcome::NoChange,
            "second install must be a no-op"
        );
    }

    #[test]
    fn vscode_entry_declares_stdio_under_servers() {
        let out = merge_install(None, &spec(), Format::JsonServersVscode);
        let content = match out {
            MergeOutcome::Write(c) => c,
            _ => panic!(),
        };
        let v: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["servers"]["ai-architect"]["type"], json!("stdio"));
    }

    #[test]
    fn zed_jsonc_with_comments_is_refused_not_clobbered() {
        let jsonc = "{\n  // my Zed settings\n  \"theme\": \"dark\",\n  \"context_servers\": {}\n}";
        let out = merge_install(Some(jsonc), &spec(), Format::JsonContextServersZed);
        match out {
            MergeOutcome::Refuse(msg) => {
                assert!(
                    msg.contains("context_servers"),
                    "guidance must name the key"
                );
                assert!(
                    msg.contains("ai-architect"),
                    "guidance must include our entry"
                );
            }
            other => panic!("JSONC must be refused, got {other:?}"),
        }
    }

    #[test]
    fn codex_toml_preserves_comments_and_other_servers() {
        let existing = "# my codex config\nmodel = \"gpt-5\"\n\n\
                        [mcp_servers.other]\ncommand = \"/bin/other\"\nargs = []\n";
        let out = merge_install(Some(existing), &spec(), Format::TomlCodex);
        let content = match out {
            MergeOutcome::Write(c) => c,
            other => panic!("expected Write, got {other:?}"),
        };
        // Comment + other server + our new table all present.
        assert!(
            content.contains("# my codex config"),
            "comment must survive"
        );
        assert!(
            content.contains("[mcp_servers.other]"),
            "other server must survive"
        );
        assert!(
            content.contains("[mcp_servers.ai-architect]"),
            "ours must be added"
        );
        assert!(content.contains(&format!("command = \"{}\"", spec().command)));
        // Idempotent.
        assert_eq!(
            merge_install(Some(&content), &spec(), Format::TomlCodex),
            MergeOutcome::NoChange
        );
    }

    #[test]
    fn uninstall_removes_only_our_entry() {
        let existing = r#"{"mcpServers":{"other":{"command":"/bin/other","args":[]},"ai-architect":{"command":"/x","args":[]}}}"#;
        let out = merge_uninstall(existing, "ai-architect", Format::JsonMcpServers);
        let content = match out {
            MergeOutcome::Write(c) => c,
            other => panic!("expected Write, got {other:?}"),
        };
        let v: Value = serde_json::from_str(&content).unwrap();
        assert!(v["mcpServers"]["ai-architect"].is_null(), "ours removed");
        assert_eq!(
            v["mcpServers"]["other"]["command"],
            json!("/bin/other"),
            "other kept"
        );
    }

    #[test]
    fn uninstall_absent_entry_is_no_change() {
        let existing = r#"{"mcpServers":{"other":{"command":"/bin/other","args":[]}}}"#;
        assert_eq!(
            merge_uninstall(existing, "ai-architect", Format::JsonMcpServers),
            MergeOutcome::NoChange
        );
    }

    #[test]
    fn unparseable_json_is_refused_never_clobbered() {
        let out = merge_install(Some("{ this is not json"), &spec(), Format::JsonMcpServers);
        assert!(
            matches!(out, MergeOutcome::Refuse(_)),
            "must refuse, not overwrite"
        );
    }

    #[test]
    fn hook_install_adds_block_and_preserves_other_hooks() {
        let existing = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"/other/thing"}]}]}}"#;
        let out = merge_hook_install(Some(existing), "/bin/ap hook-augment");
        let content = match out {
            MergeOutcome::Write(c) => c,
            other => panic!("expected Write, got {other:?}"),
        };
        let v: Value = serde_json::from_str(&content).unwrap();
        let arr = v["hooks"]["PreToolUse"].as_array().unwrap();
        // The pre-existing Bash hook survives.
        assert!(arr.iter().any(|b| b["matcher"] == "Bash"));
        // Ours is added with the Grep|Glob matcher.
        assert!(arr.iter().any(|b| b["matcher"] == HOOK_MATCHER
            && b["hooks"][0]["command"] == json!("/bin/ap hook-augment")));
        // Idempotent.
        assert_eq!(
            merge_hook_install(Some(&content), "/bin/ap hook-augment"),
            MergeOutcome::NoChange
        );
    }

    #[test]
    fn hook_uninstall_removes_only_ours() {
        let existing = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"/other"}]},{"matcher":"Grep|Glob","hooks":[{"type":"command","command":"/bin/ap hook-augment"}]}]}}"#;
        let out = merge_hook_uninstall(existing);
        let content = match out {
            MergeOutcome::Write(c) => c,
            other => panic!("expected Write, got {other:?}"),
        };
        let v: Value = serde_json::from_str(&content).unwrap();
        let arr = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "only the Bash hook remains");
        assert_eq!(arr[0]["matcher"], json!("Bash"));
    }
}
