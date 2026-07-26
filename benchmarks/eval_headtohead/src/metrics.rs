//! Retrieval metrics (precision / recall / F1) and dispersion (mean / sample
//! stdev). A point estimate without a spread is not a measurement (§AC4).

use std::collections::BTreeSet;

/// Precision / recall / F1 of a retrieved file set against ground truth.
#[derive(Debug, Clone, Copy)]
pub struct Retrieval {
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
}

/// Score a retrieved set against the truth set.
///
/// Conventions for empty sets (stated so the numbers are interpretable):
/// - Empty retrieval against a non-empty truth ⇒ precision 1.0 (vacuous: no
///   false positives), recall 0.0 (found nothing), F1 0.0.
/// - Non-empty retrieval against an (impossible here) empty truth ⇒ recall 1.0.
pub fn score(retrieved: &BTreeSet<String>, truth: &BTreeSet<String>) -> Retrieval {
    let tp = retrieved.intersection(truth).count() as f64;
    let retrieved_n = retrieved.len() as f64;
    let truth_n = truth.len() as f64;
    let precision = if retrieved_n == 0.0 {
        1.0
    } else {
        tp / retrieved_n
    };
    let recall = if truth_n == 0.0 { 1.0 } else { tp / truth_n };
    // Guard the division. Mutation note: replacing `+` with `*` here is a
    // provably EQUIVALENT mutant — when `precision * recall == 0` at least one
    // factor is 0, so the harmonic mean is 0 either way; when it is non-zero,
    // both factors are non-zero, so `precision + recall != 0` too and the same
    // formula runs. Output is identical for every input; no test can kill it.
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    Retrieval {
        precision,
        recall,
        f1,
    }
}

/// Arithmetic mean, or 0.0 for an empty slice.
pub fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

/// Sample standard deviation (N-1 denominator). Returns 0.0 for fewer than two
/// observations — one point has no spread.
/// source: Bessel's correction — unbiased sample variance uses N-1
/// (Casella & Berger, *Statistical Inference* 2nd ed., §7.3.1).
pub fn stdev(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let m = mean(xs);
    let var = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() as f64 - 1.0);
    var.sqrt()
}

/// Round to two decimals for reporting.
pub fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn perfect_retrieval_scores_one() {
        let r = score(&set(&["a", "b"]), &set(&["a", "b"]));
        assert_eq!(r.precision, 1.0);
        assert_eq!(r.recall, 1.0);
        assert_eq!(r.f1, 1.0);
    }

    #[test]
    fn over_retrieval_loses_precision_keeps_recall() {
        // retrieved {a,b,c}, truth {a,b}: 2/3 precision, 2/2 recall.
        let r = score(&set(&["a", "b", "c"]), &set(&["a", "b"]));
        assert!((r.precision - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(r.recall, 1.0);
        // F1 = 2·(2/3)·1 / (2/3 + 1) = 0.8 — pins the harmonic-mean formula.
        assert!((r.f1 - 0.8).abs() < 1e-9);
    }

    #[test]
    fn f1_is_the_harmonic_mean_not_a_degenerate_form() {
        // p=0.5, r=1.0 ⇒ F1 = 2·0.5·1/(1.5) = 2/3. A mutated formula
        // (e.g. 2/p·r or p·r) would not land here.
        let r = score(&set(&["a", "x"]), &set(&["a"]));
        assert_eq!(r.precision, 0.5);
        assert_eq!(r.recall, 1.0);
        assert!((r.f1 - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn under_retrieval_loses_recall_keeps_precision() {
        // retrieved {a}, truth {a,b}: 1/1 precision, 1/2 recall.
        let r = score(&set(&["a"]), &set(&["a", "b"]));
        assert_eq!(r.precision, 1.0);
        assert_eq!(r.recall, 0.5);
    }

    #[test]
    fn empty_retrieval_is_vacuous_precision_zero_recall() {
        let r = score(&BTreeSet::new(), &set(&["a"]));
        assert_eq!(r.precision, 1.0);
        assert_eq!(r.recall, 0.0);
        assert_eq!(r.f1, 0.0);
    }

    #[test]
    fn disjoint_retrieval_scores_zero_precision_and_recall() {
        let r = score(&set(&["x"]), &set(&["a"]));
        assert_eq!(r.precision, 0.0);
        assert_eq!(r.recall, 0.0);
        assert_eq!(r.f1, 0.0);
    }

    #[test]
    fn mean_and_stdev() {
        assert_eq!(mean(&[]), 0.0);
        assert_eq!(mean(&[2.0, 4.0]), 3.0);
        assert_eq!(stdev(&[5.0]), 0.0);
        // sample stdev of [2,4] with N-1: var = ((1)+(1))/1 = 2 → sqrt 2.
        assert!((stdev(&[2.0, 4.0]) - 2.0_f64.sqrt()).abs() < 1e-9);
        // With N=3 the N-1 denominator (2) differs from N (3) AND from N-1 as a
        // multiplier: var([2,4,6]) = (4+0+4)/2 = 4 → stdev 2.0. Pins the divide.
        assert!((stdev(&[2.0, 4.0, 6.0]) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn round2_truncates_to_two_decimals() {
        assert_eq!(round2(1.005_1), 1.01);
        assert_eq!(round2(17.399), 17.4);
    }
}
