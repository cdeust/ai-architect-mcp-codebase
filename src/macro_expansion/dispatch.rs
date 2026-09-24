// macro_expansion::dispatch — the macros whose target is decided, not listed
// (issue #339).
//
// `println!` always calls `_print`: its targets are a fixed set, and every one
// of them is called. `write!(dst, ..)` is not like that: it expands to
// `dst.write_fmt(..)`, and which `write_fmt` runs depends on the type of `dst`.
// Listing all of them as calls gives one call site several targets, some
// impossible for the receiver at hand. `vec!` is the same with an argument
// shape in place of a receiver. This module holds the rule for each and a pure
// decision function; it does no I/O.
//
// A decision is either one target with the evidence that produced it, or
// nothing: an undetermined site gets no edge, so a reader never sees a target
// the expansion may not have called.

use super::rust::{DEST_MACROS, VEC_MACROS};

/// One target a receiver-decided macro may call, with what selects it.
#[derive(Debug, Clone, Copy)]
pub struct Alternative {
    pub canonical: &'static str,
    /// Declared type names (last path segment) of a destination that selects
    /// this target.
    pub receiver_types: &'static [&'static str],
    /// Import path suffixes that bring the trait in scope; empty when only a
    /// receiver type can select this target.
    pub import_markers: &'static [&'static str],
}

/// How a macro's target is decided.
#[derive(Debug, Clone, Copy)]
pub enum Dispatch {
    /// By the destination's declared type, then by the imports in scope.
    ReceiverType(&'static [Alternative]),
    /// By the argument shape the parser recorded (`macro_arg_shape`); a shape
    /// with no entry has no stable target.
    ArgShape(&'static [(&'static str, &'static str)]),
}

/// What decided a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    ReceiverType,
    ImportScope,
    ArgShape,
}

/// The outcome for one site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Target {
        canonical: &'static str,
        basis: Basis,
    },
    /// No single target. `candidates` is how many alternatives stayed
    /// possible: 0 means the shape has no stable target at all.
    Undetermined { candidates: usize },
}

/// The dispatch rule of a Rust macro, if its target is decided.
pub fn dispatch_for(macro_name: &str) -> Option<Dispatch> {
    if DEST_MACROS.contains(&macro_name) {
        Some(Dispatch::ReceiverType(WRITE_FMT_ALTERNATIVES))
    } else if VEC_MACROS.contains(&macro_name) {
        Some(Dispatch::ArgShape(VEC_BY_SHAPE))
    } else {
        None
    }
}

// source: https://doc.rust-lang.org/std/macro.write.html — `write!` calls
// `write_fmt` on its first argument. `fmt::Formatter` has an inherent
// `write_fmt` (https://doc.rust-lang.org/std/fmt/struct.Formatter.html), so a
// `Formatter` never reaches a trait method; `String` implements `fmt::Write`
// only; the io types implement `io::Write`.
const WRITE_FMT_ALTERNATIVES: &[Alternative] = &[
    Alternative {
        canonical: "core::fmt::Formatter::write_fmt",
        receiver_types: &["Formatter"],
        import_markers: &[],
    },
    Alternative {
        canonical: "std::fmt::Write::write_fmt",
        receiver_types: &["String"],
        import_markers: &["fmt::Write"],
    },
    Alternative {
        canonical: "std::io::Write::write_fmt",
        // source: the std types documented as implementing `io::Write`.
        receiver_types: &[
            "File",
            "BufWriter",
            "LineWriter",
            "Stdout",
            "StdoutLock",
            "Stderr",
            "StderrLock",
            "Vec",
            "Cursor",
            "TcpStream",
            "ChildStdin",
            "Sink",
        ],
        import_markers: &["io::Write", "io::prelude"],
    },
];

// source: the `vec!` macro in alloc (https://doc.rust-lang.org/src/alloc/macros.rs.html):
// `vec![]` is `Vec::new()`, `vec![x; n]` is `vec::from_elem(x, n)`. The list
// form is `<[_]>::into_vec(..)`, a slice method with no path in the stdlib
// index, so it has no entry and stays undetermined.
const VEC_BY_SHAPE: &[(&str, &str)] = &[
    ("empty", "std::vec::Vec::new"),
    ("repeat", "std::vec::from_elem"),
];

/// Decides a receiver-typed macro from the destination's declared type, then
/// from the imports in scope.
pub fn decide_by_receiver(
    alternatives: &[Alternative],
    receiver_type: &str,
    imports: &[String],
) -> Decision {
    let by_type: Vec<&Alternative> = alternatives
        .iter()
        .filter(|a| !receiver_type.is_empty() && a.receiver_types.contains(&receiver_type))
        .collect();
    if let [only] = by_type.as_slice() {
        return Decision::Target {
            canonical: only.canonical,
            basis: Basis::ReceiverType,
        };
    }
    let imported: Vec<&Alternative> = alternatives
        .iter()
        .filter(|a| a.import_markers.iter().any(|m| is_imported(imports, m)))
        .collect();
    match imported.as_slice() {
        [only] => Decision::Target {
            canonical: only.canonical,
            basis: Basis::ImportScope,
        },
        [] => Decision::Undetermined {
            candidates: alternatives.len(),
        },
        several => Decision::Undetermined {
            candidates: several.len(),
        },
    }
}

/// Decides a shape-typed macro from the recorded argument shape.
pub fn decide_by_shape(rules: &[(&str, &'static str)], shape: &str) -> Decision {
    match rules.iter().find(|(s, _)| *s == shape) {
        Some((_, canonical)) => Decision::Target {
            canonical,
            basis: Basis::ArgShape,
        },
        None => Decision::Undetermined { candidates: 0 },
    }
}

fn is_imported(imports: &[String], marker: &str) -> bool {
    imports
        .iter()
        .any(|p| p == marker || p.ends_with(&format!("::{marker}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alts() -> &'static [Alternative] {
        match dispatch_for("write") {
            Some(Dispatch::ReceiverType(a)) => a,
            _ => panic!("write is receiver-decided"),
        }
    }

    fn imports(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn a_formatter_destination_reaches_only_the_inherent_write_fmt() {
        let both = imports(&["std::fmt::Write", "std::io::Write"]);
        assert_eq!(
            decide_by_receiver(alts(), "Formatter", &both),
            Decision::Target {
                canonical: "core::fmt::Formatter::write_fmt",
                basis: Basis::ReceiverType
            }
        );
    }

    #[test]
    fn an_io_destination_never_gets_the_fmt_trait_method() {
        let d = decide_by_receiver(alts(), "BufWriter", &imports(&["std::fmt::Write"]));
        assert_eq!(
            d,
            Decision::Target {
                canonical: "std::io::Write::write_fmt",
                basis: Basis::ReceiverType
            }
        );
    }

    #[test]
    fn an_unknown_destination_is_decided_by_the_one_imported_trait() {
        let d = decide_by_receiver(alts(), "", &imports(&["std::io", "std::io::Write"]));
        assert_eq!(
            d,
            Decision::Target {
                canonical: "std::io::Write::write_fmt",
                basis: Basis::ImportScope
            }
        );
    }

    #[test]
    fn an_unknown_destination_with_both_traits_imported_is_ambiguous() {
        let both = imports(&["std::fmt::Write", "std::io::Write"]);
        assert_eq!(
            decide_by_receiver(alts(), "MyWriter", &both),
            Decision::Undetermined { candidates: 2 }
        );
    }

    #[test]
    fn an_unknown_destination_with_no_trait_imported_is_ambiguous_across_all() {
        assert_eq!(
            decide_by_receiver(alts(), "", &imports(&["std::collections::HashMap"])),
            Decision::Undetermined { candidates: 3 }
        );
    }

    #[test]
    fn vec_shapes_pick_their_own_constructor_and_a_list_has_none() {
        let Some(Dispatch::ArgShape(rules)) = dispatch_for("vec") else {
            panic!("vec is shape-decided");
        };
        assert!(matches!(
            decide_by_shape(rules, "empty"),
            Decision::Target {
                canonical: "std::vec::Vec::new",
                ..
            }
        ));
        assert!(matches!(
            decide_by_shape(rules, "repeat"),
            Decision::Target {
                canonical: "std::vec::from_elem",
                ..
            }
        ));
        assert_eq!(
            decide_by_shape(rules, "list"),
            Decision::Undetermined { candidates: 0 }
        );
    }

    /// Every alternative is a different trait or type; two paths ending in the
    /// same method are exactly the pair that must never be listed as both called.
    #[test]
    fn no_two_alternatives_share_a_receiver_type() {
        let mut seen = std::collections::HashSet::new();
        for a in alts() {
            for t in a.receiver_types {
                assert!(seen.insert(*t), "{t} selects two targets");
            }
        }
    }
}
