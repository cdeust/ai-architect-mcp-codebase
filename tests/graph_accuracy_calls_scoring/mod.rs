// graph_accuracy_calls_scoring — exact scoring of the `Calls` kind (#335).
//
// Before the per-site `Calls_CallSite_*` rows existed, a CallSite-sourced
// `Calls` expectation (`<caller>::callsite::<tag>::<suffix>` -> target) could
// only be matched by COUNT: N such expectations were satisfied by any N
// observed Calls edges not in the strict set, so a call resolved to the wrong
// target scored as a true positive. The per-site rows name each site's
// target, so the expectation is now matched by identity:
//
// - a real `(callee, line)` expectation (text.py, similarity.py, hash.py)
//   must match a per-site row with the same caller, line and target;
// - a placeholder expectation (`__resolved__`, `__m2fn__`, `__m2m__`, …)
//   carries no line, because those fixtures were annotated one entry per
//   distinct (caller, callee) pair, so it must match a per-site row with the
//   same caller and target, whatever its line.
//
// A false positive is an observed (caller, target) pair, per-site or
// symbol-level, that no expectation names. Two sites of one caller reaching
// one target are one pair, which is the granularity the annotations use.

use super::{ExpectedEdge, Score};
use ai_architect_mcp::graph_store::GraphStore;
use std::collections::BTreeSet;

/// One per-site row as (caller qualified name, call line, target id).
pub type SiteTarget = (String, u64, String);

/// Every `Calls_CallSite_*` row of the graph, keyed by the site's caller and
/// line (both read from the CallSite id, `<caller>::call@<line>:<col>#…`).
pub fn collect_per_site_calls(store: &GraphStore) -> BTreeSet<SiteTarget> {
    let mut out = BTreeSet::new();
    for table in [
        "Calls_CallSite_Function",
        "Calls_CallSite_Method",
        "Calls_CallSite_StdlibSymbol",
    ] {
        let q = format!("MATCH (cs:CallSite)-[:{table}]->(t) RETURN cs.id, t.id");
        let rows = store
            .execute_query(&q)
            .unwrap_or_else(|e| panic!("query {table}: {e}"))
            .rows;
        for row in rows {
            let (caller, line) = parse_call_site_id(&row[0])
                .unwrap_or_else(|| panic!("CallSite id without ::call@<line>: {}", row[0]));
            out.insert((caller, line, row[1].clone()));
        }
    }
    out
}

/// `<caller>::call@<line>:<col>#<span>` -> (caller, line).
fn parse_call_site_id(id: &str) -> Option<(String, u64)> {
    let (caller, rest) = id.rsplit_once("::call@")?;
    let line = rest.split(':').next()?.parse().ok()?;
    Some((caller.to_string(), line))
}

/// The distinct (caller, target) pairs of the per-site rows.
pub fn per_site_pairs(per_site: &BTreeSet<SiteTarget>) -> BTreeSet<(String, String)> {
    per_site
        .iter()
        .map(|(caller, _, target)| (caller.clone(), target.clone()))
        .collect()
}

/// How one `Calls` expectation is matched.
enum CallsKey {
    /// Symbol-level `from -> to`, matched against the symbol-level edges.
    Symbol(String, String),
    /// A site with a known line, matched against one per-site row.
    Site(SiteTarget),
    /// A placeholder site, matched against any per-site row of the pair.
    Pair(String, String),
}

fn calls_key(e: &ExpectedEdge) -> CallsKey {
    let target = e.to_qn.clone();
    let Some((caller, rest)) = e.from_qn.split_once("::callsite::") else {
        return CallsKey::Symbol(e.from_qn.clone(), target);
    };
    let caller = caller.to_string();
    match rest.rsplit_once("::") {
        Some((tag, line)) if !tag.starts_with("__") => match line.parse() {
            Ok(line) => CallsKey::Site((caller, line, target)),
            Err(_) => panic!(
                "CallSite expectation with a non-numeric line: {}",
                e.from_qn
            ),
        },
        _ => CallsKey::Pair(caller, target),
    }
}

/// The outcome of matching the `Calls` expectations by identity.
pub struct CallsMatch {
    pub tp: usize,
    /// Expectations no observation satisfied, as (from_qn, to_qn).
    pub missing: Vec<(String, String)>,
    /// Observed (caller, target) pairs no expectation names.
    pub unexpected: Vec<(String, String)>,
}

impl CallsMatch {
    pub fn score(&self) -> Score {
        Score {
            tp: self.tp,
            fp: self.unexpected.len(),
            fn_: self.missing.len(),
        }
    }
}

/// Matches the `Calls` expectations by identity (see the module doc).
pub fn match_calls(
    expected: &[&ExpectedEdge],
    symbol_pairs: &BTreeSet<(String, String)>,
    per_site: &BTreeSet<SiteTarget>,
) -> CallsMatch {
    let site_pairs = per_site_pairs(per_site);
    let mut m = CallsMatch {
        tp: 0,
        missing: Vec::new(),
        unexpected: Vec::new(),
    };
    let mut named: BTreeSet<(String, String)> = BTreeSet::new();
    for e in expected {
        let (hit, pair) = match calls_key(e) {
            CallsKey::Symbol(from, to) => (
                symbol_pairs.contains(&(from.clone(), to.clone())),
                (from, to),
            ),
            CallsKey::Site(site) => (per_site.contains(&site), (site.0, site.2)),
            CallsKey::Pair(from, to) => {
                (site_pairs.contains(&(from.clone(), to.clone())), (from, to))
            }
        };
        if hit {
            m.tp += 1;
        } else {
            m.missing.push((e.from_qn.clone(), e.to_qn.clone()));
        }
        named.insert(pair);
    }
    m.unexpected = site_pairs
        .union(symbol_pairs)
        .filter(|p| !named.contains(*p))
        .cloned()
        .collect();
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relaxed(caller: &str, target: &str) -> ExpectedEdge {
        ExpectedEdge {
            kind: "Calls",
            from_qn: format!("{caller}::callsite::__resolved__::0"),
            to_qn: target.to_string(),
        }
    }

    #[test]
    fn count_parity_accepted_a_call_resolved_to_the_wrong_target() {
        // One expectation a -> b, one observed call a -> c: the counts agree,
        // which the count-based match scored as a true positive.
        let expected = relaxed("f.py::a", "f.py::b");
        let wrong = ("f.py::a".to_string(), "f.py::c".to_string());
        let per_site = BTreeSet::from([("f.py::a".to_string(), 3, "f.py::c".to_string())]);
        let s = match_calls(&[&expected], &BTreeSet::from([wrong]), &per_site).score();
        assert_eq!((s.tp, s.fp, s.fn_), (0, 1, 1));
    }

    #[test]
    fn a_site_expectation_must_match_the_line_as_well_as_the_target() {
        let expected = ExpectedEdge {
            kind: "Calls",
            from_qn: "f.py::a::callsite::b::7".to_string(),
            to_qn: "f.py::b".to_string(),
        };
        let pair = ("f.py::a".to_string(), "f.py::b".to_string());
        let at = |line| BTreeSet::from([("f.py::a".to_string(), line, "f.py::b".to_string())]);
        let pairs = BTreeSet::from([pair]);
        assert_eq!(match_calls(&[&expected], &pairs, &at(7)).tp, 1);
        assert_eq!(match_calls(&[&expected], &pairs, &at(8)).missing.len(), 1);
    }

    #[test]
    fn two_sites_of_one_pair_are_one_true_positive() {
        let expected = relaxed("f.py::a", "f.py::b");
        let site = |line| ("f.py::a".to_string(), line, "f.py::b".to_string());
        let per_site = BTreeSet::from([site(3), site(4)]);
        let s = match_calls(&[&expected], &per_site_pairs(&per_site), &per_site).score();
        assert_eq!((s.tp, s.fp, s.fn_), (1, 0, 0));
    }

    #[test]
    fn call_site_ids_parse_to_caller_and_line() {
        assert_eq!(
            parse_call_site_id("m.py::C::f::call@12:4#100-120"),
            Some(("m.py::C::f".to_string(), 12))
        );
        assert_eq!(parse_call_site_id("m.py::f"), None);
    }
}
