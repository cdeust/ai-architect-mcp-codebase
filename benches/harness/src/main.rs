// bench_end_result — end-result evaluation harness for ai-architect-mcp.
//
// Spec: stages/stage-3b-v2.md §2. Black-box runner that spawns the release
// MCP binary, dispatches JSON-RPC tool calls, and scores the answers against
// hand-labeled ground truth. Produces the `end_result_score` number that
// defines "production-grade" per the user's rewrite.
//
// Usage:
//   cargo run --release --bin bench_end_result -- --corpus rust-self
//   cargo run --release --bin bench_end_result -- --all

use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;

mod corpora;
mod queries;
mod report;
mod runner;
mod scoring;

/// CLI surface. `--corpus NAME` runs a single corpus, `--all` runs every
/// corpus whose `ground_truth.json` has at least one label.
#[derive(Parser, Debug)]
#[command(name = "bench_end_result", about = "End-result evaluation harness")]
struct Cli {
    /// Name of a single corpus under `benches/corpora/` to run.
    #[arg(long)]
    corpus: Option<String>,

    /// Run every corpus that has non-empty labels.
    #[arg(long, default_value_t = false)]
    all: bool,

    /// Path to the MCP server binary. Defaults to
    /// `target/release/ai-architect-mcp-codebase`.
    #[arg(long)]
    binary: Option<PathBuf>,

    /// Path to the `benches/corpora` root. Defaults to `<repo_root>/benches/corpora`.
    #[arg(long)]
    corpora_root: Option<PathBuf>,

    /// Write the markdown summary to `benches/runs/<timestamp>.md`.
    #[arg(long, default_value_t = true)]
    write_report: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let repo_root = detect_repo_root();
    let binary = cli
        .binary
        .clone()
        .unwrap_or_else(|| repo_root.join("target/release/ai-architect-mcp-codebase"));
    let corpora_root = cli
        .corpora_root
        .clone()
        .unwrap_or_else(|| repo_root.join("benches/corpora"));

    if !binary.exists() {
        eprintln!(
            "error: MCP binary not found at {:?}.\n  \
             run `cargo build --release` first.",
            binary
        );
        return ExitCode::from(2);
    }

    let corpora = match select_corpora(&cli, &corpora_root) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    let mut runs = Vec::new();
    for corpus in &corpora {
        eprintln!(
            "[bench] running corpus={} lang={}",
            corpus.name, corpus.language
        );
        let run = runner::run_corpus(corpus, &binary);
        runs.push(run);
    }

    let summary = report::build_summary(&runs);
    println!("{}", report::to_json(&summary));
    eprintln!("{}", report::to_human(&summary));

    if cli.write_report {
        let md_path = repo_root.join("benches/runs");
        if let Err(e) = report::write_markdown(&md_path, &summary) {
            eprintln!("warn: failed to write markdown report: {e}");
        }
    }

    // Ground-truth staleness is a hard failure (issue #132): a corpus that
    // scores against deleted symbols is not measuring anything real, so it
    // must never report a passing verdict. Gate before the score verdict.
    if corpus_is_stale(&runs) {
        let stale_total = total_stale_references(&runs);
        eprintln!(
            "\nerror: {stale_total} ground-truth reference(s) point at deleted source \
             paths (see [STALE GROUND TRUTH] lines above). The corpus is stale — fix the \
             labels before trusting any score."
        );
        return ExitCode::from(3);
    }

    if summary.verdict_production_grade {
        ExitCode::SUCCESS
    } else {
        // Non-zero exit so CI gates on the 85% + floor.
        ExitCode::from(1)
    }
}

/// Total number of stale ground-truth references across every corpus run
/// (issue #132). A non-zero total means at least one corpus scored against a
/// deleted source path and the harness must fail hard.
fn total_stale_references(runs: &[runner::CorpusRun]) -> usize {
    runs.iter().map(|r| r.stale_ground_truth.len()).sum()
}

/// True iff any corpus scored against a deleted source path (issue #132) —
/// the hard-failure gate condition. Kept separate from `main` so the boundary
/// (`> 0`) is exercised by tests rather than only by a full harness run.
fn corpus_is_stale(runs: &[runner::CorpusRun]) -> bool {
    total_stale_references(runs) > 0
}

/// Walks up from CARGO_MANIFEST_DIR to find the repo root (the one with
/// `Cargo.toml` containing `[workspace]`). Falls back to two levels up.
fn detect_repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // benches/harness
                                                                  // benches/harness -> benches -> repo_root
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or(manifest_dir)
}

/// Resolves the CLI-selected corpus set. `--all` picks every corpus with
/// non-empty labels; `--corpus NAME` picks exactly one; neither is an error.
fn select_corpora(
    cli: &Cli,
    corpora_root: &std::path::Path,
) -> Result<Vec<corpora::CorpusConfig>, String> {
    if cli.all {
        return corpora::discover_all(corpora_root);
    }
    let name = cli
        .corpus
        .clone()
        .ok_or_else(|| "specify --corpus NAME or --all".to_string())?;
    let one = corpora::load_one(corpora_root, &name)?;
    Ok(vec![one])
}

#[cfg(test)]
mod tests {
    use super::*;
    use runner::CorpusRun;
    use std::collections::HashMap;

    fn run_with_stale(stale: Vec<String>) -> CorpusRun {
        CorpusRun {
            name: "c".into(),
            language: "rust".into(),
            per_query_scores: HashMap::new(),
            per_query_samples: HashMap::new(),
            per_query_elapsed_ms: HashMap::new(),
            end_result_score: 0.0,
            index_elapsed_ms: 0,
            labels_run: 0,
            labels_skipped: 0,
            setup_error: None,
            stale_ground_truth: stale,
        }
    }

    #[test]
    fn total_stale_is_zero_when_no_run_is_stale() {
        let runs = vec![run_with_stale(vec![]), run_with_stale(vec![])];
        assert_eq!(total_stale_references(&runs), 0);
    }

    #[test]
    fn total_stale_sums_across_runs() {
        let runs = vec![
            run_with_stale(vec!["a.rs".into(), "b.rs".into()]),
            run_with_stale(vec![]),
            run_with_stale(vec!["c.rs".into()]),
        ];
        assert_eq!(total_stale_references(&runs), 3);
    }

    #[test]
    fn corpus_is_stale_is_false_only_when_every_run_is_clean() {
        assert!(!corpus_is_stale(&[
            run_with_stale(vec![]),
            run_with_stale(vec![])
        ]));
        assert!(corpus_is_stale(&[
            run_with_stale(vec![]),
            run_with_stale(vec!["x.rs".into()]),
        ]));
    }
}
