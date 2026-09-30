// binary.rs — decide which MCP server binary the bench measures, and never a
// stale one.
//
// `cargo run -p bench-end-result` compiles only the bench crate. The server
// binary it spawns is a different package, so before issue #397 the runner
// happily measured whatever `target/release/ai-architect-mcp-codebase` was
// left over from an earlier build and reported it as the current tree.
// Cargo already knows exactly when a build is up to date (its fingerprints),
// so the default path asks it to build instead of comparing mtimes, which a
// `git checkout`, a tar export or a copied tree all defeat.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Package and binary name of the MCP server (root crate).
const SERVER_PACKAGE: &str = "ai-architect-mcp-codebase";

/// Arguments of the build that makes `target/release/<server>` current.
fn release_build_args() -> [&'static str; 6] {
    [
        "build",
        "--release",
        "-p",
        SERVER_PACKAGE,
        "--bin",
        SERVER_PACKAGE,
    ]
}

/// The cargo target directory: `CARGO_TARGET_DIR` when set (made absolute
/// against the current directory, as cargo does), else `<repo_root>/target`.
fn target_dir(repo_root: &Path, env_value: Option<OsString>) -> PathBuf {
    match env_value.filter(|v| !v.is_empty()).map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        Some(dir) => std::env::current_dir()
            .map(|cwd| cwd.join(&dir))
            .unwrap_or(dir),
        None => repo_root.join("target"),
    }
}

/// Run `cargo build --release` for the server in `repo_root`.
///
/// precondition:  `cargo` is a cargo executable; `repo_root` is the workspace root.
/// postcondition: `Ok(())` iff cargo exited 0, i.e. the release binary is
///                current for the sources on disk. Cargo's progress goes to
///                stderr; stdout is not touched (it carries the JSON summary).
fn build_release_binary(cargo: &OsStr, repo_root: &Path) -> Result<(), String> {
    let status = Command::new(cargo)
        .args(release_build_args())
        .current_dir(repo_root)
        .stdout(Stdio::null())
        .status()
        .map_err(|e| format!("spawn {cargo:?} to build the server binary: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "`cargo build --release` of the server failed ({status}); refusing to measure \
             a possibly stale binary"
        ))
    }
}

/// The binary to measure.
///
/// - `explicit` (`--binary`): used as given; the caller owns its freshness.
/// - otherwise: rebuilt via cargo first, and a failed build is an error, never
///   a silent fall back to whatever file is already on disk.
///
/// postcondition: on `Ok(p)`, `p` exists; without `explicit` it was produced or
///                confirmed current by a successful cargo build in this call.
pub fn resolve_binary(
    explicit: Option<PathBuf>,
    repo_root: &Path,
    cargo: &OsStr,
    target_dir_env: Option<OsString>,
) -> Result<PathBuf, String> {
    let (binary, must_build) = match explicit {
        Some(path) => (path, false),
        None => (
            target_dir(repo_root, target_dir_env)
                .join("release")
                .join(SERVER_PACKAGE),
            true,
        ),
    };
    if must_build {
        build_release_binary(cargo, repo_root)?;
    }
    if binary.exists() {
        Ok(binary)
    } else {
        Err(format!("MCP binary not found at {binary:?}"))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const SERVER_PACKAGE_REL: &str = "target/release/ai-architect-mcp-codebase";

    /// A fake `cargo` (shell script) that runs `body` in the invocation's cwd.
    fn fake_cargo(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-cargo");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn server_path(root: &Path) -> PathBuf {
        let p = root.join("target/release").join(SERVER_PACKAGE);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        p
    }

    #[test]
    fn the_build_asks_cargo_for_the_release_server_binary() {
        assert_eq!(
            release_build_args().join(" "),
            "build --release -p ai-architect-mcp-codebase --bin ai-architect-mcp-codebase"
        );
    }

    #[test]
    fn a_stale_binary_on_disk_is_replaced_by_the_build_before_it_is_used() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = server_path(tmp.path());
        std::fs::write(&bin, "stale").unwrap();
        let cargo = fake_cargo(tmp.path(), &format!("echo fresh > {SERVER_PACKAGE_REL}"));
        let got = resolve_binary(None, tmp.path(), cargo.as_os_str(), None).unwrap();
        assert_eq!(got, bin);
        assert_eq!(std::fs::read_to_string(&bin).unwrap().trim(), "fresh");
    }

    #[test]
    fn a_failed_build_is_refused_even_though_an_old_binary_exists() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(server_path(tmp.path()), "stale").unwrap();
        let cargo = fake_cargo(tmp.path(), "exit 101");
        let err = resolve_binary(None, tmp.path(), cargo.as_os_str(), None).unwrap_err();
        assert!(err.contains("refusing to measure"), "{err}");
    }

    #[test]
    fn an_explicit_binary_is_used_as_given_without_building() {
        let tmp = tempfile::tempdir().unwrap();
        let mine = tmp.path().join("mine");
        std::fs::write(&mine, "x").unwrap();
        // A cargo that would fail proves it is not invoked.
        let cargo = fake_cargo(tmp.path(), "exit 1");
        let got = resolve_binary(Some(mine.clone()), tmp.path(), cargo.as_os_str(), None).unwrap();
        assert_eq!(got, mine);
    }

    #[test]
    fn a_missing_binary_after_a_successful_build_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cargo = fake_cargo(tmp.path(), "exit 0");
        let err = resolve_binary(None, tmp.path(), cargo.as_os_str(), None).unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn cargo_target_dir_moves_where_the_binary_is_looked_up() {
        let root = Path::new("/repo");
        assert_eq!(target_dir(root, None), PathBuf::from("/repo/target"));
        assert_eq!(
            target_dir(root, Some(OsString::from("/elsewhere"))),
            PathBuf::from("/elsewhere")
        );
        assert_eq!(
            target_dir(root, Some(OsString::new())),
            PathBuf::from("/repo/target")
        );
    }
}
