// parser::spec::swift_ground_truth — the Swift extraction ground truth: the
// corpus (fixture) and the exact node/ref sets it must produce. The assertions
// over it live in the sibling `swift_parity_tests` module; keeping the tables
// here is what holds both files under the 500-line cap (coding-standards §4.1).
// Mirrors the C/C++ split (`c_ground_truth`, `cpp_ground_truth`). No expected
// record is changed by the move — it is a pure relocation.
//
// The comprehensive Swift corpus (also captured by tests/fixtures/. It exercises
// every concern the spec walker handles for Swift:
//   - three `import`s → `Import`/`Imports` with a `path` property.
//   - top-level `let`/`var` (`maxRetries`/`counter`) and a `fileprivate let`
//     (`sharedFlag`) → `Constant`s with modifier visibility.
//   - top-level `public func`/`private func` → `Function`/`Defines` keyed
//     `name#seq`, with their calls.
//   - a `struct` (`Point`) with stored `let`/`var` (→ `Constant`s), an `init`
//     (→ `Method` marked `member_kind=init`), a method, a computed `var`
//     (→ `Constant`, its getter body call-scanned → `Calls -> sqrt`; issue #100)
//     and a `subscript` (→ `Method` marked `member_kind=subscript`, its
//     `computed_property` body scanned for calls).
//   - a `public class` (`Animal`) with `init`/`deinit`/`open func` (open
//     visibility), and an `extension Animal: Equatable` → a second `Struct` node
//     with the same QN marked `is_extension=true` + `bases="Equatable"` (issue
//     #216 fix) whose method scopes under `::Animal`, plus an
//     `Extends Animal -> Equatable` conformance edge (#97).
//   - an `actor` (`Counter`) → `Struct`.
//   - an `enum` (`Color`) → `Enum`, with `case red` / `case green, blue`
//     (one `enum_entry`, two variants) / `case custom(Int)` → `Variant`s edged
//     `HasVariant` with `public` visibility (the canonical model; issue #99),
//     plus a method.
//   - a `protocol` (`Serializable`) → `Trait` whose `serialize` requirement
//     (a `protocol_function_declaration`) is extracted as a `Method` +
//     `HasMethod`, bodiless so no calls are scanned (issue #98).
//   - a `typealias` → `Constant` marked `typealias=true`.
//   - calls incl. trailing closures (`items.map { transform($0) }`,
//     `fetch { … handle(…) }`), a qualified `Utils.parse()` reduced to `parse`,
//     and `self.reset()` reduced to `reset`.
pub(super) const CORPUS: &str = include_str!("../../../tests/fixtures/swift_parity_corpus.swift");
pub(super) const PATH: &str = "Sources/App/Demo.swift";

pub(super) fn expected_node_records() -> Vec<&'static str> {
    vec![
        "CallSite|at|Sources/App/Demo.swift::Point::subscript#10::call@36:16#11|36|36|internal|[(\"callee_name\", \"at\")]",
        "CallSite|bump|Sources/App/Demo.swift::Counter::increment#17::call@60:9#18|60|60|internal|[(\"callee_name\", \"bump\")]",
        "CallSite|cleanup|Sources/App/Demo.swift::Animal::deinit#13::call@48:9#14|48|48|internal|[(\"callee_name\", \"cleanup\")]",
        "CallSite|compute|Sources/App/Demo.swift::Point::distance#7::call@28:16#8|28|28|internal|[(\"callee_name\", \"compute\")]",
        "CallSite|compute|Sources/App/Demo.swift::helper#3::call@13:5#4|13|13|internal|[(\"callee_name\", \"compute\")]",
        "CallSite|convert|Sources/App/Demo.swift::Color::hex#19::call@70:16#20|70|70|internal|[(\"callee_name\", \"convert\")]",
        "CallSite|describe|Sources/App/Demo.swift::Point::init#5::call@24:22#6|24|24|internal|[(\"callee_name\", \"describe\")]",
        "CallSite|fetch|Sources/App/Demo.swift::useClosures#24::call@90:5#27|90|92|internal|[(\"callee_name\", \"fetch\")]",
        "CallSite|format|Sources/App/Demo.swift::greet#1::call@9:12#2|9|9|internal|[(\"callee_name\", \"format\")]",
        "CallSite|handle|Sources/App/Demo.swift::useClosures#24::call@91:9#28|91|91|internal|[(\"callee_name\", \"handle\")]",
        "CallSite|map|Sources/App/Demo.swift::useClosures#24::call@89:5#29|89|89|internal|[(\"callee_name\", \"map\")]",
        "CallSite|parse|Sources/App/Demo.swift::useClosures#24::call@93:5#26|93|93|internal|[(\"callee_name\", \"parse\")]",
        "CallSite|render|Sources/App/Demo.swift::Animal::describe#22::call@80:16#23|80|80|internal|[(\"callee_name\", \"render\")]",
        "CallSite|reset|Sources/App/Demo.swift::useClosures#24::call@94:5#25|94|94|internal|[(\"callee_name\", \"reset\")]",
        "CallSite|sound|Sources/App/Demo.swift::Animal::speak#15::call@52:16#16|52|52|internal|[(\"callee_name\", \"sound\")]",
        "CallSite|sqrt|Sources/App/Demo.swift::Point::magnitude::call@32:16#9|32|32|internal|[(\"callee_name\", \"sqrt\")]",
        "CallSite|transform|Sources/App/Demo.swift::useClosures#24::call@89:17#30|89|89|internal|[(\"callee_name\", \"transform\")]",
        "Constant|Handler|Sources/App/Demo.swift::Handler|84|84|internal|[(\"typealias\", \"true\")]",
        "Constant|counter|Sources/App/Demo.swift::counter|6|6|internal|[]",
        "Constant|label|Sources/App/Demo.swift::Point::label|19|19|internal|[]",
        "Constant|magnitude|Sources/App/Demo.swift::Point::magnitude|31|33|internal|[]",
        "Constant|maxRetries|Sources/App/Demo.swift::maxRetries|5|5|internal|[]",
        "Constant|name|Sources/App/Demo.swift::Animal::name|41|41|internal|[]",
        "Constant|sharedFlag|Sources/App/Demo.swift::sharedFlag|86|86|fileprivate|[]",
        "Constant|value|Sources/App/Demo.swift::Counter::value|57|57|internal|[]",
        "Constant|x|Sources/App/Demo.swift::Point::x|17|17|internal|[]",
        "Constant|y|Sources/App/Demo.swift::Point::y|18|18|internal|[]",
        "Enum|Color|Sources/App/Demo.swift::Color|64|72|internal|[]",
        "Function|greet|Sources/App/Demo.swift::greet#1|8|10|public|[]",
        "Function|helper|Sources/App/Demo.swift::helper#3|12|14|private|[]",
        "Function|useClosures|Sources/App/Demo.swift::useClosures#24|88|95|internal|[]",
        "Import|Foundation|Sources/App/Demo.swift::import:Foundation|1|1|internal|[(\"path\", \"Foundation\")]",
        "Import|SwiftUI|Sources/App/Demo.swift::import:SwiftUI|3|3|internal|[(\"path\", \"SwiftUI\")]",
        "Import|UIKit|Sources/App/Demo.swift::import:UIKit|2|2|internal|[(\"path\", \"UIKit\")]",
        "Method|deinit|Sources/App/Demo.swift::Animal::deinit#13|47|49|internal|[(\"member_kind\", \"deinit\"), (\"receiver_type\", \"Sources/App/Demo.swift::Animal\")]",
        "Method|describe|Sources/App/Demo.swift::Animal::describe#22|79|81|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Animal\")]",
        "Method|distance|Sources/App/Demo.swift::Point::distance#7|27|29|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Point\")]",
        "Method|hex|Sources/App/Demo.swift::Color::hex#19|69|71|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Color\")]",
        "Method|increment|Sources/App/Demo.swift::Counter::increment#17|59|61|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Counter\")]",
        "Method|init|Sources/App/Demo.swift::Animal::init#12|43|45|internal|[(\"member_kind\", \"init\"), (\"receiver_type\", \"Sources/App/Demo.swift::Animal\")]",
        "Method|init|Sources/App/Demo.swift::Point::init#5|21|25|internal|[(\"member_kind\", \"init\"), (\"receiver_type\", \"Sources/App/Demo.swift::Point\")]",
        "Method|serialize|Sources/App/Demo.swift::Serializable::serialize#21|75|75|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Serializable\")]",
        "Method|speak|Sources/App/Demo.swift::Animal::speak#15|51|53|open|[(\"receiver_type\", \"Sources/App/Demo.swift::Animal\")]",
        "Method|subscript|Sources/App/Demo.swift::Point::subscript#10|35|37|internal|[(\"member_kind\", \"subscript\"), (\"receiver_type\", \"Sources/App/Demo.swift::Point\")]",
        "Struct|Animal|Sources/App/Demo.swift::Animal|40|54|public|[]",
        "Struct|Animal|Sources/App/Demo.swift::Animal|78|82|internal|[(\"bases\", \"Equatable\"), (\"is_extension\", \"true\")]",
        "Struct|Counter|Sources/App/Demo.swift::Counter|56|62|internal|[]",
        "Struct|Point|Sources/App/Demo.swift::Point|16|38|internal|[]",
        "Trait|Serializable|Sources/App/Demo.swift::Serializable|74|76|internal|[]",
        "Variant|blue|Sources/App/Demo.swift::Color::blue|66|66|public|[]",
        "Variant|custom|Sources/App/Demo.swift::Color::custom|67|67|public|[]",
        "Variant|green|Sources/App/Demo.swift::Color::green|66|66|public|[]",
        "Variant|red|Sources/App/Demo.swift::Color::red|65|65|public|[]",
        ]
}

pub(super) fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Calls",
            "Sources/App/Demo.swift::Animal::deinit#13",
            "cleanup",
        ),
        (
            "Calls",
            "Sources/App/Demo.swift::Animal::describe#22",
            "render",
        ),
        ("Calls", "Sources/App/Demo.swift::Animal::speak#15", "sound"),
        ("Calls", "Sources/App/Demo.swift::Color::hex#19", "convert"),
        (
            "Calls",
            "Sources/App/Demo.swift::Counter::increment#17",
            "bump",
        ),
        (
            "Calls",
            "Sources/App/Demo.swift::Point::distance#7",
            "compute",
        ),
        ("Calls", "Sources/App/Demo.swift::Point::init#5", "describe"),
        ("Calls", "Sources/App/Demo.swift::Point::magnitude", "sqrt"),
        ("Calls", "Sources/App/Demo.swift::Point::subscript#10", "at"),
        ("Calls", "Sources/App/Demo.swift::greet#1", "format"),
        ("Calls", "Sources/App/Demo.swift::helper#3", "compute"),
        ("Calls", "Sources/App/Demo.swift::useClosures#24", "fetch"),
        ("Calls", "Sources/App/Demo.swift::useClosures#24", "handle"),
        ("Calls", "Sources/App/Demo.swift::useClosures#24", "map"),
        ("Calls", "Sources/App/Demo.swift::useClosures#24", "parse"),
        ("Calls", "Sources/App/Demo.swift::useClosures#24", "reset"),
        (
            "Calls",
            "Sources/App/Demo.swift::useClosures#24",
            "transform",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::Animal",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::Color",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::Counter",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::Handler",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::Point",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::Serializable",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::counter",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::greet#1",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::helper#3",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::maxRetries",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::sharedFlag",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::useClosures#24",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::name",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Counter",
            "Sources/App/Demo.swift::Counter::value",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::label",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::magnitude",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::x",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::y",
        ),
        ("Extends", "Sources/App/Demo.swift::Animal", "Equatable"),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::deinit#13",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::describe#22",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::init#12",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::speak#15",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::hex#19",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Counter",
            "Sources/App/Demo.swift::Counter::increment#17",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::distance#7",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::init#5",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::subscript#10",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Serializable",
            "Sources/App/Demo.swift::Serializable::serialize#21",
        ),
        (
            "HasVariant",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::blue",
        ),
        (
            "HasVariant",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::custom",
        ),
        (
            "HasVariant",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::green",
        ),
        (
            "HasVariant",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::red",
        ),
        ("Imports", "Sources/App/Demo.swift", "Foundation"),
        ("Imports", "Sources/App/Demo.swift", "SwiftUI"),
        ("Imports", "Sources/App/Demo.swift", "UIKit"),
    ]
}
