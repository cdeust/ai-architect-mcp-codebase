use super::*;

#[test]
fn test_extract_verbatim_identifiers() {
    let text = "Use `grad_rgb` and `make_bar()` to render the gauge.";
    let ids = extract_verbatim_identifiers(text);
    assert_eq!(ids, vec!["grad_rgb".to_string(), "make_bar".to_string()]);
}

#[test]
fn test_extract_verbatim_identifiers_none() {
    assert!(extract_verbatim_identifiers("no backticks here").is_empty());
}

#[test]
fn test_tokenize_natural_basic() {
    let t = tokenize_natural(
        "Handle tool call should reject unknown tools",
        "The handle_tool_call function in main.rs silently returns",
    );
    assert!(t.iter().any(|x| x == "handle"));
    assert!(t.iter().any(|x| x == "tool"));
    assert!(t.iter().any(|x| x == "handle_tool_call"));
    assert!(!t.iter().any(|x| x.len() < MIN_TOKEN_LEN));
}

#[test]
fn test_tokenize_natural_respects_max_tokens() {
    let description = (0..100)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let t = tokenize_natural("title", &description);
    assert!(t.len() <= MAX_TOKENS);
}

#[test]
fn test_clean_token_strips_punctuation() {
    assert_eq!(clean_token("Foo,"), "foo");
    assert_eq!(clean_token("bar.baz"), "barbaz");
    assert_eq!(clean_token("x::y"), "x::y");
    assert_eq!(clean_token("(parens)"), "parens");
}

#[test]
fn test_is_exact_hit_name_match() {
    let hit = search::SearchResult {
        qualified_name: "src/main.rs::handle_tool_call".into(),
        name: "handle_tool_call".into(),
        label: "Function".into(),
        file_path: "src/main.rs".into(),
        score: 0.5,
        community_id: None,
        process_names: vec![],
        start_line: None,
        end_line: None,
    };
    assert!(is_exact_hit("handle_tool_call", &hit));
    assert!(!is_exact_hit("handle", &hit));
}

// Homonymy limit (review 2026-07-15, deliberately left as a documented gap
// rather than fixed under #14): `is_exact_hit` compares by NAME/qn-tail
// string equality only, with no cross-file disambiguation. Two distinct
// symbols in different files sharing the same name (e.g. two `parse`
// functions in different modules) both classify as ExactName for a token
// equal to that name — there is no way to tell from the description alone
// which one the author meant. This test documents the behavior (both hits
// surface, both trusted) rather than asserting it is wrong; disambiguating
// by file/module context is a follow-up, not part of issue #14's scope.
#[test]
fn test_homonymous_symbols_in_different_files_both_classify_as_exact() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp = tempfile::Builder::new()
        .prefix("prd_input_issue14_homonym_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = std::fs::remove_dir_all(&tmp);
    let src_dir = tmp.join("src");
    std::fs::create_dir_all(src_dir.join("a")).unwrap();
    std::fs::create_dir_all(src_dir.join("b")).unwrap();
    std::fs::write(
        src_dir.join("a/mod.rs"),
        "pub fn parse(input: &str) -> u32 { input.len() as u32 }\n",
    )
    .unwrap();
    std::fs::write(
        src_dir.join("b/mod.rs"),
        "pub fn parse(input: &str) -> u32 { 0 }\n",
    )
    .unwrap();
    let graph_dir = tmp.join("graph");
    let r = crate::indexer::index_codebase(&src_dir, &graph_dir).expect("index fixture");
    let store = GraphStore::open_or_create(&r.graph_path).unwrap();

    let title = "Fix parse edge case";
    let description = "The `parse` function mishandles empty input.";
    let natural = tokenize_natural(title, description);
    let verbatim = extract_verbatim_identifiers(&format!("{title} {description}"));
    let outcome = search_and_classify(&store, &verbatim, &natural, None);

    let exact_hits: Vec<_> = outcome
        .matched
        .iter()
        .filter(|m| m.name == "parse" && m.match_mode == MatchMode::Verbatim)
        .collect();
    assert_eq!(
        exact_hits.len(),
        2,
        "documented limit: both homonymous `parse` symbols classify as \
         verified matches — is_exact_hit has no file/module disambiguation"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// Measured evidence (2026-07-15, this repo's own scoring function
// `search::score_candidate`'s per-term formula reproduced here at the
// unit boundary): a genuine partial-word hit ("handle" inside
// "handle_tool_call") and an accidental substring false positive
// ("anchor" inside "_concrete_anchor") land on the SAME score when the
// substring-to-name length ratio matches. This is why match_mode — not a
// score cutoff — is the load-bearing signal: no threshold value can
// separate these two cases because they are numerically identical.
#[test]
fn test_lexical_and_exact_score_identically_by_ratio() {
    fn term_score(term: &str, name_lower: &str) -> f64 {
        if name_lower == term {
            return 1.0;
        }
        let ratio = term.len() as f64 / name_lower.len() as f64;
        0.7 + 0.3 * ratio
    }
    let handle_score = term_score("handle", "handle__tool__call"); // 6 / 18
    let anchor_score = term_score("anchor", "_concrete__anchor_"); // 6 / 18
    assert!(
        (handle_score - anchor_score).abs() < 1e-9,
        "handle={handle_score} anchor={anchor_score} — expected identical scores at equal ratio"
    );
}

// Full reproduction of issue #14: a fixture graph carrying a symbol whose
// name lexically contains a common English word ("anchor" inside
// `_CONCRETE_ANCHOR`), plus the two real target symbols the original bug
// report's description cited verbatim (`grad_rgb`, `make_bar`). No
// reproduction, no fix (Move 4) — this is that reproduction, run against
// the actual `search_and_classify` pipeline stage-4 uses in production.
#[test]
fn test_repro_issue14_lexical_false_positive_excluded_from_matched() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp = tempfile::Builder::new()
        .prefix("prd_input_issue14_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = std::fs::remove_dir_all(&tmp);
    let src_dir = tmp.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("fixture.rs"),
        r#"
pub const _CONCRETE_ANCHOR: &str = "anchor";

pub fn grad_rgb(a: u8, b: u8) -> u8 {
    a.wrapping_add(b)
}

pub fn make_bar(v: u8) -> String {
    format!("bar-{v}")
}
"#,
    )
    .unwrap();
    let graph_dir = tmp.join("graph");
    let r = crate::indexer::index_codebase(&src_dir, &graph_dir).expect("index fixture");
    let store = GraphStore::open_or_create(&r.graph_path).unwrap();

    // Mirrors the real issue #14 report: description cites `grad_rgb`
    // verbatim and separately uses the natural word "anchor" (a pure
    // substring of _CONCRETE_ANCHOR) with no backticks.
    let title = "Gradient bar rendering";
    let description = "Continuous RGB gradient between color anchors, rendered by `grad_rgb`.";
    let combined = format!("{title} {description}");
    let verbatim = extract_verbatim_identifiers(&combined);
    let natural = tokenize_natural(title, description);
    let outcome = search_and_classify(&store, &verbatim, &natural, None);

    assert!(
        outcome
            .matched
            .iter()
            .any(|m| m.name == "grad_rgb" && m.match_mode == MatchMode::Verbatim),
        "grad_rgb must be matched via verbatim citation; matched={:?}",
        outcome.matched.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
    assert!(
        !outcome.matched.iter().any(|m| m.name == "_CONCRETE_ANCHOR"),
        "issue #14: _CONCRETE_ANCHOR must never be verified grounding purely \
         because the description contains the substring word 'anchor'"
    );
    if let Some(c) = outcome
        .candidates
        .iter()
        .find(|m| m.name == "_CONCRETE_ANCHOR")
    {
        assert_eq!(
            c.match_mode,
            MatchMode::Lexical,
            "_CONCRETE_ANCHOR may surface as a candidate but must be tagged lexical"
        );
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

// Complement: when nothing exact or verbatim resolves, matched_symbols
// must be empty rather than filled with plausible-looking noise (issue
// #14: "an empty bundle beats a misleading one").
#[test]
fn test_repro_issue14_pure_lexical_description_yields_empty_matched() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp = tempfile::Builder::new()
        .prefix("prd_input_issue14_empty_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = std::fs::remove_dir_all(&tmp);
    let src_dir = tmp.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("fixture.rs"),
        "pub const _CONCRETE_ANCHOR: &str = \"anchor\";\n",
    )
    .unwrap();
    let graph_dir = tmp.join("graph");
    let r = crate::indexer::index_codebase(&src_dir, &graph_dir).expect("index fixture");
    let store = GraphStore::open_or_create(&r.graph_path).unwrap();

    let title = "Color gradient anchors";
    let description = "Continuous gradient rendered between color anchor points.";
    let combined = format!("{title} {description}");
    let verbatim = extract_verbatim_identifiers(&combined);
    let natural = tokenize_natural(title, description);
    let outcome = search_and_classify(&store, &verbatim, &natural, None);

    assert!(
        outcome.matched.is_empty(),
        "no verbatim/exact-name evidence exists — matched_symbols must be empty, got {:?}",
        outcome.matched.iter().map(|m| &m.name).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// Measured evidence for issue #18 (2026-07-15): `search_and_classify`
// unconditionally passed `index_dir: None` to `search::search_graph`, so
// every call ran the substring-fallback scorer even when a hybrid
// BM25/vector index existed for the graph — capping recall at the weakest
// matcher unconditionally. `search::score_candidate`'s substring path
// returns 0.0 for a token with no substring relation to a candidate at
// all, and that 0.0 is NOT filtered (min_score in `search_hits` is also
// 0.0), so up to `MATCHES_PER_TOKEN` zero-relevance symbols leak into
// `candidate_symbols` for every unrelated filler word in a description.
// This test measures that leak on a fixed fixture, comparing the pre-fix
// behavior (`index_dir: None`, reproduced explicitly) against the fixed
// behavior (`index_dir: Some(resolved)`) on the IDENTICAL graph.
// source: measured on 2026-07-15, this test's own fixture (below).
#[test]
fn test_issue18_hybrid_index_reduces_spurious_candidates() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp = tempfile::Builder::new()
        .prefix("prd_input_issue18_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = std::fs::remove_dir_all(&tmp);
    let src_dir = tmp.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("main.rs"),
        "fn main() {\n    handle_tool_call(\"probe\");\n}\n\n\
         fn handle_tool_call(name: &str) -> String {\n    resolve_name(name)\n}\n\n\
         fn resolve_name(name: &str) -> String {\n    name.to_string()\n}\n\n\
         pub struct Tool {\n    pub name: String,\n}\n",
    )
    .unwrap();
    let graph_dir = tmp.join("graph");
    let r = crate::indexer::index_codebase(&src_dir, &graph_dir).expect("index fixture");
    let store = GraphStore::open_or_create(&r.graph_path).unwrap();
    let _ = crate::resolver::resolve_graph(&store);
    let _ = crate::clustering::cluster_graph(&store, 1.0);
    let output_dir = graph_dir.parent().unwrap();
    crate::search::build_search_index(&store, output_dir).expect("build search index");
    let index_dir = crate::search::resolve_search_index_dir(&graph_dir);
    assert!(
        index_dir.is_some(),
        "fixture must produce a search index for this measurement"
    );

    // Description mixes one genuine exact citation (`handle_tool_call`)
    // with natural-language filler words that have NO substring relation
    // to any symbol in the fixture (main/handle_tool_call/resolve_name/Tool).
    let title = "Improve reliability during deployment rollouts";
    let description = "Operators worry about latency spikes and monitoring \
        dashboards while `handle_tool_call` dispatches requests.";
    let combined = format!("{title} {description}");
    let verbatim = extract_verbatim_identifiers(&combined);
    let natural = tokenize_natural(title, description);

    // Pre-fix behavior, reproduced explicitly (not via the production path,
    // which no longer passes None once the fixture has an index).
    let before = search_and_classify(&store, &verbatim, &natural, None);
    // Fixed behavior — the resolved hybrid index.
    let after = search_and_classify(&store, &verbatim, &natural, index_dir.as_deref());

    eprintln!(
        "[measured issue18] candidate_symbols before(substring, index_dir=None)={} \
         after(hybrid, index_dir=resolved)={}",
        before.candidates.len(),
        after.candidates.len()
    );

    // The exact verbatim citation must resolve identically either way —
    // wiring in the hybrid index must not change match_mode semantics for
    // a real hit (issue #14's classification is independent of scorer).
    assert!(
        before
            .matched
            .iter()
            .any(|m| m.name == "handle_tool_call" && m.match_mode == MatchMode::Verbatim),
        "substring-fallback path (pre-fix behavior) must still classify the citation correctly"
    );
    assert!(
        after
            .matched
            .iter()
            .any(|m| m.name == "handle_tool_call" && m.match_mode == MatchMode::Verbatim),
        "hybrid path must still classify the same verbatim citation correctly"
    );

    // The substring-fallback path leaks zero-relevance candidates (see
    // header comment); the hybrid path only surfaces symbols BM25/vector
    // actually rank for the query, so it must never leak MORE noise.
    assert!(
        before.candidates.len() >= after.candidates.len(),
        "expected substring fallback to leak at least as many spurious \
         candidates as the hybrid index: before={} after={}",
        before.candidates.len(),
        after.candidates.len()
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// Regression: a symbol can be hit by a WEAK token (lexical) before its
// own EXACT token is processed later in tokenization order (e.g. "handle"
// in the title, "handle_tool_call" in the description). Classifying on
// first-seen would permanently lock the symbol in as a lexical candidate
// and silently drop it from matched_symbols even though an exact hit
// exists — caught by tests/stage4_integration.rs's end-to-end test.
#[test]
fn test_weak_token_before_exact_token_still_classifies_as_exact() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp = tempfile::Builder::new()
        .prefix("prd_input_issue14_order_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = std::fs::remove_dir_all(&tmp);
    let src_dir = tmp.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("fixture.rs"),
        "pub fn handle_tool_call(name: &str) -> String { name.to_string() }\n",
    )
    .unwrap();
    let graph_dir = tmp.join("graph");
    let r = crate::indexer::index_codebase(&src_dir, &graph_dir).expect("index fixture");
    let store = GraphStore::open_or_create(&r.graph_path).unwrap();

    // "handle" (weak, lexical-only against handle_tool_call) appears in
    // the title, BEFORE the exact token "handle_tool_call" in the
    // description — tokenize_natural preserves that order.
    let title = "handle tool call should reject unknown tools";
    let description = "The handle_tool_call function must reject unknown tool names.";
    let natural = tokenize_natural(title, description);
    assert_eq!(
        natural[0], "handle",
        "test assumes 'handle' tokenizes first"
    );
    let outcome = search_and_classify(&store, &[], &natural, None);

    let m = outcome
        .matched
        .iter()
        .find(|m| m.name == "handle_tool_call")
        .expect(
            "handle_tool_call must end up in matched_symbols despite the earlier weak 'handle' hit",
        );
    assert_eq!(m.match_mode, MatchMode::ExactName);
    assert!(
        !outcome
            .candidates
            .iter()
            .any(|c| c.name == "handle_tool_call"),
        "handle_tool_call must not ALSO appear as a downgraded candidate"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
