// parser::spec::kotlin_ground_truth — the Kotlin extraction ground truth: the
// corpus and the exact node/ref sets it must produce. The assertions over it
// live in the sibling `kotlin_parity_tests` module; keeping the tables here is
// what holds both files under the 500-line cap (coding-standards §4.1). Mirrors
// the C/C++/Swift split. No expected record is changed by the move.
//
// One Kotlin file exercising every concern the spec walker handles for Kotlin:
//   - `package` (skipped) and three imports (plain `import a.b.C`, aliased
//     `import a.b.C as D`, wildcard `import a.b.*`), shaped as `import:<path>`
//     QNs with a `path` property and a last-segment display name.
//   - an `interface` (`Greeter`) → `Trait` with its abstract method → `Method`/
//     `HasMethod`, and an `annotation class` (`Marker`) → `Struct`.
//   - an `enum class` (`Color`) → `Enum`, its entries (`RED`/`GREEN`/`BLUE`) →
//     `Constant` with `enum_entry=true`, reached via the `enum_class_body`.
//   - a `sealed class` (`Shape`) → `Struct` with a nested `class Circle : Shape()`
//     → `Struct` + `Extends`, and its method's call.
//   - a `data class` (`Point`) → `Struct`; its ctor params (`val x`, `val y`)
//     are `class_parameter` nodes inside `primary_constructor`, NOT
//     `property_declaration`, so they remain dropped (issue #93 is scoped to
//     `property_declaration`; constructor-property params are out of scope).
//   - an `object` (`Registry`) → `Struct` with a method + a call; its `val
//     instances` property → `Constant` + `Defines` (#93, name descended through
//     `variable_declaration`).
//   - classes (`Animal`, `Dog`); `Dog : Animal(), Greeter` → two `Extends`
//     refs; `public` modifier visibility; a member extension `fun String.wag()`
//     and an override method; member properties `Animal::species` (public) and
//     `Dog::breed` (`private` modifier) → `Constant` + `Defines` (#93).
//   - top-level functions (`topLevel`, extension `fun String.shout()`,
//     `useLambda`) → `Function`/`Defines`, keyed `name#seq`; a top-level `val
//     VERSION` → `Constant` + `Defines` from the file scope (#93); and calls
//     incl. a chained `listOf(...).map { it * 2 }` and a `this.uppercase()`
//     navigation call reduced to its tail.
pub(super) const CORPUS: &str = r#"package com.example.app

import kotlin.collections.List
import kotlin.math.max as maximum
import com.example.util.*

interface Greeter {
    fun greet(): String
}

annotation class Marker

enum class Color {
    RED,
    GREEN,
    BLUE
}

sealed class Shape {
    class Circle : Shape() {
        fun area(): Int {
            return compute()
        }
    }
}

data class Point(val x: Int, val y: Int)

object Registry {
    val instances: Int = 0
    fun register(): Int {
        return compute()
    }
}

class Animal {
    val species: String = "animal"
    fun breathe() {
        inhale()
    }
}

public class Dog : Animal(), Greeter {
    private val breed: String = "mutt"
    override fun greet(): String {
        return bark()
    }
    fun String.wag() {
        return
    }
}

fun topLevel(): Int {
    return helper()
}

fun String.shout(): String {
    return this.uppercase()
}

val VERSION: String = "1.0"

fun useLambda() {
    listOf(1, 2, 3).map { it * 2 }
}
"#;

pub(super) const PATH: &str = "com/example/app/Demo.kt";

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
