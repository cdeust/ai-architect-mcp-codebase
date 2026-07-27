// parser::spec::c_parity_expected — the pinned pre-migration ground truth
// for the C parity suite (ADR-0055 §5 step 3).
//
// These records ARE the hand-written walker's output, captured before it was
// deleted: one full 7-tuple per node, one full triple per ref. Split out of
// `c_parity_tests.rs` (which kept the corpus, the record helpers and the
// assertions) purely to hold both files inside the §4.1 500-line cap — the
// pinned data is a table, the assertions are logic, and they change for
// different reasons. A pure move: every string is byte-for-byte the one the
// capture produced, which the unchanged parity assertions prove.

pub(super) fn expected_node_records() -> Vec<&'static str> {
    vec![
        "CallSite|_internal|app/main.c::add#3::call@46:5#4|46|46|public|[(\"callee_name\", \"_internal\")]",
        "CallSite|call|app/main.c::add#3::call@45:5#5|45|45|public|[(\"callee_name\", \"call\")]",
        "CallSite|dbg|app/main.c::gated#11::call@62:5#12|62|62|public|[(\"callee_name\", \"dbg\")]",
        "CallSite|helper|app/main.c::add#3::call@42:13#8|42|42|public|[(\"callee_name\", \"helper\")]",
        "CallSite|method|app/main.c::add#3::call@44:5#6|44|44|public|[(\"callee_name\", \"method\")]",
        "CallSite|printf|app/main.c::add#3::call@43:5#7|43|43|public|[(\"callee_name\", \"printf\")]",
        "Constant|BLUE|app/main.c::Color::BLUE|26|26|public|[(\"enum_entry\", \"true\")]",
        // issue #107 — object-like macro
        "Constant|MAX|app/main.c::MAX|5|6|public|[(\"macro\", \"true\")]",
        // issue #107 — anonymous struct named by its typedef alias, with fields
        "Struct|Anon|app/main.c::Anon|70|73|public|[]",
        "Field|ax|app/main.c::Anon::ax|71|71|public|[(\"type_annotation\", \"int\")]",
        "Field|ay|app/main.c::Anon::ay|72|72|public|[(\"type_annotation\", \"int\")]",
        // issue #107 — struct defined inline in a declaration
        "Struct|Tagged|app/main.c::Tagged|75|77|public|[]",
        "Field|tv|app/main.c::Tagged::tv|76|76|public|[(\"type_annotation\", \"int\")]",
        "Constant|Callback|app/main.c::Callback|31|31|public|[(\"typedef\", \"true\")]",
        "Constant|GREEN|app/main.c::Color::GREEN|25|25|public|[(\"enum_entry\", \"true\")]",
        "Constant|PointT|app/main.c::PointT|29|29|public|[(\"typedef\", \"true\")]",
        "Constant|RED|app/main.c::Color::RED|24|24|public|[(\"enum_entry\", \"true\")]",
        "Constant|ulong_t|app/main.c::ulong_t|30|30|public|[(\"typedef\", \"true\")]",
        "Enum|Color|app/main.c::Color|23|27|public|[]",
        "Field|buf|app/main.c::Point::buf|13|13|public|[(\"type_annotation\", \"char\")]",
        "Field|f|app/main.c::Value::f|20|20|public|[(\"type_annotation\", \"float\")]",
        "Field|flag|app/main.c::Gated::flag|59|59|public|[(\"type_annotation\", \"int\")]",
        "Field|handler|app/main.c::Point::handler|14|14|public|[(\"type_annotation\", \"int\")]",
        "Field|height|app/main.c::Point::height|11|11|public|[(\"type_annotation\", \"int\")]",
        "Field|i|app/main.c::Value::i|19|19|public|[(\"type_annotation\", \"int\")]",
        "Field|name|app/main.c::Point::name|12|12|public|[(\"type_annotation\", \"char\")]",
        "Field|next|app/main.c::Point::next|15|15|public|[(\"type_annotation\", \"struct Point\")]",
        "Field|width|app/main.c::Point::width|11|11|public|[(\"type_annotation\", \"int\")]",
        "Field|x|app/main.c::Point::x|9|9|public|[(\"type_annotation\", \"int\")]",
        "Field|y|app/main.c::Point::y|10|10|public|[(\"type_annotation\", \"int\")]",
        // issue #107 — function-like macro
        "Function|SQUARE|app/main.c::SQUARE|6|7|public|[(\"macro\", \"true\")]",
        "Function|add|app/main.c::add#1|35|35|public|[(\"is_prototype\", \"true\")]",
        "Function|add|app/main.c::add#3|41|49|public|[]",
        "Function|dbg|app/main.c::dbg#10|54|54|public|[(\"is_prototype\", \"true\")]",
        "Function|empty|app/main.c::empty#9|51|51|public|[]",
        "Function|gated|app/main.c::gated#11|61|63|public|[]",
        "Function|helper|app/main.c::helper#2|37|39|public|[]",
        "Function|signal_handler|app/main.c::signal_handler#13|66|66|public|[(\"is_prototype\", \"true\")]",
        "Import|config.h|app/main.c::include:config.h|2|3|public|[(\"path\", \"config.h\")]",
        "Import|stdio.h|app/main.c::include:stdio.h|1|2|public|[(\"path\", \"stdio.h\")]",
        "Import|types.h|app/main.c::include:sys/types.h|3|4|public|[(\"path\", \"sys/types.h\")]",
        "Struct|Gated|app/main.c::Gated|58|60|public|[]",
        "Struct|Point|app/main.c::Point|8|16|public|[]",
        "Struct|Value|app/main.c::Value|18|21|public|[]",
    ]
}

pub(super) fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Calls", "app/main.c::add#3", "_internal"),
        ("Calls", "app/main.c::add#3", "call"),
        ("Calls", "app/main.c::add#3", "helper"),
        ("Calls", "app/main.c::add#3", "method"),
        ("Calls", "app/main.c::add#3", "printf"),
        ("Calls", "app/main.c::gated#11", "dbg"),
        ("Defines", "app/main.c::Color", "app/main.c::Color::BLUE"),
        ("Defines", "app/main.c::Color", "app/main.c::Color::GREEN"),
        ("Defines", "app/main.c::Color", "app/main.c::Color::RED"),
        ("Defines", "app/main.c", "app/main.c::Callback"),
        ("Defines", "app/main.c", "app/main.c::Color"),
        ("Defines", "app/main.c", "app/main.c::Gated"),
        ("Defines", "app/main.c", "app/main.c::Point"),
        ("Defines", "app/main.c", "app/main.c::PointT"),
        ("Defines", "app/main.c", "app/main.c::Value"),
        ("Defines", "app/main.c", "app/main.c::add#1"),
        ("Defines", "app/main.c", "app/main.c::add#3"),
        ("Defines", "app/main.c", "app/main.c::dbg#10"),
        ("Defines", "app/main.c", "app/main.c::empty#9"),
        ("Defines", "app/main.c", "app/main.c::gated#11"),
        ("Defines", "app/main.c", "app/main.c::helper#2"),
        ("Defines", "app/main.c", "app/main.c::signal_handler#13"),
        ("Defines", "app/main.c", "app/main.c::ulong_t"),
        // issue #107 — macros and inline types
        ("Defines", "app/main.c", "app/main.c::MAX"),
        ("Defines", "app/main.c", "app/main.c::SQUARE"),
        ("Defines", "app/main.c", "app/main.c::Anon"),
        ("Defines", "app/main.c", "app/main.c::Tagged"),
        ("HasField", "app/main.c::Anon", "app/main.c::Anon::ax"),
        ("HasField", "app/main.c::Anon", "app/main.c::Anon::ay"),
        ("HasField", "app/main.c::Tagged", "app/main.c::Tagged::tv"),
        ("HasField", "app/main.c::Gated", "app/main.c::Gated::flag"),
        ("HasField", "app/main.c::Point", "app/main.c::Point::buf"),
        (
            "HasField",
            "app/main.c::Point",
            "app/main.c::Point::handler",
        ),
        ("HasField", "app/main.c::Point", "app/main.c::Point::height"),
        ("HasField", "app/main.c::Point", "app/main.c::Point::name"),
        ("HasField", "app/main.c::Point", "app/main.c::Point::next"),
        ("HasField", "app/main.c::Point", "app/main.c::Point::width"),
        ("HasField", "app/main.c::Point", "app/main.c::Point::x"),
        ("HasField", "app/main.c::Point", "app/main.c::Point::y"),
        ("HasField", "app/main.c::Value", "app/main.c::Value::f"),
        ("HasField", "app/main.c::Value", "app/main.c::Value::i"),
        ("Imports", "app/main.c", "config.h"),
        ("Imports", "app/main.c", "stdio.h"),
        ("Imports", "app/main.c", "sys/types.h"),
    ]
}
