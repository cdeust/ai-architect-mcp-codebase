// parser::spec::rust_parity_corpus — the Rust parity CORPUS and the ground-truth
// records captured from the hand-written walker before it was deleted
// (ADR-0055 phase 8). Data only: no assertions live here, so the pin's evidence
// and the pin's logic are separable and each file stays under the 500-line cap
// (§4.1). The assertions are in `rust_parity_tests`.
//
// data table (mechanically captured): the `expected_*` bodies below were printed
// by a temporary harness running `parse_file` on `CORPUS` — 62 nodes, 67 refs,
// 0 parse errors — not written by hand. The pre-migration capture was 61 nodes /
// 66 refs; #130 and #131 changed it deliberately (behavior-changing fixes, not a
// rebaseline): #131 ADDED one `CallSite` + one `Defines` under
// `Handler::describe` (net +1 node, +1 ref), and #130 RE-SCOPED `Inner::toggle`'s
// QN and its `HasMethod` edge from the file to the `helpers` module (0 net,
// changed in place). What each record pins, construct by construct, is documented
// in `rust_parity_tests`.

pub(super) const CORPUS: &str = r##"extern crate serde;

use std::collections::HashMap;
use std::collections::{HashSet, BTreeMap};
use serde::{Deserialize, Serialize as Ser};
use std::io::{self, BufRead};
use a::b::*;
use a::{b, c::{d, e}};
use n::{m::*, o};
use core::fmt as format_mod;

pub const MAX: usize = 42;
static GREETING: &str = "hi";
pub(crate) type Alias = Vec<String>;

macro_rules! shout {
    ($x:expr) => { $x };
}

#[derive(Debug, Clone)]
// a comment between the derive and the item must not clear pending derives
pub struct Config {
    pub name: String,
    retries: u32,
}

pub struct Wrapper<T>(T);

pub struct Unit;

pub union Value {
    int_val: i64,
    float_val: f64,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum State {
    Idle,
    Running(u32),
    Failed { code: i32 },
}

pub trait Handler: Clone + Send {
    fn handle(&self, input: &str) -> bool;
    fn describe(&self) -> String {
        String::from("handler")
    }
}

impl Config {
    pub fn new(name: String) -> Self {
        Config { name, retries: 0 }
    }

    pub(crate) async fn reload(&mut self) -> bool {
        self.retries += 1;
        validate(&self.name)
    }
}

impl Handler for Config {
    fn handle(&self, input: &str) -> bool {
        let f = |s: &str| s.len();
        shout!(f(input));
        helpers::normalize(input);
        run_all(validate, input)
    }
}

impl<T> Wrapper<T> {
    fn get(&self) -> &T {
        &self.0
    }
}

pub async fn validate(name: &str) -> bool {
    !name.is_empty()
}

fn run_all(check: fn(&str) -> bool, arg: &str) -> bool {
    check(arg)
}

pub(super) mod helpers {
    use super::Config;

    pub const LIMIT: u8 = 7;

    pub fn normalize(input: &str) -> String {
        input.trim().to_string()
    }

    pub struct Inner {
        pub flag: bool,
    }

    impl Inner {
        fn toggle(&mut self) {
            self.flag = !self.flag;
        }
    }
}

mod declared_elsewhere;
"##;

pub(super) const PATH: &str = "src/lib.rs";

/// The hand-written walker's node records on `CORPUS`, in emission order.
pub(super) fn expected_node_records() -> Vec<&'static str> {
    let mut v = Vec::new();
    v.extend(expected_node_records_part0());
    v.extend(expected_node_records_part1());
    v.extend(expected_node_records_part2());
    v
}

fn expected_node_records_part0() -> Vec<&'static str> {
    vec![
        "Import|serde|src/lib.rs::import:serde|1|1||[(\"path\", \"serde\")]",
        "Import|std::collections::HashMap|src/lib.rs::std::collections::HashMap|3|3||[(\"path\", \"std::collections::HashMap\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|std::collections::HashSet|src/lib.rs::std::collections::HashSet|4|4||[(\"path\", \"std::collections::HashSet\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|std::collections::BTreeMap|src/lib.rs::std::collections::BTreeMap|4|4||[(\"path\", \"std::collections::BTreeMap\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|serde::Deserialize|src/lib.rs::serde::Deserialize|5|5||[(\"path\", \"serde::Deserialize\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|Ser|src/lib.rs::Ser|5|5||[(\"path\", \"serde::Serialize\"), (\"alias\", \"Ser\"), (\"is_glob\", \"false\")]",
        "Import|std::io|src/lib.rs::std::io|6|6||[(\"path\", \"std::io\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|std::io::BufRead|src/lib.rs::std::io::BufRead|6|6||[(\"path\", \"std::io::BufRead\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|a::b::*|src/lib.rs::a::b::*|7|7||[(\"path\", \"a::b\"), (\"alias\", \"\"), (\"is_glob\", \"true\")]",
        "Import|a::b|src/lib.rs::a::b|8|8||[(\"path\", \"a::b\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|a::c::d|src/lib.rs::a::c::d|8|8||[(\"path\", \"a::c::d\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|a::c::e|src/lib.rs::a::c::e|8|8||[(\"path\", \"a::c::e\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|n::m::*|src/lib.rs::n::m::*|9|9||[(\"path\", \"n::m\"), (\"alias\", \"\"), (\"is_glob\", \"true\")]",
        "Import|n::o|src/lib.rs::n::o|9|9||[(\"path\", \"n::o\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|format_mod|src/lib.rs::format_mod|10|10||[(\"path\", \"core::fmt\"), (\"alias\", \"format_mod\"), (\"is_glob\", \"false\")]",
        "Constant|MAX|src/lib.rs::MAX|12|12|pub|[(\"type_annotation\", \"usize\")]",
        "Constant|GREETING|src/lib.rs::GREETING|13|13||[(\"type_annotation\", \"&str\")]",
        "TypeAlias|Alias|src/lib.rs::Alias|14|14|pub(crate)|[(\"target_type\", \"Vec<String>\")]",
        "Constant|shout|src/lib.rs::shout|16|18||[(\"is_macro\", \"true\")]",
        "Struct|Config|src/lib.rs::Config|22|25|pub|[(\"implements\", \"Debug,Clone\")]",
        "Field|name|src/lib.rs::Config::name|23|23|pub|[(\"type_annotation\", \"String\")]",
        "Field|retries|src/lib.rs::Config::retries|24|24||[(\"type_annotation\", \"u32\")]",
        "Struct|Wrapper|src/lib.rs::Wrapper|27|27|pub|[]",
        "Struct|Unit|src/lib.rs::Unit|29|29|pub|[]",
        "Struct|Value|src/lib.rs::Value|31|34|pub|[]",
        "Field|int_val|src/lib.rs::Value::int_val|32|32||[(\"type_annotation\", \"i64\")]",
        "Field|float_val|src/lib.rs::Value::float_val|33|33||[(\"type_annotation\", \"f64\")]",
        "Enum|State|src/lib.rs::State|38|42|pub|[(\"implements\", \"Debug\")]",
        "Variant|Idle|src/lib.rs::State::Idle|39|39||[]",
        "Variant|Running|src/lib.rs::State::Running|40|40||[]",
    ]
}

fn expected_node_records_part1() -> Vec<&'static str> {
    vec![
        "Variant|Failed|src/lib.rs::State::Failed|41|41||[]",
        "Trait|Handler|src/lib.rs::Handler|44|49|pub|[(\"supertraits\", \"Clone,Send\")]",
        "Method|handle|src/lib.rs::Handler::handle|45|45||[(\"is_async\", \"false\"), (\"receiver_type\", \"src/lib.rs::Handler\"), (\"return_type\", \"bool\")]",
        "Method|describe|src/lib.rs::Handler::describe|46|48||[(\"is_async\", \"false\"), (\"receiver_type\", \"src/lib.rs::Handler\"), (\"return_type\", \"String\")]",
        // #131: the defaulted `describe`'s body is now scanned — `String::from` is
        // its one call site, keyed by the method's own QN.
        "CallSite|String::from|src/lib.rs::Handler::describe::call@47:8#893-916|47|47||[(\"callee_name\", \"String::from\"), (\"caller_qn\", \"src/lib.rs::Handler::describe\"), (\"lsp_col\", \"8\")]",
        "Method|new|src/lib.rs::Config::new|52|54|pub|[(\"is_async\", \"false\"), (\"receiver_type\", \"src/lib.rs::Config\"), (\"return_type\", \"Self\"), (\"constructed_types\", \"Config\")]",
        "Method|reload|src/lib.rs::Config::reload|56|59|pub(crate)|[(\"is_async\", \"true\"), (\"receiver_type\", \"src/lib.rs::Config\"), (\"return_type\", \"bool\")]",
        "CallSite|validate|src/lib.rs::Config::reload::call@58:8#1109-1129|58|58||[(\"callee_name\", \"validate\"), (\"caller_qn\", \"src/lib.rs::Config::reload\"), (\"lsp_col\", \"8\")]",
        "Method|handle|src/lib.rs::Config::handle|63|68||[(\"is_async\", \"false\"), (\"receiver_type\", \"src/lib.rs::Config\"), (\"trait_name\", \"Handler\"), (\"return_type\", \"bool\")]",
        "CallSite|run_all|src/lib.rs::Config::handle::call@67:8#1313-1337|67|67||[(\"callee_name\", \"run_all\"), (\"caller_qn\", \"src/lib.rs::Config::handle\"), (\"lsp_col\", \"8\")]",
        "CallSite|validate|src/lib.rs::Config::handle::call@67:16#1321-1329|67|67||[(\"callee_name\", \"validate\"), (\"caller_qn\", \"src/lib.rs::Config::handle\"), (\"lsp_col\", \"16\")]",
        "CallSite|input|src/lib.rs::Config::handle::call@67:26#1331-1336|67|67||[(\"callee_name\", \"input\"), (\"caller_qn\", \"src/lib.rs::Config::handle\"), (\"lsp_col\", \"26\")]",
        "CallSite|helpers::normalize|src/lib.rs::Config::handle::call@66:8#1278-1303|66|66||[(\"callee_name\", \"helpers::normalize\"), (\"caller_qn\", \"src/lib.rs::Config::handle\"), (\"lsp_col\", \"8\")]",
        "CallSite|input|src/lib.rs::Config::handle::call@66:27#1297-1302|66|66||[(\"callee_name\", \"input\"), (\"caller_qn\", \"src/lib.rs::Config::handle\"), (\"lsp_col\", \"27\")]",
        "CallSite|shout!|src/lib.rs::Config::handle::call@65:8#1252-1268|65|65||[(\"callee_name\", \"shout!\"), (\"caller_qn\", \"src/lib.rs::Config::handle\"), (\"lsp_col\", \"8\")]",
        "CallSite|s.len|src/lib.rs::Config::handle::call@64:26#1235-1242|64|64||[(\"callee_name\", \"s.len\"), (\"caller_qn\", \"src/lib.rs::Config::handle\"), (\"lsp_col\", \"26\")]",
        "Method|get|src/lib.rs::Wrapper<T>::get|72|74||[(\"is_async\", \"false\"), (\"receiver_type\", \"src/lib.rs::Wrapper<T>\"), (\"return_type\", \"&T\")]",
        "Function|validate|src/lib.rs::validate|77|79|pub|[(\"is_async\", \"true\"), (\"return_type\", \"bool\")]",
        "CallSite|name.is_empty|src/lib.rs::validate::call@78:5#1468-1483|78|78||[(\"callee_name\", \"name.is_empty\"), (\"caller_qn\", \"src/lib.rs::validate\"), (\"lsp_col\", \"5\")]",
        "Function|run_all|src/lib.rs::run_all|81|83||[(\"is_async\", \"false\"), (\"return_type\", \"bool\")]",
        "CallSite|check|src/lib.rs::run_all::call@82:4#1548-1558|82|82||[(\"callee_name\", \"check\"), (\"caller_qn\", \"src/lib.rs::run_all\"), (\"lsp_col\", \"4\")]",
        "CallSite|arg|src/lib.rs::run_all::call@82:10#1554-1557|82|82||[(\"callee_name\", \"arg\"), (\"caller_qn\", \"src/lib.rs::run_all\"), (\"lsp_col\", \"10\")]",
        "Module|helpers|src/lib.rs::helpers|85|103|pub(super)|[]",
        "Import|super::Config|src/lib.rs::helpers::super::Config|86|86||[(\"path\", \"super::Config\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Constant|LIMIT|src/lib.rs::helpers::LIMIT|88|88|pub|[(\"type_annotation\", \"u8\")]",
        "Function|normalize|src/lib.rs::helpers::normalize|90|92|pub|[(\"is_async\", \"false\"), (\"return_type\", \"String\")]",
        "CallSite|input.trim().to_string|src/lib.rs::helpers::normalize::call@91:8#1695-1719|91|91||[(\"callee_name\", \"input.trim().to_string\"), (\"caller_qn\", \"src/lib.rs::helpers::normalize\"), (\"lsp_col\", \"8\")]",
        "CallSite|input.trim|src/lib.rs::helpers::normalize::call@91:8#1695-1707|91|91||[(\"callee_name\", \"input.trim\"), (\"caller_qn\", \"src/lib.rs::helpers::normalize\"), (\"lsp_col\", \"8\")]",
    ]
}

fn expected_node_records_part2() -> Vec<&'static str> {
    vec![
        "Struct|Inner|src/lib.rs::helpers::Inner|94|96|pub|[]",
        "Field|flag|src/lib.rs::helpers::Inner::flag|95|95|pub|[(\"type_annotation\", \"bool\")]",
        // #130: a module-nested `impl`'s method is now scoped to the module, so
        // its QN and receiver_type carry `helpers::` and match the `Struct` node's
        // own QN (`src/lib.rs::helpers::Inner`).
        "Method|toggle|src/lib.rs::helpers::Inner::toggle|99|101||[(\"is_async\", \"false\"), (\"receiver_type\", \"src/lib.rs::helpers::Inner\")]",
        "Module|declared_elsewhere|src/lib.rs::declared_elsewhere|105|105||[]",
    ]
}

/// The hand-written walker's ref triples on `CORPUS`, in emission order.
pub(super) fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut v = Vec::new();
    v.extend(expected_refs_part0());
    v.extend(expected_refs_part1());
    v.extend(expected_refs_part2());
    v.extend(expected_refs_part3());
    v.extend(expected_refs_part4());
    v.extend(expected_refs_part5());
    v.extend(expected_refs_part6());
    v.extend(expected_refs_part7());
    v
}

fn expected_refs_part0() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Imports", "src/lib.rs", "serde"),
        (
            "Defines",
            "src/lib.rs",
            "src/lib.rs::std::collections::HashMap",
        ),
        (
            "Defines",
            "src/lib.rs",
            "src/lib.rs::std::collections::HashSet",
        ),
        (
            "Defines",
            "src/lib.rs",
            "src/lib.rs::std::collections::BTreeMap",
        ),
        ("Defines", "src/lib.rs", "src/lib.rs::serde::Deserialize"),
        ("Defines", "src/lib.rs", "src/lib.rs::Ser"),
        ("Defines", "src/lib.rs", "src/lib.rs::std::io"),
        ("Defines", "src/lib.rs", "src/lib.rs::std::io::BufRead"),
        ("Defines", "src/lib.rs", "src/lib.rs::a::b::*"),
        ("Defines", "src/lib.rs", "src/lib.rs::a::b"),
        ("Defines", "src/lib.rs", "src/lib.rs::a::c::d"),
        ("Defines", "src/lib.rs", "src/lib.rs::a::c::e"),
        ("Defines", "src/lib.rs", "src/lib.rs::n::m::*"),
        ("Defines", "src/lib.rs", "src/lib.rs::n::o"),
        ("Defines", "src/lib.rs", "src/lib.rs::format_mod"),
        ("Defines", "src/lib.rs", "src/lib.rs::MAX"),
        ("Defines", "src/lib.rs", "src/lib.rs::GREETING"),
        ("Defines", "src/lib.rs", "src/lib.rs::Alias"),
    ]
}

fn expected_refs_part1() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Defines", "src/lib.rs", "src/lib.rs::shout"),
        ("Defines", "src/lib.rs", "src/lib.rs::Config"),
        ("HasField", "src/lib.rs::Config", "src/lib.rs::Config::name"),
        (
            "HasField",
            "src/lib.rs::Config",
            "src/lib.rs::Config::retries",
        ),
        ("DeriveImplements", "src/lib.rs::Config", "Debug"),
        ("DeriveImplements", "src/lib.rs::Config", "Clone"),
        ("Defines", "src/lib.rs", "src/lib.rs::Wrapper"),
        ("Defines", "src/lib.rs", "src/lib.rs::Unit"),
        ("Defines", "src/lib.rs", "src/lib.rs::Value"),
        (
            "HasField",
            "src/lib.rs::Value",
            "src/lib.rs::Value::int_val",
        ),
        (
            "HasField",
            "src/lib.rs::Value",
            "src/lib.rs::Value::float_val",
        ),
        ("Defines", "src/lib.rs", "src/lib.rs::State"),
        ("HasVariant", "src/lib.rs::State", "src/lib.rs::State::Idle"),
        (
            "HasVariant",
            "src/lib.rs::State",
            "src/lib.rs::State::Running",
        ),
    ]
}

fn expected_refs_part2() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "HasVariant",
            "src/lib.rs::State",
            "src/lib.rs::State::Failed",
        ),
        ("DeriveImplements", "src/lib.rs::State", "Debug"),
        ("Defines", "src/lib.rs", "src/lib.rs::Handler"),
        ("Extends", "src/lib.rs::Handler", "Clone"),
        ("Extends", "src/lib.rs::Handler", "Send"),
        (
            "HasMethod",
            "src/lib.rs::Handler",
            "src/lib.rs::Handler::handle",
        ),
        (
            "HasMethod",
            "src/lib.rs::Handler",
            "src/lib.rs::Handler::describe",
        ),
        // #131: the call site `String::from` inside `describe`'s default body,
        // owned by the method via a `Defines` edge exactly as an impl method's is.
        (
            "Defines",
            "src/lib.rs::Handler::describe",
            "src/lib.rs::Handler::describe::call@47:8#893-916",
        ),
        ("HasMethod", "src/lib.rs::Config", "src/lib.rs::Config::new"),
    ]
}

fn expected_refs_part3() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "HasMethod",
            "src/lib.rs::Config",
            "src/lib.rs::Config::reload",
        ),
        (
            "Defines",
            "src/lib.rs::Config::reload",
            "src/lib.rs::Config::reload::call@58:8#1109-1129",
        ),
        (
            "HasMethod",
            "src/lib.rs::Config",
            "src/lib.rs::Config::handle",
        ),
        (
            "Defines",
            "src/lib.rs::Config::handle",
            "src/lib.rs::Config::handle::call@67:8#1313-1337",
        ),
        (
            "Defines",
            "src/lib.rs::Config::handle",
            "src/lib.rs::Config::handle::call@67:16#1321-1329",
        ),
        (
            "Defines",
            "src/lib.rs::Config::handle",
            "src/lib.rs::Config::handle::call@67:26#1331-1336",
        ),
    ]
}

fn expected_refs_part4() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Defines",
            "src/lib.rs::Config::handle",
            "src/lib.rs::Config::handle::call@66:8#1278-1303",
        ),
        (
            "Defines",
            "src/lib.rs::Config::handle",
            "src/lib.rs::Config::handle::call@66:27#1297-1302",
        ),
        (
            "Defines",
            "src/lib.rs::Config::handle",
            "src/lib.rs::Config::handle::call@65:8#1252-1268",
        ),
        (
            "Defines",
            "src/lib.rs::Config::handle",
            "src/lib.rs::Config::handle::call@64:26#1235-1242",
        ),
        (
            "HasMethod",
            "src/lib.rs::Wrapper<T>",
            "src/lib.rs::Wrapper<T>::get",
        ),
        ("Defines", "src/lib.rs", "src/lib.rs::validate"),
    ]
}

fn expected_refs_part5() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Defines",
            "src/lib.rs::validate",
            "src/lib.rs::validate::call@78:5#1468-1483",
        ),
        ("Defines", "src/lib.rs", "src/lib.rs::run_all"),
        (
            "Defines",
            "src/lib.rs::run_all",
            "src/lib.rs::run_all::call@82:4#1548-1558",
        ),
        (
            "Defines",
            "src/lib.rs::run_all",
            "src/lib.rs::run_all::call@82:10#1554-1557",
        ),
        ("Defines", "src/lib.rs", "src/lib.rs::helpers"),
        (
            "Defines",
            "src/lib.rs::helpers",
            "src/lib.rs::helpers::super::Config",
        ),
        (
            "Defines",
            "src/lib.rs::helpers",
            "src/lib.rs::helpers::LIMIT",
        ),
    ]
}

fn expected_refs_part6() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Defines",
            "src/lib.rs::helpers",
            "src/lib.rs::helpers::normalize",
        ),
        (
            "Defines",
            "src/lib.rs::helpers::normalize",
            "src/lib.rs::helpers::normalize::call@91:8#1695-1719",
        ),
        (
            "Defines",
            "src/lib.rs::helpers::normalize",
            "src/lib.rs::helpers::normalize::call@91:8#1695-1707",
        ),
        (
            "Defines",
            "src/lib.rs::helpers",
            "src/lib.rs::helpers::Inner",
        ),
        (
            "HasField",
            "src/lib.rs::helpers::Inner",
            "src/lib.rs::helpers::Inner::flag",
        ),
        // #130: the `HasMethod` edge now originates from the module-scoped
        // receiver QN the `Struct` node declares, not the file-scoped one.
    ]
}

fn expected_refs_part7() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "HasMethod",
            "src/lib.rs::helpers::Inner",
            "src/lib.rs::helpers::Inner::toggle",
        ),
        ("Defines", "src/lib.rs", "src/lib.rs::declared_elsewhere"),
    ]
}
