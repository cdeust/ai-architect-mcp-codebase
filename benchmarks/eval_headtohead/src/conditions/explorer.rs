//! EXPLORER condition — the Grep/Glob/Read baseline, run competently.
//!
//! Operationalization (pre-registered, §2): Glob the corpus (1 call), Grep each
//! keyword as a case-insensitive substring (1 unioned call) producing a
//! `path:lineno:line` transcript, then Read the full contents of every distinct
//! file that had a hit. The retrieved-file set is exactly `rg -i <kw> -l`. No
//! magic post-read filter — that judgement is what the blinded judge assesses.

use super::{visit_files, Answer};
use crate::questions::Question;
use std::collections::BTreeSet;
use std::path::Path;

/// Run the baseline for one question over its language corpus.
pub fn run(q: &Question, corpus_dir: &Path) -> Answer {
    let keywords: Vec<String> = q
        .explorer_keywords
        .iter()
        .map(|k| k.to_lowercase())
        .collect();

    let mut transcript = String::new();
    let mut hit_files: BTreeSet<String> = BTreeSet::new();
    let mut contents: Vec<(String, String)> = Vec::new();

    visit_files(corpus_dir, &mut |rel, content| {
        let mut file_hit = false;
        for (i, line) in content.lines().enumerate() {
            let line_lc = line.to_lowercase();
            if keywords.iter().any(|kw| line_lc.contains(kw)) {
                transcript.push_str(&format!("{rel}:{}:{line}\n", i + 1));
                file_hit = true;
            }
        }
        if file_hit {
            hit_files.insert(rel.to_string());
            contents.push((rel.to_string(), content.to_string()));
        }
    });

    // Read cost: the full contents of every distinct hit file.
    let read_chars: usize = contents.iter().map(|(_, c)| c.len()).sum();
    let chars = transcript.len() + read_chars;

    // Tool calls: 1 Glob + 1 Grep (unioned keywords) + one Read per hit file.
    let tool_calls = 2 + hit_files.len();

    let text = build_text(&transcript, &contents);

    Answer {
        files: hit_files,
        chars,
        tool_calls,
        text,
    }
}

/// Judge-readable rendering: the grep transcript followed by the read files.
fn build_text(transcript: &str, contents: &[(String, String)]) -> String {
    let mut text = String::from("# grep transcript\n");
    text.push_str(transcript);
    for (rel, content) in contents {
        text.push_str(&format!("\n# read {rel}\n{content}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::questions::GraphOp;

    fn question(keywords: &[&str]) -> Question {
        Question {
            id: "t".into(),
            language: "python".into(),
            dimension: "D5".into(),
            question: "q".into(),
            graph_op: GraphOp::Search,
            target: "config".into(),
            explorer_keywords: keywords.iter().map(|s| s.to_string()).collect(),
            ground_truth_files: vec![],
        }
    }

    fn corpus() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\napply_config()\n").unwrap();
        std::fs::write(dir.path().join("b.py"), "class OrderConfig: pass\n").unwrap();
        std::fs::write(dir.path().join("c.py"), "unrelated = True\n").unwrap();
        dir
    }

    #[test]
    fn case_insensitive_substring_hits_two_of_three_files() {
        let dir = corpus();
        let ans = run(&question(&["config"]), dir.path());
        // "config" (case-insensitive) hits apply_config (a.py) and OrderConfig
        // (b.py); c.py has no match.
        assert_eq!(
            ans.files.iter().cloned().collect::<Vec<_>>(),
            vec!["a.py".to_string(), "b.py".to_string()]
        );
    }

    #[test]
    fn tool_calls_are_glob_plus_grep_plus_one_read_per_hit_file() {
        let dir = corpus();
        let ans = run(&question(&["config"]), dir.path());
        assert_eq!(ans.tool_calls, 2 + 2); // glob + grep + 2 reads
        assert!(ans.chars > 0);
    }

    #[test]
    fn transcript_reports_one_based_line_numbers_and_reads_files() {
        let dir = corpus();
        let ans = run(&question(&["config"]), dir.path());
        // b.py line 1 is `class OrderConfig`; the transcript must cite it as
        // line 1 (1-based). This pins `i + 1` and the transcript rendering.
        assert!(
            ans.text.contains("b.py:1:class OrderConfig"),
            "text was: {}",
            ans.text
        );
        // And the read of the matched file is included in the judge text.
        assert!(ans.text.contains("# read b.py"));
    }

    #[test]
    fn no_hit_yields_empty_answer_and_two_tool_calls() {
        let dir = corpus();
        let ans = run(&question(&["absent_keyword"]), dir.path());
        assert!(ans.files.is_empty());
        assert_eq!(ans.tool_calls, 2); // glob + grep, no reads
    }

    #[test]
    fn chars_are_the_additive_sum_of_transcript_and_reads() {
        // Minimal single-file corpus with exact sizes so the char accounting is
        // pinned to the SUM (transcript + reads), not a product or difference.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("only.py"), "config\n").unwrap(); // 7 bytes
        let ans = run(&question(&["config"]), dir.path());
        // transcript: "only.py:1:config\n" = 17 chars; read: 7 chars ⇒ 24.
        assert_eq!(ans.chars, 24);
        // 1 hit file ⇒ glob + grep + 1 read = 3 (pins `2 + n`, not `2 * n`).
        assert_eq!(ans.tool_calls, 3);
    }
}
