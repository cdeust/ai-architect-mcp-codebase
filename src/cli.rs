// cli — the `install` / `uninstall` / `hook-augment` subcommands (issue #59).
//
// Layer: composition root. Resolves each host's config path from the
// environment, reads the existing file, delegates the never-clobber merge to
// `host_install` (the pure, unit-tested core), and writes the result — honoring
// `--dry-run`, `--only`, `--skip`, and `--with-hooks`. The end-to-end behavior
// is pinned by a subprocess integration test with fixture configs.

use crate::hook_augment;
use crate::host_install::{self, Format, MergeOutcome, ServerSpec};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The MCP server key written into every host config.
const SERVER_NAME: &str = "ai-architect";

/// One host's resolved config location + dialect.
struct HostConfig {
    id: &'static str,
    label: &'static str,
    config_path: PathBuf,
    detected: bool,
    format: Format,
}

/// Parsed install/uninstall flags.
struct Opts {
    dry_run: bool,
    with_hooks: bool,
    only: Vec<String>,
    skip: Vec<String>,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        dry_run: false,
        with_hooks: false,
        only: Vec::new(),
        skip: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => o.dry_run = true,
            "--with-hooks" => o.with_hooks = true,
            "--only" => {
                i += 1;
                o.only
                    .push(args.get(i).cloned().ok_or("--only needs a host id")?);
            }
            "--skip" => {
                i += 1;
                o.skip
                    .push(args.get(i).cloned().ok_or("--skip needs a host id")?);
            }
            other => return Err(format!("unknown flag '{other}'")),
        }
        i += 1;
    }
    Ok(o)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Resolves every supported host's config path + detection from the environment.
fn resolve_hosts() -> Vec<HostConfig> {
    let home = match home() {
        Some(h) => h,
        None => return Vec::new(),
    };
    let exists = |p: &Path| p.exists();

    // Claude Code — ~/.claude.json (honors $CLAUDE_CONFIG_DIR for the dir).
    let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from);
    let claude_file = claude_dir
        .as_ref()
        .map(|d| d.join(".claude.json"))
        .unwrap_or_else(|| home.join(".claude.json"));
    let claude_detected = exists(&claude_file) || exists(&home.join(".claude"));

    // Codex — $CODEX_HOME/config.toml or ~/.codex/config.toml.
    let codex_dir = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let codex_file = codex_dir.join("config.toml");

    // Gemini CLI — ~/.gemini/settings.json.
    let gemini_dir = home.join(".gemini");
    let gemini_file = gemini_dir.join("settings.json");

    // Cursor — ~/.cursor/mcp.json.
    let cursor_dir = home.join(".cursor");
    let cursor_file = cursor_dir.join("mcp.json");

    // VS Code — platform user dir / mcp.json.
    let vscode_user = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Code/User")
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Code/User")
    } else {
        home.join(".config/Code/User")
    };
    let vscode_file = vscode_user.join("mcp.json");

    // Zed — ~/.config/zed/settings.json.
    let zed_dir = home.join(".config/zed");
    let zed_file = zed_dir.join("settings.json");

    vec![
        HostConfig {
            id: "claude",
            label: "Claude Code",
            detected: claude_detected,
            config_path: claude_file,
            format: Format::JsonMcpServers,
        },
        HostConfig {
            id: "codex",
            label: "Codex CLI",
            detected: exists(&codex_dir),
            config_path: codex_file,
            format: Format::TomlCodex,
        },
        HostConfig {
            id: "gemini",
            label: "Gemini CLI",
            detected: exists(&gemini_dir),
            config_path: gemini_file,
            format: Format::JsonMcpServers,
        },
        HostConfig {
            id: "cursor",
            label: "Cursor",
            detected: exists(&cursor_dir),
            config_path: cursor_file,
            format: Format::JsonMcpServers,
        },
        HostConfig {
            id: "vscode",
            label: "VS Code",
            detected: exists(&vscode_user),
            config_path: vscode_file,
            format: Format::JsonServersVscode,
        },
        HostConfig {
            id: "zed",
            label: "Zed",
            detected: exists(&zed_dir),
            config_path: zed_file,
            format: Format::JsonContextServersZed,
        },
    ]
}

/// Whether a host is selected given `--only`/`--skip`. `--only` forces the host
/// even when undetected (the user is setting it up); otherwise only detected
/// hosts not in `--skip` are selected.
fn selected(host: &HostConfig, opts: &Opts) -> bool {
    if !opts.only.is_empty() {
        return opts.only.iter().any(|o| o == host.id);
    }
    host.detected && !opts.skip.iter().any(|s| s == host.id)
}

/// Absolute path to this running binary (the command hosts will launch).
fn binary_command() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok().or(Some(p)))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ai-architect-mcp-codebase".to_string())
}

/// Applies a merge outcome to `path` (writing unless `dry_run`) and returns a
/// one-line human report.
fn apply(path: &Path, outcome: MergeOutcome, dry_run: bool, label: &str) -> String {
    match outcome {
        MergeOutcome::NoChange => format!("  {label}: already configured (no change)"),
        MergeOutcome::Refuse(msg) => format!("  {label}: MANUAL STEP — {msg}"),
        MergeOutcome::Write(content) => {
            if dry_run {
                format!(
                    "  {label}: [dry-run] would write {}:\n----\n{content}----",
                    path.display()
                )
            } else {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(path, content) {
                    Ok(()) => format!("  {label}: configured {}", path.display()),
                    Err(e) => format!("  {label}: ERROR writing {}: {e}", path.display()),
                }
            }
        }
    }
}

/// `install` subcommand. Returns the process exit code (always 0 — a per-host
/// failure is reported, not fatal).
pub fn run_install(args: &[String]) -> i32 {
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("install: {e}");
            return 2;
        }
    };
    let command = binary_command();
    let spec = ServerSpec {
        name: SERVER_NAME.to_string(),
        command: command.clone(),
        args: Vec::new(),
    };
    println!(
        "ai-architect install{} (binary: {command})",
        if opts.dry_run { " [dry-run]" } else { "" }
    );
    let mut any = false;
    for host in resolve_hosts() {
        if !selected(&host, &opts) {
            if opts.only.is_empty() && !host.detected {
                println!(
                    "  {}: not detected, skipped (use --only {})",
                    host.label, host.id
                );
            }
            continue;
        }
        any = true;
        let existing = std::fs::read_to_string(&host.config_path).ok();
        let outcome = host_install::merge_install(existing.as_deref(), &spec, host.format);
        println!(
            "{}",
            apply(&host.config_path, outcome, opts.dry_run, host.label)
        );
    }
    if !any {
        println!("  (no hosts selected — nothing to do)");
    }

    if opts.with_hooks {
        if let Some(home) = home() {
            let settings = home.join(".claude/settings.json");
            let hook_command = format!("{command} hook-augment");
            let existing = std::fs::read_to_string(&settings).ok();
            let outcome = host_install::merge_hook_install(existing.as_deref(), &hook_command);
            println!(
                "{}",
                apply(
                    &settings,
                    outcome,
                    opts.dry_run,
                    "Claude Code PreToolUse hook"
                )
            );
        }
    } else {
        println!(
            "  (hook not registered — pass --with-hooks to add the Grep/Glob PreToolUse hook)"
        );
    }
    0
}

/// `uninstall` subcommand — removes exactly our entries. Returns 0.
pub fn run_uninstall(args: &[String]) -> i32 {
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("uninstall: {e}");
            return 2;
        }
    };
    println!(
        "ai-architect uninstall{}",
        if opts.dry_run { " [dry-run]" } else { "" }
    );
    for host in resolve_hosts() {
        // Uninstall targets any host with a config file, honoring only/skip.
        if !opts.only.is_empty() && !opts.only.iter().any(|o| o == host.id) {
            continue;
        }
        if opts.skip.iter().any(|s| s == host.id) {
            continue;
        }
        let existing = match std::fs::read_to_string(&host.config_path) {
            Ok(c) => c,
            Err(_) => continue, // no config → nothing of ours to remove
        };
        let outcome = host_install::merge_uninstall(&existing, SERVER_NAME, host.format);
        println!(
            "{}",
            apply(&host.config_path, outcome, opts.dry_run, host.label)
        );
    }
    // Always attempt hook removal from Claude settings.
    if let Some(home) = home() {
        let settings = home.join(".claude/settings.json");
        if let Ok(existing) = std::fs::read_to_string(&settings) {
            let outcome = host_install::merge_hook_uninstall(&existing);
            println!(
                "{}",
                apply(
                    &settings,
                    outcome,
                    opts.dry_run,
                    "Claude Code PreToolUse hook"
                )
            );
        }
    }
    0
}

/// `hook-augment` subcommand — reads a PreToolUse payload on stdin and prints the
/// suggestion, or NOTHING. ALWAYS exits 0 (the cardinal fail-open rule).
pub fn run_hook_augment() -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0; // can't read → say nothing, never block
    }
    if let Some(out) = hook_augment::build_suggestion(&input, hook_augment::graph_present_fs) {
        println!("{out}");
    }
    0
}
