// macro_expansion::rust — Rust macro + derive expansion rules.
//
// source: https://doc.rust-lang.org/std/ and the Rust Reference
// https://doc.rust-lang.org/reference/macros-by-example.html. Each rule is
// either (a) a `name!(...)` invocation whose canonical lowering involves
// known std:: symbols, or (b) a `derive_<Trait>` marker fabricated by the
// parser when it encounters `#[derive(Trait)]`, whose lowering is an
// `impl Trait for Struct` and thus an Implements edge.

use super::{MacroExpansion, MacroTable};

pub struct RustMacros;

impl MacroTable for RustMacros {
    fn language(&self) -> &'static str {
        "rust"
    }
    fn expansions(&self) -> &'static [MacroExpansion] {
        RUST_MACROS
    }
}

// source: https://doc.rust-lang.org/std/macro.println.html (and siblings) —
// each macro's official expansion documented by the std crate. Derive
// markers come from https://doc.rust-lang.org/reference/attributes/derive.html
// and the Rust book chapter 19.6.
// source: https://doc.rust-lang.org/std/macro.write.html and
// https://doc.rust-lang.org/std/macro.vec.html — these macros have no fixed
// expansion: their target is decided by `dispatch` from the destination's type
// or the argument shape, so they carry no `emit_calls` entry here.
pub const DEST_MACROS: &[&str] = &["write", "writeln"];
pub const VEC_MACROS: &[&str] = &["vec"];

// source: issue #344, checked against the expansion of rustc 1.93 (nightly of
// 2025-11-20), 1.94, 1.95 (the toolchain this repository pins) and 1.98
// (`RUSTC_BOOTSTRAP=1 rustc -Zunpretty=expanded`) and the std source of 1.95
// (core/src/panic.rs, core/src/macros/mod.rs, std/src/macros.rs).
// The rule for the table: an entry names the ONE callee that every form of the
// macro reaches. These macros fail it. `panic!()` and `assert!(c)` call
// `core::panicking::panic`; `panic!("{}", x)` and `assert!(c, "{}", x)` call
// `core::panicking::panic_fmt` (`panic!` on edition 2015 and 2018 with a single
// non-literal argument calls another function again); `todo!()`,
// `unimplemented!()` and `unreachable!()` call `panic` with a fixed message and
// `panic_fmt` when given one. Listing one of them would name a callee the site
// may not call, so none is listed and the site says its callee depends on the
// arguments.
pub const FORM_DEPENDENT_MACROS: &[&str] = &[
    "panic",
    "assert",
    "debug_assert",
    "todo",
    "unimplemented",
    "unreachable",
];

// source: issue #345, checked against the same expansions (1.93 to 1.98) and
// core/src/macros/mod.rs, std/src/macros.rs of 1.95. Each expands to a `match`, a
// literal or a compiler constant and calls no function (`matches!` is a
// `match`; `include_str!`, `include_bytes!`, `concat!`, `stringify!`, `env!`,
// `option_env!`, `cfg!`, `line!`, `file!`, `column!` and `module_path!` are
// replaced by a literal at expansion; measured: `cfg!(unix)` is `true`,
// `option_env!("X")` is `None::<&'static str>`, `env!("X")` is a string
// literal). A site of one is not a call reference. `include!` is left out on
// purpose: it splices the code of another file into the caller, and that code
// can call.
pub const NO_CALL_MACROS: &[&str] = &[
    "matches",
    "include_str",
    "include_bytes",
    "concat",
    "stringify",
    "env",
    "option_env",
    "cfg",
    "line",
    "file",
    "column",
    "module_path",
];

// source: the 1.95 library sources, the same callees in the expansion of 1.93 to
// 1.98. Each path below is the callee every form of the macros that list it
// reaches (the rule above):
//   std::io::_print / _eprint  std/src/macros.rs: `print!` and `println!` call
//     `$crate::io::_print`; `eprint!`, `eprintln!` and `dbg!` call `_eprint`.
//   std::fmt::format  alloc/src/macros.rs: `format!` calls `$crate::fmt::format`.
//   core::panicking::assert_failed  core/src/macros/mod.rs: all four arms of
//     `assert_eq!`, `assert_ne!`, `debug_assert_eq!` and `debug_assert_ne!`
//     (with and without a message) call it, and no arm calls another function.
// A path may enter `RUST_MACROS` only through this list.
#[cfg(test)]
pub const VERIFIED_CALL_TARGETS: &[&str] = &[
    "std::io::_print",
    "std::io::_eprint",
    "std::fmt::format",
    "core::panicking::assert_failed",
];

pub const RUST_MACROS: &[MacroExpansion] = &[
    MacroExpansion {
        macro_name: "println",
        emit_calls: &["std::io::_print"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "eprintln",
        emit_calls: &["std::io::_eprint"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "print",
        emit_calls: &["std::io::_print"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "eprint",
        emit_calls: &["std::io::_eprint"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "format",
        emit_calls: &["std::fmt::format"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "assert_eq",
        emit_calls: &["core::panicking::assert_failed"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "assert_ne",
        emit_calls: &["core::panicking::assert_failed"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "debug_assert_eq",
        emit_calls: &["core::panicking::assert_failed"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "debug_assert_ne",
        emit_calls: &["core::panicking::assert_failed"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "dbg",
        emit_calls: &["std::io::_eprint"],
        emit_implements: &[],
        language: "rust",
    },
    // Derive markers — emit Implements edges, never Calls edges.
    // source: https://doc.rust-lang.org/reference/attributes/derive.html
    MacroExpansion {
        macro_name: "derive_Debug",
        emit_calls: &[],
        emit_implements: &["std::fmt::Debug"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_Clone",
        emit_calls: &[],
        emit_implements: &["std::clone::Clone"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_Copy",
        emit_calls: &[],
        emit_implements: &["std::marker::Copy"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_PartialEq",
        emit_calls: &[],
        emit_implements: &["std::cmp::PartialEq"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_Eq",
        emit_calls: &[],
        emit_implements: &["std::cmp::Eq"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_Hash",
        emit_calls: &[],
        emit_implements: &["std::hash::Hash"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_Default",
        emit_calls: &[],
        emit_implements: &["std::default::Default"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_PartialOrd",
        emit_calls: &[],
        emit_implements: &["std::cmp::PartialOrd"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_Ord",
        emit_calls: &[],
        emit_implements: &["std::cmp::Ord"],
        language: "rust",
    },
];
