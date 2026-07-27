// parser::spec::kotlin_parity_expected — the pinned pre-migration ground truth
// for the Kotlin parity suite (ADR-0055 §5 step 3).
//
// These records ARE the hand-written walker's output, captured before it was
// deleted: one full 7-tuple per node, one full triple per ref. Split out of
// `kotlin_parity_tests.rs` (which kept the corpus, the record helpers and the
// assertions) purely to hold both files inside the §4.1 500-line cap — the
// pinned data is a table, the assertions are logic, and they change for
// different reasons. A pure move: every string is byte-for-byte the one the
// capture produced, which the unchanged parity assertions prove.

pub(super) fn expected_node_records() -> Vec<&'static str> {
    vec![
        "CallSite|bark|com/example/app/Demo.kt::Dog::greet#8::call@46:16#9|46|46|public|[(\"callee_name\", \"bark\")]",
        "CallSite|compute|com/example/app/Demo.kt::Registry::register#4::call@32:16#5|32|32|public|[(\"callee_name\", \"compute\")]",
        "CallSite|compute|com/example/app/Demo.kt::Shape::Circle::area#2::call@22:20#3|22|22|public|[(\"callee_name\", \"compute\")]",
        "CallSite|helper|com/example/app/Demo.kt::topLevel#11::call@54:12#12|54|54|public|[(\"callee_name\", \"helper\")]",
        "CallSite|inhale|com/example/app/Demo.kt::Animal::breathe#6::call@39:9#7|39|39|public|[(\"callee_name\", \"inhale\")]",
        "CallSite|listOf|com/example/app/Demo.kt::useLambda#15::call@64:5#17|64|64|public|[(\"callee_name\", \"listOf\")]",
        "CallSite|map|com/example/app/Demo.kt::useLambda#15::call@64:5#16|64|64|public|[(\"callee_name\", \"map\")]",
        "CallSite|uppercase|com/example/app/Demo.kt::shout#13::call@58:12#14|58|58|public|[(\"callee_name\", \"uppercase\")]",
        "Constant|BLUE|com/example/app/Demo.kt::Color::BLUE|16|16|public|[(\"enum_entry\", \"true\")]",
        "Constant|GREEN|com/example/app/Demo.kt::Color::GREEN|15|15|public|[(\"enum_entry\", \"true\")]",
        "Constant|RED|com/example/app/Demo.kt::Color::RED|14|14|public|[(\"enum_entry\", \"true\")]",
        // #93: `property_declaration` names, previously dropped (name nested
        // under `variable_declaration`). Now emitted as `Constant`s with
        // modifier-derived visibility and no marker (Java field-parity).
        "Constant|VERSION|com/example/app/Demo.kt::VERSION|61|61|public|[]",
        "Constant|breed|com/example/app/Demo.kt::Dog::breed|44|44|private|[]",
        "Constant|instances|com/example/app/Demo.kt::Registry::instances|30|30|public|[]",
        "Constant|species|com/example/app/Demo.kt::Animal::species|37|37|public|[]",
        "Enum|Color|com/example/app/Demo.kt::Color|13|17|public|[]",
        "Function|shout|com/example/app/Demo.kt::shout#13|57|59|public|[]",
        "Function|topLevel|com/example/app/Demo.kt::topLevel#11|53|55|public|[]",
        "Function|useLambda|com/example/app/Demo.kt::useLambda#15|63|65|public|[]",
        "Import|List|com/example/app/Demo.kt::import:kotlin.collections.List|3|3|public|[(\"path\", \"kotlin.collections.List\")]",
        "Import|max as maximum|com/example/app/Demo.kt::import:kotlin.math.max as maximum|4|4|public|[(\"path\", \"kotlin.math.max as maximum\")]",
        "Import||com/example/app/Demo.kt::import:com.example.util.*|5|5|public|[(\"path\", \"com.example.util.*\")]",
        "Method|area|com/example/app/Demo.kt::Shape::Circle::area#2|21|23|public|[(\"receiver_type\", \"com/example/app/Demo.kt::Shape::Circle\")]",
        "Method|breathe|com/example/app/Demo.kt::Animal::breathe#6|38|40|public|[(\"receiver_type\", \"com/example/app/Demo.kt::Animal\")]",
        "Method|greet|com/example/app/Demo.kt::Dog::greet#8|45|47|public|[(\"receiver_type\", \"com/example/app/Demo.kt::Dog\")]",
        "Method|greet|com/example/app/Demo.kt::Greeter::greet#1|8|8|public|[(\"receiver_type\", \"com/example/app/Demo.kt::Greeter\")]",
        "Method|register|com/example/app/Demo.kt::Registry::register#4|31|33|public|[(\"receiver_type\", \"com/example/app/Demo.kt::Registry\")]",
        "Method|wag|com/example/app/Demo.kt::Dog::wag#10|48|50|public|[(\"receiver_type\", \"com/example/app/Demo.kt::Dog\")]",
        "Struct|Animal|com/example/app/Demo.kt::Animal|36|41|public|[]",
        "Struct|Circle|com/example/app/Demo.kt::Shape::Circle|20|24|public|[]",
        "Struct|Dog|com/example/app/Demo.kt::Dog|43|51|public|[]",
        "Struct|Marker|com/example/app/Demo.kt::Marker|11|11|public|[]",
        "Struct|Point|com/example/app/Demo.kt::Point|27|27|public|[]",
        "Struct|Registry|com/example/app/Demo.kt::Registry|29|34|public|[]",
        "Struct|Shape|com/example/app/Demo.kt::Shape|19|25|public|[]",
        "Trait|Greeter|com/example/app/Demo.kt::Greeter|7|9|public|[]",
    ]
}

pub(super) fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Calls",
            "com/example/app/Demo.kt::Animal::breathe#6",
            "inhale",
        ),
        ("Calls", "com/example/app/Demo.kt::Dog::greet#8", "bark"),
        (
            "Calls",
            "com/example/app/Demo.kt::Registry::register#4",
            "compute",
        ),
        (
            "Calls",
            "com/example/app/Demo.kt::Shape::Circle::area#2",
            "compute",
        ),
        ("Calls", "com/example/app/Demo.kt::shout#13", "uppercase"),
        ("Calls", "com/example/app/Demo.kt::topLevel#11", "helper"),
        ("Calls", "com/example/app/Demo.kt::useLambda#15", "listOf"),
        ("Calls", "com/example/app/Demo.kt::useLambda#15", "map"),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Animal",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Color",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Dog",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Greeter",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Marker",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Point",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Registry",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Shape",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::shout#13",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::topLevel#11",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::useLambda#15",
        ),
        // #93: top-level `val VERSION` defined by the file.
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::VERSION",
        ),
        // #93: class-member properties defined by their enclosing class/object.
        (
            "Defines",
            "com/example/app/Demo.kt::Animal",
            "com/example/app/Demo.kt::Animal::species",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt::Color",
            "com/example/app/Demo.kt::Color::BLUE",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt::Color",
            "com/example/app/Demo.kt::Color::GREEN",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt::Color",
            "com/example/app/Demo.kt::Color::RED",
        ),
        // #93: private property on `Dog`, public property on `Registry`.
        (
            "Defines",
            "com/example/app/Demo.kt::Dog",
            "com/example/app/Demo.kt::Dog::breed",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt::Registry",
            "com/example/app/Demo.kt::Registry::instances",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt::Shape",
            "com/example/app/Demo.kt::Shape::Circle",
        ),
        ("Extends", "com/example/app/Demo.kt::Dog", "Animal"),
        ("Extends", "com/example/app/Demo.kt::Dog", "Greeter"),
        ("Extends", "com/example/app/Demo.kt::Shape::Circle", "Shape"),
        (
            "HasMethod",
            "com/example/app/Demo.kt::Animal",
            "com/example/app/Demo.kt::Animal::breathe#6",
        ),
        (
            "HasMethod",
            "com/example/app/Demo.kt::Dog",
            "com/example/app/Demo.kt::Dog::greet#8",
        ),
        (
            "HasMethod",
            "com/example/app/Demo.kt::Dog",
            "com/example/app/Demo.kt::Dog::wag#10",
        ),
        (
            "HasMethod",
            "com/example/app/Demo.kt::Greeter",
            "com/example/app/Demo.kt::Greeter::greet#1",
        ),
        (
            "HasMethod",
            "com/example/app/Demo.kt::Registry",
            "com/example/app/Demo.kt::Registry::register#4",
        ),
        (
            "HasMethod",
            "com/example/app/Demo.kt::Shape::Circle",
            "com/example/app/Demo.kt::Shape::Circle::area#2",
        ),
        ("Imports", "com/example/app/Demo.kt", "com.example.util.*"),
        (
            "Imports",
            "com/example/app/Demo.kt",
            "kotlin.collections.List",
        ),
        (
            "Imports",
            "com/example/app/Demo.kt",
            "kotlin.math.max as maximum",
        ),
    ]
}
