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
}

/// How a macro's target is decided.
#[derive(Debug, Clone, Copy)]
pub enum Dispatch {
    /// By the destination's declared type, placed in the scope of its file.
    /// No import decides a target: a write trait in scope says what the file
    /// may call, not what the destination is.
    ReceiverType(&'static [Alternative]),
    /// By the argument shape the parser recorded (`macro_arg_shape`); a shape
    /// with no entry has no stable target.
    ArgShape(&'static [(&'static str, &'static str)]),
}

/// What decided a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    ReceiverType,
    ArgShape,
}

/// The outcome for one site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Target {
        canonical: &'static str,
        basis: Basis,
    },
    /// The expansion has no stable target whatever the site is (the list form
    /// of `vec!`).
    NoStableTarget,
    /// The site's expansion has a target, but the destination's type is not
    /// determined: not nameable, not std, unplaced or of two origins.
    TypeNotDetermined,
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
    },
    Alternative {
        canonical: "std::fmt::Write::write_fmt",
        receiver_types: &["String"],
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
    /// The file that holds the site defines a type named like `declared`.
    pub defined_in_file: bool,
}

/// Decides a receiver-typed macro from the destination's type in the scope of
/// its file.
///
/// Only a type that resolves to std and names one alternative decides. A
/// destination with no nameable type (a field, `impl Trait`, `dyn Trait`, a
/// generic parameter, a local with no declaration), a type that is not std, a
/// name that nothing in the file places and a name with two origins are all
/// undetermined. The write traits a file imports are not consulted: what is in
/// scope says what the file may call, not what the destination is, so a
/// heuristic on it names a wrong single target for a type that implements the
/// other trait.
pub fn decide_by_receiver(
    alternatives: &[Alternative],
    dest: &Destination,
    imports: &[Import],
) -> Decision {
    if type_origin(dest.declared, imports, dest.defined_in_file) != TypeOrigin::Std {
        return Decision::TypeNotDetermined;
    }
    let name = original_name(dest.declared, imports);
    let mut by_type = alternatives
        .iter()
        .filter(|a| a.receiver_types.contains(&name));
    match (by_type.next(), by_type.next()) {
        (Some(only), None) => Decision::Target {
            canonical: only.canonical,
            basis: Basis::ReceiverType,
        },
        _ => Decision::TypeNotDetermined,
    }
}

/// Decides a shape-typed macro from the recorded argument shape.
pub fn decide_by_shape(rules: &[(&str, &'static str)], shape: &str) -> Decision {
    match rules.iter().find(|(s, _)| *s == shape) {
        Some((_, canonical)) => Decision::Target {
            canonical,
            basis: Basis::ArgShape,
        },
        None => Decision::NoStableTarget,
    }
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
            defined_in_file: false,
        }
    }

    fn imports(paths: &[&str]) -> Vec<Import> {
        paths.iter().map(|p| Import::new(p, "", false)).collect()
    }

    fn target(canonical: &'static str) -> Decision {
        Decision::Target {
            canonical,
            basis: Basis::ReceiverType,
        }
    }

    const NONE: Decision = Decision::TypeNotDetermined;

    #[test]
    fn a_formatter_destination_reaches_only_the_inherent_write_fmt() {
        let all = imports(&["std::fmt::Formatter", "std::fmt::Write", "std::io::Write"]);
        assert_eq!(
            decide_by_receiver(alts(), &dest("Formatter"), &all),
            target("core::fmt::Formatter::write_fmt")
        );
    }

    #[test]
    fn an_io_destination_never_gets_the_fmt_trait_method() {
        let imp = imports(&["std::io::BufWriter", "std::fmt::Write"]);
        assert_eq!(
            decide_by_receiver(alts(), &dest("BufWriter"), &imp),
            target("std::io::Write::write_fmt")
        );
    }

    /// The write traits a file imports never decide a target, whatever the
    /// destination: an untyped local, an unnameable type, a name nothing places.
    #[test]
    fn no_imported_write_trait_decides_a_destination() {
        let one = imports(&["std::io", "std::io::Write"]);
        let both = imports(&["std::fmt::Write", "std::io::Write"]);
        let prelude = [Import::new("std::io::prelude", "", true)];
        for declared in ["", "W", "Custom", "File"] {
            for imp in [&one[..], &both[..], &prelude[..], &[][..]] {
                assert_eq!(
                    decide_by_receiver(alts(), &dest(declared), imp),
                    NONE,
                    "{declared:?}"
                );
            }
        }
    }

    #[test]
    fn a_type_defined_in_the_file_is_undetermined() {
        let own = Destination {
            defined_in_file: true,
            ..dest("Formatter")
        };
        let imp = imports(&["std::fmt::Formatter"]);
        assert_eq!(decide_by_receiver(alts(), &own, &imp), NONE);
    }

    #[test]
    fn an_alias_of_a_non_std_type_is_undetermined_and_a_std_alias_resolves() {
        let imp = vec![
            Import::new("tokio::fs::File", "F", false),
            Import::new("std::fs::File", "SF", false),
        ];
        assert_eq!(decide_by_receiver(alts(), &dest("F"), &imp), NONE);
        assert_eq!(
            decide_by_receiver(alts(), &dest("SF"), &imp),
            target("std::io::Write::write_fmt")
        );
    }

    #[test]
    fn a_std_type_with_no_write_fmt_target_of_its_own_is_undetermined() {
        let imp = imports(&["std::net::UdpSocket"]);
        assert_eq!(decide_by_receiver(alts(), &dest("UdpSocket"), &imp), NONE);
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
        assert_eq!(decide_by_shape(rules, "list"), Decision::NoStableTarget);
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
