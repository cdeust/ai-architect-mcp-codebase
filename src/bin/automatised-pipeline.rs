//! Compatibility launcher for the pre-0.9 executable name.

use std::process::{self, Command};

fn main() {
    let mut canonical = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("automatised-pipeline: cannot locate executable: {error}");
            process::exit(1);
        }
    };
    canonical.set_file_name(format!(
        "ai-architect-mcp-codebase{}",
        std::env::consts::EXE_SUFFIX
    ));

    match Command::new(&canonical)
        .args(std::env::args_os().skip(1))
        .status()
    {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!(
                "automatised-pipeline: compatibility launcher could not start {}: {error}",
                canonical.display()
            );
            process::exit(1);
        }
    }
}
