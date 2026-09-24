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

/// Where a type name comes from, in the scope of one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeOrigin {
    /// std, core or alloc: written as such, imported from there, or a prelude
    /// name the file does not redefine.
    Std,
    /// Defined in the file or brought in from anywhere else (an external
    /// crate included): a namesake of a std type, never the std one.
    Other,
    /// Nothing in the file says. A bare `File` reaches a file through a glob
    /// import of a module the parser stores without its members, so it may be
    /// either.
    Unknown,
}

// source: the Rust Reference, "Paths" and "Extern crates": `std`, `core` and
// `alloc` are the standard crates a path can start from.
const STD_ROOTS: [&str; 3] = ["std", "core", "alloc"];
// source: https://doc.rust-lang.org/std/prelude/v1/ — the std prelude names
// that select a `write_fmt` target (`String`, `Vec`).
const PRELUDE_TYPES: [&str; 2] = ["String", "Vec"];

/// Resolves a declared type in the scope of the file that holds the site:
/// a path by its root, a bare name by the file's own definitions, then its
/// imports, then the prelude.
pub fn type_origin(declared: &str, imports: &[String], defined_in_file: bool) -> TypeOrigin {
    if declared.is_empty() {
        return TypeOrigin::Unknown;
    }
    if let Some((root, _)) = declared.split_once("::") {
        return origin_of_root(root, imports);
    }
    if defined_in_file {
        return TypeOrigin::Other;
    }
    if let Some(path) = imports.iter().find(|p| last_segment(p) == declared) {
        return if is_std_path(path) {
            TypeOrigin::Std
        } else {
            TypeOrigin::Other
        };
    }
    if PRELUDE_TYPES.contains(&declared) {
        TypeOrigin::Std
    } else {
        TypeOrigin::Unknown
    }
}

/// The first segment of a written path: a std root, an imported module, or
/// something local (`crate`, `self`, an inline module) that is not std.
fn origin_of_root(root: &str, imports: &[String]) -> TypeOrigin {
    if STD_ROOTS.contains(&root) {
        return TypeOrigin::Std;
    }
    match imports.iter().find(|p| last_segment(p) == root) {
        Some(path) if is_std_path(path) => TypeOrigin::Std,
        _ => TypeOrigin::Other,
    }
}

fn is_std_path(path: &str) -> bool {
    path.split("::")
        .next()
        .is_some_and(|r| STD_ROOTS.contains(&r))
}

fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
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
    imports: &[String],
) -> Decision {
    let all = Decision::Undetermined {
        candidates: alternatives.len(),
    };
    if !dest.plain_local {
        return all;
    }
    let name = last_segment(dest.declared);
    let std_named = alternatives
        .iter()
        .any(|a| a.receiver_types.contains(&name));
    match type_origin(dest.declared, imports, dest.defined_in_file) {
        TypeOrigin::Other => return all,
        TypeOrigin::Unknown if std_named => return all,
        _ => {}
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

fn decide_by_imports(alternatives: &[Alternative], imports: &[String]) -> Decision {
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

    fn local(ty: &str) -> Destination<'_> {
        Destination {
            declared: ty,
            plain_local: true,
            defined_in_file: false,
        }
    }

    fn imports(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn a_formatter_destination_reaches_only_the_inherent_write_fmt() {
        let both = imports(&["std::fmt::Formatter", "std::fmt::Write", "std::io::Write"]);
        assert_eq!(
            decide_by_receiver(alts(), &local("Formatter"), &both),
            Decision::Target {
                canonical: "core::fmt::Formatter::write_fmt",
                basis: Basis::ReceiverType
            }
        );
    }

    #[test]
    fn an_io_destination_never_gets_the_fmt_trait_method() {
        let d = decide_by_receiver(
            alts(),
            &local("BufWriter"),
            &imports(&["std::io::BufWriter", "std::fmt::Write"]),
        );
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
        let d = decide_by_receiver(alts(), &local(""), &imports(&["std::io", "std::io::Write"]));
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
            decide_by_receiver(alts(), &local("MyWriter"), &both),
            Decision::Undetermined { candidates: 2 }
        );
    }

    #[test]
    fn an_unknown_destination_with_no_trait_imported_is_ambiguous_across_all() {
        assert_eq!(
            decide_by_receiver(alts(), &local(""), &imports(&["std::collections::HashMap"])),
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

    /// The parser stores `use std::io::prelude::*` as the path without the
    /// `::*` (`rust_walker_tests`: `use a::b::*` is `a::b`), so the marker is
    /// the module path itself.
    #[test]
    fn a_glob_import_of_the_io_prelude_brings_io_write_into_scope() {
        let d = decide_by_receiver(alts(), &local(""), &imports(&["std::io::prelude"]));
        assert_eq!(
            d,
            Decision::Target {
                canonical: "std::io::Write::write_fmt",
                basis: Basis::ImportScope
            }
        );
    }

    #[test]
    fn a_destination_that_is_not_a_plain_local_is_undetermined_whatever_is_imported() {
        let dest = Destination {
            declared: "",
            plain_local: false,
            defined_in_file: false,
        };
        assert_eq!(
            decide_by_receiver(alts(), &dest, &imports(&["std::io::Write"])),
            Decision::Undetermined { candidates: 3 }
        );
    }

    fn dest(declared: &str, defined_in_file: bool) -> Destination<'_> {
        Destination {
            declared,
            plain_local: true,
            defined_in_file,
        }
    }

    #[test]
    fn a_type_defined_in_the_file_is_never_the_std_one() {
        assert_eq!(
            decide_by_receiver(
                alts(),
                &dest("Formatter", true),
                &imports(&["std::io::Write"])
            ),
            Decision::Undetermined { candidates: 3 }
        );
        assert_eq!(type_origin("Formatter", &[], true), TypeOrigin::Other);
    }

    #[test]
    fn a_std_name_imported_from_another_crate_is_not_std() {
        for (name, import) in [
            ("File", "tokio::fs::File"),
            ("Sink", "futures::Sink"),
            ("Cursor", "my_crate::io::Cursor"),
        ] {
            let imp = imports(&[import, "std::io::Write"]);
            assert_eq!(type_origin(name, &imp, false), TypeOrigin::Other, "{name}");
            assert_eq!(
                decide_by_receiver(alts(), &dest(name, false), &imp),
                Decision::Undetermined { candidates: 3 },
                "{name}"
            );
        }
    }

    #[test]
    fn a_std_name_imported_from_std_or_core_is_std() {
        let imp = imports(&["std::fs::File", "core::fmt::Formatter"]);
        assert_eq!(type_origin("File", &imp, false), TypeOrigin::Std);
        assert_eq!(type_origin("Formatter", &imp, false), TypeOrigin::Std);
    }

    #[test]
    fn a_path_is_resolved_by_its_root() {
        let fmt = imports(&["std::fmt"]);
        assert_eq!(type_origin("fmt::Formatter", &fmt, false), TypeOrigin::Std);
        assert_eq!(type_origin("std::fs::File", &[], false), TypeOrigin::Std);
        assert_eq!(
            type_origin("tokio::fs::File", &[], false),
            TypeOrigin::Other
        );
        assert_eq!(
            type_origin("fs::File", &imports(&["tokio::fs"]), false),
            TypeOrigin::Other
        );
    }

    #[test]
    fn a_prelude_name_is_std_unless_the_file_says_otherwise() {
        assert_eq!(type_origin("String", &[], false), TypeOrigin::Std);
        assert_eq!(type_origin("String", &[], true), TypeOrigin::Other);
        assert_eq!(
            type_origin("Vec", &imports(&["mylib::Vec"]), false),
            TypeOrigin::Other
        );
    }

    #[test]
    fn a_bare_std_name_that_nothing_in_the_file_places_is_undetermined() {
        let glob = imports(&["some_crate::io"]);
        assert_eq!(type_origin("File", &glob, false), TypeOrigin::Unknown);
        assert_eq!(
            decide_by_receiver(alts(), &dest("File", false), &glob),
            Decision::Undetermined { candidates: 3 }
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
