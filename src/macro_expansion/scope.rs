// macro_expansion::scope — where a type name comes from, in one file
// (issue #339).
//
// The parser records a `write!` destination's type as written (`fmt::Formatter`,
// `File`, `F`). Whether that is the std type depends on the file: what it
// defines, and what its `use` items bind. A binding is a name and the path it
// stands for; `use tokio::fs::File as F` binds `F` to `tokio::fs::File`, and a
// glob binds no name. This module is pure: it answers from the bindings it is
// given.

/// One `use` item of a file: the name it binds and the path it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The alias when there is one, else the path's last segment; empty for a
    /// glob, which binds no name the parser can see.
    pub name: String,
    /// The path as written, without a glob's trailing `::*`.
    pub path: String,
}

impl Import {
    pub fn new(path: &str, alias: &str, is_glob: bool) -> Self {
        let name = if is_glob {
            String::new()
        } else if alias.is_empty() {
            last_segment(path).to_string()
        } else {
            alias.to_string()
        };
        Import {
            name,
            path: path.to_string(),
        }
    }
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
    /// Two bindings of the name, one std and one not (a `use` in an inline
    /// module beside one at the top of the file): which one the site sees is
    /// not recorded.
    Ambiguous,
    /// Nothing in the file says. A bare `File` may reach a file through a glob
    /// of a module whose members the parser does not store.
    Unknown,
}

// source: the Rust Reference, "Paths" and "Extern crates": `std`, `core` and
// `alloc` are the standard crates a path can start from.
const STD_ROOTS: [&str; 3] = ["std", "core", "alloc"];
// source: https://doc.rust-lang.org/std/prelude/v1/ — the std prelude names
// that select a `write_fmt` target (`String`, `Vec`).
const PRELUDE_TYPES: [&str; 2] = ["String", "Vec"];

/// Resolves a declared type in the scope of the file that holds the site:
/// a path by its root, a bare name by the file's own definitions, then the
/// bindings that carry the name, then the prelude.
pub fn type_origin(declared: &str, imports: &[Import], defined_in_file: bool) -> TypeOrigin {
    if declared.is_empty() {
        return TypeOrigin::Unknown;
    }
    if let Some((root, _)) = declared.split_once("::") {
        return if STD_ROOTS.contains(&root) {
            TypeOrigin::Std
        } else {
            bound_origin(root, imports).unwrap_or(TypeOrigin::Other)
        };
    }
    if defined_in_file {
        return TypeOrigin::Other;
    }
    match bound_origin(declared, imports) {
        Some(origin) => origin,
        None if PRELUDE_TYPES.contains(&declared) => TypeOrigin::Std,
        None => TypeOrigin::Unknown,
    }
}

/// The origin of the bindings of `name`: `None` when nothing binds it, and
/// `Ambiguous` when its bindings do not agree.
fn bound_origin(name: &str, imports: &[Import]) -> Option<TypeOrigin> {
    let mut origins = imports
        .iter()
        .filter(|i| i.name == name)
        .map(|i| is_std_path(&i.path));
    let first = origins.next()?;
    if origins.any(|o| o != first) {
        return Some(TypeOrigin::Ambiguous);
    }
    Some(if first {
        TypeOrigin::Std
    } else {
        TypeOrigin::Other
    })
}

/// The type's own name: for a bare name bound by a `use`, the last segment of
/// the path it stands for (so `SF` of `use std::fs::File as SF` is `File`);
/// otherwise the last segment of what was written.
pub fn original_name<'a>(declared: &'a str, imports: &'a [Import]) -> &'a str {
    if !declared.contains("::") {
        if let Some(import) = imports.iter().find(|i| i.name == declared) {
            return last_segment(&import.path);
        }
    }
    last_segment(declared)
}

fn is_std_path(path: &str) -> bool {
    path.split("::")
        .next()
        .is_some_and(|r| STD_ROOTS.contains(&r))
}

/// The last `::` segment of a path.
pub fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imp(path: &str) -> Import {
        Import::new(path, "", false)
    }

    fn alias(path: &str, name: &str) -> Import {
        Import::new(path, name, false)
    }

    #[test]
    fn a_type_defined_in_the_file_is_never_the_std_one() {
        assert_eq!(type_origin("Formatter", &[], true), TypeOrigin::Other);
    }

    #[test]
    fn a_std_name_imported_from_another_crate_is_not_std() {
        for path in ["tokio::fs::File", "futures::Sink", "my_crate::io::Cursor"] {
            let name = last_segment(path);
            assert_eq!(type_origin(name, &[imp(path)], false), TypeOrigin::Other);
        }
    }

    #[test]
    fn a_std_name_imported_from_std_or_core_is_std() {
        let imports = [imp("std::fs::File"), imp("core::fmt::Formatter")];
        assert_eq!(type_origin("File", &imports, false), TypeOrigin::Std);
        assert_eq!(type_origin("Formatter", &imports, false), TypeOrigin::Std);
    }

    #[test]
    fn an_alias_resolves_through_its_original_path() {
        let imports = [alias("std::fs::File", "SF"), alias("tokio::fs::File", "F")];
        assert_eq!(type_origin("SF", &imports, false), TypeOrigin::Std);
        assert_eq!(type_origin("F", &imports, false), TypeOrigin::Other);
        assert_eq!(
            type_origin("File", &imports, false),
            TypeOrigin::Unknown,
            "an alias hides the original name"
        );
    }

    #[test]
    fn the_original_name_of_an_alias_is_the_last_segment_of_its_path() {
        let imports = [alias("std::fs::File", "SF")];
        assert_eq!(original_name("SF", &imports), "File");
        assert_eq!(original_name("fmt::Formatter", &imports), "Formatter");
        assert_eq!(original_name("Vec", &imports), "Vec");
    }

    #[test]
    fn a_path_is_resolved_by_its_root() {
        assert_eq!(
            type_origin("fmt::Formatter", &[imp("std::fmt")], false),
            TypeOrigin::Std
        );
        assert_eq!(type_origin("std::fs::File", &[], false), TypeOrigin::Std);
        assert_eq!(
            type_origin("tokio::fs::File", &[], false),
            TypeOrigin::Other
        );
        assert_eq!(
            type_origin("fs::File", &[imp("tokio::fs")], false),
            TypeOrigin::Other
        );
    }

    #[test]
    fn a_prelude_name_is_std_unless_the_file_says_otherwise() {
        assert_eq!(type_origin("String", &[], false), TypeOrigin::Std);
        assert_eq!(type_origin("String", &[], true), TypeOrigin::Other);
        assert_eq!(
            type_origin("Vec", &[imp("mylib::Vec")], false),
            TypeOrigin::Other
        );
    }

    #[test]
    fn a_bare_name_that_nothing_in_the_file_places_is_unknown() {
        let glob = [Import::new("some_crate::io", "", true)];
        assert_eq!(type_origin("File", &glob, false), TypeOrigin::Unknown);
    }

    #[test]
    fn a_name_bound_to_std_and_to_another_crate_is_ambiguous() {
        let imports = [imp("std::fs::File"), imp("tokio::fs::File")];
        assert_eq!(type_origin("File", &imports, false), TypeOrigin::Ambiguous);
        let twice = [imp("std::fs::File"), imp("std::fs::File")];
        assert_eq!(type_origin("File", &twice, false), TypeOrigin::Std);
    }
}
