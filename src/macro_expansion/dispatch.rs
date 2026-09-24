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
use super::scope::{original_name, type_origin, Import, TypeOrigin};

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

/// What the parser and the file say about a `write!` destination.
#[derive(Debug, Clone, Copy)]
pub struct Destination<'a> {
    /// The declared type of a plain local as written (`fmt::Formatter`,
    /// `File`, `std::fs::File`); empty when the binding is untyped or the
    /// destination is not a plain local.
    pub declared: &'a str,
    /// The destination is a plain local rather than a field, call or other
    /// expression. Only a local's type is ever recorded.
    pub plain_local: bool,
    /// The file that holds the site defines a type named like `declared`.
    pub defined_in_file: bool,
}

/// Decides a receiver-typed macro from the destination's type in the scope of
/// its file, then from the imports in scope.
///
/// A destination that is not a plain local is undetermined: its type is not
/// recorded, and an import cannot stand in for it (`Formatter` needs no import
/// at all). A type that is not std is undetermined, and so is a type with a std
/// name that nothing in the file places (`Unknown`): guessing std for either is
/// how a tokio `File` or a repository `Formatter` got a std target.
pub fn decide_by_receiver(
    alternatives: &[Alternative],
    dest: &Destination,
    imports: &[Import],
) -> Decision {
    let all = Decision::Undetermined {
        candidates: alternatives.len(),
    };
    if !dest.plain_local {
        return all;
    }
    let name = original_name(dest.declared, imports);
    match type_origin(dest.declared, imports, dest.defined_in_file) {
        TypeOrigin::Std => {}
        TypeOrigin::Unknown if dest.declared.is_empty() => {}
        _ => return all,
    }
    let by_type: Vec<&Alternative> = alternatives
        .iter()
        .filter(|a| a.receiver_types.contains(&name))
        .collect();
    if let [only] = by_type.as_slice() {
        return Decision::Target {
            canonical: only.canonical,
            basis: Basis::ReceiverType,
        };
    }
    decide_by_imports(alternatives, imports)
}

fn decide_by_imports(alternatives: &[Alternative], imports: &[Import]) -> Decision {
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

fn is_imported(imports: &[Import], marker: &str) -> bool {
    imports
        .iter()
        .any(|i| i.path == marker || i.path.ends_with(&format!("::{marker}")))
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

    fn dest(declared: &str) -> Destination<'_> {
        Destination {
            declared,
            plain_local: true,
            defined_in_file: false,
        }
    }

    fn imports(paths: &[&str]) -> Vec<Import> {
        paths.iter().map(|p| Import::new(p, "", false)).collect()
    }

    fn target(canonical: &'static str, basis: Basis) -> Decision {
        Decision::Target { canonical, basis }
    }

    #[test]
    fn a_formatter_destination_reaches_only_the_inherent_write_fmt() {
        let all = imports(&["std::fmt::Formatter", "std::fmt::Write", "std::io::Write"]);
        assert_eq!(
            decide_by_receiver(alts(), &dest("Formatter"), &all),
            target("core::fmt::Formatter::write_fmt", Basis::ReceiverType)
        );
    }

    #[test]
    fn an_io_destination_never_gets_the_fmt_trait_method() {
        let imp = imports(&["std::io::BufWriter", "std::fmt::Write"]);
        assert_eq!(
            decide_by_receiver(alts(), &dest("BufWriter"), &imp),
            target("std::io::Write::write_fmt", Basis::ReceiverType)
        );
    }

    #[test]
    fn an_untyped_local_is_decided_by_the_one_imported_trait() {
        let imp = imports(&["std::io", "std::io::Write"]);
        assert_eq!(
            decide_by_receiver(alts(), &dest(""), &imp),
            target("std::io::Write::write_fmt", Basis::ImportScope)
        );
    }

    #[test]
    fn an_untyped_local_with_both_traits_imported_is_ambiguous() {
        let both = imports(&["std::fmt::Write", "std::io::Write"]);
        assert_eq!(
            decide_by_receiver(alts(), &dest(""), &both),
            Decision::Undetermined { candidates: 2 }
        );
    }

    #[test]
    fn an_untyped_local_with_no_trait_imported_is_ambiguous_across_all() {
        assert_eq!(
            decide_by_receiver(alts(), &dest(""), &imports(&["std::collections::HashMap"])),
            Decision::Undetermined { candidates: 3 }
        );
    }

    /// A named type nothing in the file places (a generic `W`, a name from a
    /// glob) is not decided by which write trait happens to be imported.
    #[test]
    fn a_named_type_that_nothing_places_is_undetermined_whatever_is_imported() {
        for name in ["W", "Custom", "File"] {
            assert_eq!(
                decide_by_receiver(alts(), &dest(name), &imports(&["std::io::Write"])),
                Decision::Undetermined { candidates: 3 },
                "{name}"
            );
        }
    }

    #[test]
    fn a_glob_import_of_the_io_prelude_brings_io_write_into_scope() {
        let glob = [Import::new("std::io::prelude", "", true)];
        assert_eq!(
            decide_by_receiver(alts(), &dest(""), &glob),
            target("std::io::Write::write_fmt", Basis::ImportScope)
        );
    }

    #[test]
    fn a_destination_that_is_not_a_plain_local_is_undetermined_whatever_is_imported() {
        let not_local = Destination {
            declared: "",
            plain_local: false,
            defined_in_file: false,
        };
        assert_eq!(
            decide_by_receiver(alts(), &not_local, &imports(&["std::io::Write"])),
            Decision::Undetermined { candidates: 3 }
        );
    }

    #[test]
    fn a_type_defined_in_the_file_is_undetermined_unless_std_is_imported_by_name() {
        let own = Destination {
            defined_in_file: true,
            ..dest("Formatter")
        };
        assert_eq!(
            decide_by_receiver(alts(), &own, &imports(&["std::io::Write"])),
            Decision::Undetermined { candidates: 3 }
        );
    }

    #[test]
    fn an_alias_of_a_non_std_type_is_undetermined_and_a_std_alias_resolves() {
        let imp = vec![
            Import::new("tokio::fs::File", "F", false),
            Import::new("std::fs::File", "SF", false),
            Import::new("std::io::Write", "", false),
        ];
        assert_eq!(
            decide_by_receiver(alts(), &dest("F"), &imp),
            Decision::Undetermined { candidates: 3 }
        );
        assert_eq!(
            decide_by_receiver(alts(), &dest("SF"), &imp),
            target("std::io::Write::write_fmt", Basis::ReceiverType)
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
