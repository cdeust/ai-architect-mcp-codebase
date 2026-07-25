// Dockerfile parsing — line-oriented, no dependencies.
//
// Mirrors DeusData/codebase-memory-mcp src/pipeline/pass_infrascan.c
// (`cbm_parse_dockerfile_source`): a Dockerfile is a sequence of instructions,
// one per logical line, each an uppercase keyword followed by arguments. We
// extract exactly the deployment-surface facts issue #63 criterion 1 names —
// base image, build stages, exposed ports, entrypoint/cmd, workdir, and local
// COPY/ADD sources — and nothing else. A full Dockerfile grammar (line
// continuations, heredocs, --mount flags) is deliberately out of scope: the
// unhandled forms degrade to "field absent", never to a crash, and the file is
// still a File node with an IacResource attached.
//
// source: issue #63 criterion 1; field set and per-list caps mirror
// pass_infrascan.c (IS_MAX_STAGES=16, IS_MAX_PORTS=16, IS_MAX_SOURCES=16).

/// Upper bound on each extracted list. A real Dockerfile has a handful of
/// stages/ports/sources; the cap bounds work on adversarial input (issue #63
/// criterion 6: extraction is bounded, not assumed-small). source: pass_infrascan.c.
const MAX_STAGES: usize = 16;
const MAX_PORTS: usize = 16;
const MAX_SOURCES: usize = 16;

/// The deployment-surface facts extracted from one Dockerfile.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DockerfileInfo {
    /// The final stage's image — what actually ships. Empty only if no `FROM`.
    pub base_image: String,
    /// Every `FROM` image in file order (multi-stage builds have several).
    pub stage_images: Vec<String>,
    /// `EXPOSE`d ports, protocol suffix (`/tcp`) stripped.
    pub exposed_ports: Vec<String>,
    /// `ENTRYPOINT` if present, else `CMD`, JSON-array brackets stripped.
    pub entrypoint: String,
    /// `WORKDIR`.
    pub workdir: String,
    /// Local (non-URL, non-`--from`) `COPY`/`ADD` source paths.
    pub copy_sources: Vec<String>,
}

/// Parses Dockerfile `source` into its deployment-surface facts.
///
/// Precondition: `source` is the full UTF-8 text of a Dockerfile.
/// Postcondition: returns `Some(info)` iff at least one `FROM` instruction was
/// found (the minimum for a buildable image); `info.base_image` is the last
/// stage's image and every list is capped at its `MAX_*` bound. Returns `None`
/// for a file with no `FROM` — the caller records that as a parse gap, never a
/// crash (issue #63 criterion 4).
pub fn parse(source: &str) -> Option<DockerfileInfo> {
    let mut info = DockerfileInfo::default();
    let mut have_entrypoint = false; // ENTRYPOINT wins over CMD

    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (keyword, rest) = split_keyword(line);
        // Keyword match is case-insensitive per the Dockerfile spec.
        match keyword.to_ascii_uppercase().as_str() {
            "FROM" => parse_from(rest, &mut info),
            "EXPOSE" => parse_expose(rest, &mut info),
            "WORKDIR" => info.workdir = rest.trim().to_string(),
            "ENTRYPOINT" => {
                info.entrypoint = clean_exec(rest);
                have_entrypoint = true;
            }
            "CMD" if !have_entrypoint => info.entrypoint = clean_exec(rest),
            "COPY" | "ADD" => parse_copy(rest, &mut info),
            _ => {}
        }
    }

    if info.stage_images.is_empty() {
        return None;
    }
    // base_image = the last stage's image (what the final container runs).
    info.base_image = info.stage_images.last().cloned().unwrap_or_default();
    Some(info)
}

/// Splits a trimmed instruction line into (keyword, remainder). The keyword is
/// the leading run of non-whitespace; the remainder is everything after the
/// first whitespace run (may be empty).
fn split_keyword(line: &str) -> (&str, &str) {
    match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], line[i..].trim_start()),
        None => (line, ""),
    }
}

/// `FROM <image> [AS <stage>]` — records the image (ignoring the stage alias,
/// which names an intra-file stage, not a shipped image). A `FROM <prev-stage>`
/// that references an earlier `AS` name is still recorded verbatim; the image
/// node dedup layer treats it as a reference like any other.
fn parse_from(rest: &str, info: &mut DockerfileInfo) {
    if info.stage_images.len() >= MAX_STAGES {
        return;
    }
    if let Some(image) = rest.split_whitespace().next() {
        info.stage_images.push(image.to_string());
    }
}

/// `EXPOSE <port>[/proto] ...` — one entry per space-separated port, protocol
/// suffix stripped (mirrors pass_infrascan.c df_parse_expose).
fn parse_expose(rest: &str, info: &mut DockerfileInfo) {
    for tok in rest.split_whitespace() {
        if info.exposed_ports.len() >= MAX_PORTS {
            return;
        }
        let port = tok.split('/').next().unwrap_or(tok);
        if !port.is_empty() {
            info.exposed_ports.push(port.to_string());
        }
    }
}

/// `COPY [--flags] <src>... <dest>` / `ADD ...` — records the local source
/// paths (all tokens except the last, which is the destination). Tokens that
/// are flags (`--from=`, `--chown=`), URLs, or `--from` stage refs are dropped:
/// only paths that could resolve to an indexed File are useful downstream.
fn parse_copy(rest: &str, info: &mut DockerfileInfo) {
    // A `COPY --from=stage ...` copies from another build stage, not the source
    // tree — skip its sources (they are not repo files).
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.iter().any(|t| t.starts_with("--from")) {
        return;
    }
    let sources = tokens
        .iter()
        .filter(|t| !t.starts_with("--")) // drop flags
        .copied()
        .collect::<Vec<_>>();
    // The last token is the destination; everything before it is a source.
    if sources.len() < 2 {
        return;
    }
    for src in &sources[..sources.len() - 1] {
        if info.copy_sources.len() >= MAX_SOURCES {
            return;
        }
        if src.starts_with("http://") || src.starts_with("https://") {
            continue;
        }
        info.copy_sources.push((*src).to_string());
    }
}

/// Strips a JSON-exec-form array to a plain command string:
/// `["./app", "--flag"]` → `./app --flag`. A shell-form value passes through
/// trimmed. Mirrors pass_infrascan.c cbm_clean_json_brackets.
fn clean_exec(rest: &str) -> String {
    let t = rest.trim();
    if t.starts_with('[') && t.ends_with(']') {
        let inner = &t[1..t.len() - 1];
        inner
            .split(',')
            .map(|part| part.trim().trim_matches('"').trim())
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_stage() {
        let df = "FROM node:18\nWORKDIR /app\nCOPY . /app\nEXPOSE 8080/tcp 9090\nCMD [\"node\", \"server.js\"]\n";
        let info = parse(df).expect("valid dockerfile");
        assert_eq!(info.base_image, "node:18");
        assert_eq!(info.stage_images, vec!["node:18"]);
        assert_eq!(info.exposed_ports, vec!["8080", "9090"]);
        assert_eq!(info.workdir, "/app");
        assert_eq!(info.entrypoint, "node server.js");
        assert_eq!(info.copy_sources, vec!["."]);
    }

    #[test]
    fn multistage_base_is_last_stage() {
        let df = "FROM golang:1.22 AS build\nCOPY . /src\nFROM alpine:3.19\nCOPY --from=build /src/app /app\nENTRYPOINT [\"/app\"]\n";
        let info = parse(df).expect("valid dockerfile");
        assert_eq!(info.stage_images, vec!["golang:1.22", "alpine:3.19"]);
        assert_eq!(info.base_image, "alpine:3.19");
        // `COPY --from=build` sources are NOT repo files — dropped.
        assert_eq!(info.copy_sources, vec!["."]);
        assert_eq!(info.entrypoint, "/app");
    }

    #[test]
    fn entrypoint_wins_over_cmd() {
        let df = "FROM x\nCMD [\"a\"]\nENTRYPOINT [\"b\"]\n";
        assert_eq!(parse(df).unwrap().entrypoint, "b");
    }

    #[test]
    fn no_from_is_none() {
        assert!(parse("# just a comment\nRUN echo hi\n").is_none());
    }

    #[test]
    fn case_insensitive_keywords() {
        let df = "from ubuntu:22.04\nexpose 80\n";
        let info = parse(df).unwrap();
        assert_eq!(info.base_image, "ubuntu:22.04");
        assert_eq!(info.exposed_ports, vec!["80"]);
    }

    #[test]
    fn cmd_after_entrypoint_does_not_override_it() {
        // ENTRYPOINT wins even when CMD comes AFTER it — the `!have_entrypoint`
        // guard must suppress the later CMD. (The reverse order is covered by
        // `entrypoint_wins_over_cmd`.)
        let df = "FROM x\nENTRYPOINT [\"b\"]\nCMD [\"a\"]\n";
        assert_eq!(parse(df).unwrap().entrypoint, "b");
    }

    #[test]
    fn copy_with_many_sources_keeps_all_but_the_destination() {
        // `COPY a b c /dest` has three sources and one destination — all three
        // sources are kept (guards the `sources.len() < 2` and the source/dest
        // split).
        let df = "FROM x\nCOPY a.txt b.txt c.txt /dest/\n";
        assert_eq!(
            parse(df).unwrap().copy_sources,
            vec!["a.txt", "b.txt", "c.txt"]
        );
    }

    #[test]
    fn copy_url_sources_are_not_local_files() {
        // A remote COPY source (http/https) is NOT a repo file and must be
        // dropped (guards the URL filter's `||`).
        let df = "FROM x\nCOPY https://example.com/pkg.tar /opt/pkg.tar\n";
        assert!(parse(df).unwrap().copy_sources.is_empty());
    }

    #[test]
    fn malformed_exec_bracket_is_left_verbatim() {
        // A value that opens `[` but never closes `]` is NOT valid JSON-exec
        // form and must pass through unchanged, not be half-stripped (guards the
        // `&&` in clean_exec's bracket test).
        let df = "FROM x\nENTRYPOINT [unclosed\n";
        assert_eq!(parse(df).unwrap().entrypoint, "[unclosed");
    }
}
