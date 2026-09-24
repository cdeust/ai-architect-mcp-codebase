// macro_expansion — Layer 4 of the stage-3b-v2 resolver.
//
// Language-neutral macro / decorator / intrinsic expansion tables. Each
// language contributes rules that map a name-triggered site (a macro
// invocation, a decorator, a derive marker) to the set of canonical symbols
// it implicitly references. The parser emits synthetic ExtractedRefs
// (kind = "Calls" or "Implements") using the canonical path as the target,
// which the resolver then wires to StdlibSymbol nodes at confidence 0.85.
//
// source: stages/stage-3b-v2.md §5 (Layer 4 — universal strategy,
// per-language expansion data).

pub mod dispatch;
pub mod python;
pub mod rust;
pub mod scope;
pub mod typescript;

/// One expansion rule. `emit_calls` is the canonical-path set implied by a
/// call site; `emit_implements` is the canonical-trait set implied by a
/// derive/decorator marker.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // language is inspected by future multi-lang dispatch.
pub struct MacroExpansion {
    pub macro_name: &'static str,
    pub emit_calls: &'static [&'static str],
    pub emit_implements: &'static [&'static str],
    pub language: &'static str,
}

#[allow(dead_code)] // language() is documentation for future dispatch.
pub trait MacroTable: Send + Sync {
    fn language(&self) -> &'static str;
    fn expansions(&self) -> &'static [MacroExpansion];
}

pub fn get_macro_table(language: &str) -> Option<&'static dyn MacroTable> {
    match language {
        "rust" => Some(&rust::RustMacros),
        "python" => Some(&python::PythonMacros),
        "typescript" => Some(&typescript::TypeScriptMacros),
        _ => None,
    }
}

/// Lookup by macro name. O(n) scan; tables are small.
pub fn lookup(language: &str, macro_name: &str) -> Option<&'static MacroExpansion> {
    let table = get_macro_table(language)?;
    table
        .expansions()
        .iter()
        .find(|e| e.macro_name == macro_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table keeps both kinds of entry: the macros whose every form calls
    /// one verified function, and the derive markers (issue #344 removed the
    /// entries whose callee depends on the arguments, so no total is pinned).
    #[test]
    fn test_rust_macro_count() {
        let all = rust::RustMacros.expansions();
        let derives = all.iter().filter(|e| !e.emit_implements.is_empty()).count();
        let calls = all.iter().filter(|e| !e.emit_calls.is_empty()).count();
        assert!(derives >= 9, "derive markers: {derives}");
        assert!(calls >= 10, "listed call macros: {calls}");
    }

    #[test]
    fn test_lookup_println() {
        let exp = lookup("rust", "println").expect("println macro must be indexed");
        assert!(exp.emit_calls.contains(&"std::io::_print"));
    }

    /// An `emit_calls` set lists calls the expansion makes all together, so two
    /// paths ending in the same method name are two candidates for one call
    /// (`fmt::Write::write_fmt` and `io::Write::write_fmt`), never both made.
    /// That shape belongs in `dispatch`, not here.
    #[test]
    fn no_emit_calls_set_lists_the_same_method_twice() {
        for lang in ["rust", "python", "typescript"] {
            let table = get_macro_table(lang).expect("table");
            for exp in table.expansions() {
                let mut seen = std::collections::HashSet::new();
                for path in exp.emit_calls {
                    let method = path.rsplit("::").next().unwrap_or(path);
                    assert!(
                        seen.insert(method),
                        "{lang} {}: two targets end in `{method}`",
                        exp.macro_name
                    );
                }
            }
        }
    }

    /// Issue #344: a target enters the Rust table only through the list of
    /// callees verified in the std sources, so an internal that changed between
    /// versions (`Arguments::new_v1`) cannot come back unnoticed.
    #[test]
    fn every_rust_emit_call_is_a_verified_call_target() {
        for exp in rust::RustMacros.expansions() {
            for path in exp.emit_calls {
                assert!(
                    rust::VERIFIED_CALL_TARGETS.contains(path),
                    "{}: `{path}` is not in VERIFIED_CALL_TARGETS",
                    exp.macro_name
                );
            }
        }
    }

    /// The four comparison assert macros call `assert_failed` whatever their
    /// arguments (core/src/macros/mod.rs); the others of the family and `panic!`
    /// call `panic` or `panic_fmt` by arguments, so they list no target.
    #[test]
    fn the_assert_family_lists_a_target_only_where_every_form_calls_it() {
        for name in [
            "assert_eq",
            "assert_ne",
            "debug_assert_eq",
            "debug_assert_ne",
        ] {
            let exp = lookup("rust", name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(exp.emit_calls, ["core::panicking::assert_failed"], "{name}");
        }
        for name in rust::FORM_DEPENDENT_MACROS {
            assert!(lookup("rust", name).is_none(), "{name} must list no target");
        }
    }

    /// A macro is in one class only: listed, decided, form dependent or no-call.
    #[test]
    fn a_rust_macro_belongs_to_one_class() {
        let listed: Vec<&str> = rust::RustMacros
            .expansions()
            .iter()
            .filter(|e| !e.emit_calls.is_empty())
            .map(|e| e.macro_name)
            .collect();
        let classes: [(&str, &[&str]); 4] = [
            ("listed", &listed),
            ("dest", rust::DEST_MACROS),
            ("vec", rust::VEC_MACROS),
            ("form dependent", rust::FORM_DEPENDENT_MACROS),
        ];
        let mut seen = std::collections::HashSet::new();
        for (class, names) in classes
            .iter()
            .chain([("no call", rust::NO_CALL_MACROS)].iter())
        {
            for name in *names {
                assert!(seen.insert(*name), "{name} is in two classes ({class})");
            }
        }
    }

    #[test]
    fn test_derive_debug_implements() {
        let exp = lookup("rust", "derive_Debug").expect("derive_Debug must be indexed");
        assert!(exp.emit_implements.contains(&"std::fmt::Debug"));
        assert!(exp.emit_calls.is_empty());
    }
}
