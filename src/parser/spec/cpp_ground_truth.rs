// parser::spec::cpp_ground_truth — the C++ extraction ground truth: the corpus,
// and the exact node/ref sets it must produce. The assertions over it live in the
// sibling `cpp_parity_tests` module; keeping the tables here is what holds both
// files under the 500-line cap (coding-standards §4.1).
//
// This table is NOT a captured baseline. Every change against the previous
// (parity-with-the-hand-written-walker) table was verified as an INTENDED row in
// both directions — the set difference was computed each way and each entry
// mapped to an issue clause:
//
//   +11 nodes / +11 refs are genuinely NEW symbols the old walker dropped:
//     5 enum members (RED/GREEN/BLUE/OK/FAIL, #124.1)
//     5 ctor/dtor methods (Point, ~Point, Shape, ~Shape, Circle, #124.2)
//     1 alias declaration (Distance, #124.3)
//   The 26 removed rows pair 1:1 with 26 replacements:
//     6 data members: Constant+Defines -> Field+HasField with type_annotation (#124.4)
//     1 out-of-body definition: Function+Defines -> Method+HasMethod under
//       geometry::Circle (#124.5)
//     6 names corrected from a parameter to the declared name (b->freeFunction,
//       b->gated, x->identity, other->operator+, item->add, i->get; #123)
//     13 QN/`#seq` shifts that FOLLOW from the above — the 5 new ctor/dtor
//       prototypes consume seq numbers ahead of every member after them, and each
//       CallSite QN is keyed on its now-renamed caller
//   41 nodes -> 52, 44 refs -> 55. No row changed without a named cause.
//
// The exact-set assertion over this table is the mutation oracle: it kills every
// walker mutant that perturbs any emitted node or ref.
//
// The corpus exercises every C++ concern the hybrid walker handles, plus the edge
// cases that pin specific behaviors:
//   - `namespace geometry { ... }` - a `Struct` (`is_namespace=true`) whose body
//     recurses as a NON-class scope: the inner template function `identity` stays
//     a `Function` (`geometry::identity#15`), NOT a method.
//   - `struct Point` / `union Value` - `Struct` (no `is_class`); their data
//     members (`x`, `y`, `i`, `f`) are `Field` + `HasField` with a
//     `type_annotation`, the same model the flat C walker uses (#124.4).
//   - `class Shape` / `Circle` / `Multi` / `Container` - `Struct`
//     (`is_class=true`); `Circle : public Shape` -> one `Extends` + a `bases`
//     CSV property; `Multi : public Shape, protected Point` -> TWO `Extends`
//     + `bases="Shape,Point"` (issue #216 fix — the property, not the ref, is
//     what `resolver::extends::resolve_extends` reads).
//   - Member prototypes (`virtual double area() const;`, `void draw();`) ->
//     `Method` (`is_prototype=true`, receiver = enclosing class). An inline
//     definition (`int getId() { ... }`) -> `Method` WITHOUT `is_prototype`, and
//     its body IS scanned (the `compute` call).
//   - Constructors/destructors (`Point(int,int);`, `~Point();`, `Shape();`) are
//     `declaration` nodes, NOT `field_declaration` - reached through
//     `member_decl_kinds` and emitted as prototype `Method`s; a destructor keeps
//     its declared spelling `~Point` (#124.2).
//   - Operator overload `Shape operator+(const Shape& other);` -> a `Method` named
//     `operator+` (the `operator_name` node's own text, #123); template members
//     `add(T item)` / `get(int i)` -> `add` / `get`.
//   - `enum Color { ... }` and `enum class Status { ... }` -> an `Enum` plus one
//     `Constant` (`enum_entry=true`) per member (#124.1).
//   - `typedef int Length;` -> `Constant` (`typedef=true`). `using Distance =
//     double;` is an `alias_declaration` -> `TypeAlias` carrying the aliased type
//     (#124.3).
//   - `#include <iostream>` / `#include "myheader.h"` / `#include <sys/types.h>`
//     -> `Import` (display = last path segment, edge = full path).
//   - `using namespace std;` / `using std::vector;` -> `Import` (`using=true`) -
//     a different node kind from the alias declaration above.
//   - Free function `freeFunction(int a, int b)` -> `Function` named
//     `freeFunction` (#123), `freeFunction#17`; its body's calls resolve by
//     member-access tail (`printf`, `obj.method`->`method`, `ptr->call`->`call`,
//     `geometry::identity`->`identity`, `_internal`), and `(fp)()` - a
//     parenthesized non-identifier callee - is DROPPED (negative assertion). The
//     call `seq`s are assigned in REVERSE source order (stack DFS), keying the
//     `call@line:col#seq` QNs.
//   - The out-of-body definition `double geometry::Circle::area() const { ... }`
//     at file scope -> a `Method` re-attached to `geometry::Circle`
//     (`geometry::Circle::area#16`), NOT a file-scoped `Function` (#124.5).
//   - The `#ifdef DEBUG ... #endif`-wrapped `gated` at the corpus tail is
//     load-bearing for the default-recursion arm - a preprocessor conditional is
//     not an explicit dispatch kind, so the walker only reaches its inner
//     `function_definition` by recursing the wrapper (which HAS named children).
//     Its presence kills the `named_child_count() > 0` -> `== 0` / `< 0` mutants
//     (both would drop `gated` (`gated#24`) and its `helper` call); only
//     `> 0` -> `>= 0` survives, a documented EQUIVALENT (recursing a childless
//     node emits nothing).
// source: tree-sitter-cpp 0.23.4 src/node-types.json (every node kind and field
// the walker reads for these shapes was verified against it).

pub(super) const CORPUS: &str = r#"#include <iostream>
#include "myheader.h"
#include <sys/types.h>

using namespace std;
using std::vector;

namespace geometry {

typedef int Length;
using Distance = double;

struct Point {
    int x;
    int y;
    Point(int a, int b);
    int magnitude() const;
    ~Point();
};

class Shape {
public:
    Shape();
    virtual ~Shape();
    virtual double area() const;
    Shape operator+(const Shape& other);
    int getId() { return compute(id); }
private:
    int id;
};

class Circle : public Shape {
public:
    Circle(double r);
    double area() const;
private:
    double radius;
};

class Multi : public Shape, protected Point {
    void draw();
};

union Value {
    int i;
    float f;
};

enum Color { RED, GREEN, BLUE };

enum class Status { OK, FAIL };

template <typename T>
class Container {
public:
    void add(T item);
    T get(int i);
};

template <typename T>
T identity(T x) {
    return x;
}

}

double geometry::Circle::area() const {
    return 3.14 * radius * radius;
}

int freeFunction(int a, int b) {
    int r = a + b;
    printf("%d", r);
    obj.method(a);
    ptr->call(b);
    geometry::identity(r);
    _internal();
    (fp)();
    return r;
}

void empty() {}

#ifdef DEBUG
int gated(int a, int b) {
    return helper(a);
}
#endif
"#;

pub(super) const PATH: &str = "app/main.cpp";

pub(super) fn expected_node_records() -> Vec<&'static str> {
    vec![
        "CallSite|_internal|app/main.cpp::freeFunction#17::call@77:5#18|77|77|public|[(\"callee_name\", \"_internal\"), (\"lsp_col\", \"4\")]",
        "CallSite|call|app/main.cpp::freeFunction#17::call@75:5#20|75|75|public|[(\"callee_name\", \"call\"), (\"lsp_col\", \"4\")]",
        "CallSite|compute|app/main.cpp::geometry::Shape::getId#8::call@27:26#9|27|27|public|[(\"callee_name\", \"compute\"), (\"lsp_col\", \"25\")]",
        "CallSite|helper|app/main.cpp::gated#24::call@86:12#25|86|86|public|[(\"callee_name\", \"helper\"), (\"lsp_col\", \"11\")]",
        "CallSite|identity|app/main.cpp::freeFunction#17::call@76:5#19|76|76|public|[(\"callee_name\", \"identity\"), (\"lsp_col\", \"4\")]",
        "CallSite|method|app/main.cpp::freeFunction#17::call@74:5#21|74|74|public|[(\"callee_name\", \"method\"), (\"lsp_col\", \"4\")]",
        "CallSite|printf|app/main.cpp::freeFunction#17::call@73:5#22|73|73|public|[(\"callee_name\", \"printf\"), (\"lsp_col\", \"4\")]",
        "Constant|BLUE|app/main.cpp::geometry::Color::BLUE|49|49|public|[(\"enum_entry\", \"true\")]",
        "Constant|FAIL|app/main.cpp::geometry::Status::FAIL|51|51|public|[(\"enum_entry\", \"true\")]",
        "Constant|GREEN|app/main.cpp::geometry::Color::GREEN|49|49|public|[(\"enum_entry\", \"true\")]",
        "Constant|Length|app/main.cpp::geometry::Length|10|10|public|[(\"typedef\", \"true\")]",
        "Constant|OK|app/main.cpp::geometry::Status::OK|51|51|public|[(\"enum_entry\", \"true\")]",
        "Constant|RED|app/main.cpp::geometry::Color::RED|49|49|public|[(\"enum_entry\", \"true\")]",
        "Enum|Color|app/main.cpp::geometry::Color|49|49|public|[]",
        "Enum|Status|app/main.cpp::geometry::Status|51|51|public|[]",
        "Field|f|app/main.cpp::geometry::Value::f|46|46|public|[(\"type_annotation\", \"float\")]",
        "Field|id|app/main.cpp::geometry::Shape::id|29|29|public|[(\"type_annotation\", \"int\")]",
        "Field|i|app/main.cpp::geometry::Value::i|45|45|public|[(\"type_annotation\", \"int\")]",
        "Field|radius|app/main.cpp::geometry::Circle::radius|37|37|public|[(\"type_annotation\", \"double\")]",
        "Field|x|app/main.cpp::geometry::Point::x|14|14|public|[(\"type_annotation\", \"int\")]",
        "Field|y|app/main.cpp::geometry::Point::y|15|15|public|[(\"type_annotation\", \"int\")]",
        "Function|empty|app/main.cpp::empty#23|82|82|public|[]",
        "Function|freeFunction|app/main.cpp::freeFunction#17|71|80|public|[]",
        "Function|gated|app/main.cpp::gated#24|85|87|public|[]",
        "Function|identity|app/main.cpp::geometry::identity#15|61|63|public|[]",
        "Import|iostream|app/main.cpp::include:iostream|1|2|public|[(\"path\", \"iostream\")]",
        "Import|myheader.h|app/main.cpp::include:myheader.h|2|3|public|[(\"path\", \"myheader.h\")]",
        "Import|namespace std|app/main.cpp::using:namespace std|5|5|public|[(\"using\", \"true\")]",
        "Import|std::vector|app/main.cpp::using:std::vector|6|6|public|[(\"using\", \"true\")]",
        "Import|types.h|app/main.cpp::include:sys/types.h|3|4|public|[(\"path\", \"sys/types.h\")]",
        "Method|Circle|app/main.cpp::geometry::Circle::Circle#10|34|34|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Circle\")]",
        "Method|Point|app/main.cpp::geometry::Point::Point#1|16|16|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Point\")]",
        "Method|Shape|app/main.cpp::geometry::Shape::Shape#4|23|23|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Shape\")]",
        "Method|add|app/main.cpp::geometry::Container::add#13|56|56|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Container\")]",
        "Method|area|app/main.cpp::geometry::Circle::area#11|35|35|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Circle\")]",
        "Method|area|app/main.cpp::geometry::Circle::area#16|67|69|public|[(\"receiver_type\", \"app/main.cpp::geometry::Circle\")]",
        "Method|area|app/main.cpp::geometry::Shape::area#6|25|25|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Shape\")]",
        "Method|draw|app/main.cpp::geometry::Multi::draw#12|41|41|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Multi\")]",
        "Method|getId|app/main.cpp::geometry::Shape::getId#8|27|27|public|[(\"receiver_type\", \"app/main.cpp::geometry::Shape\")]",
        "Method|get|app/main.cpp::geometry::Container::get#14|57|57|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Container\")]",
        "Method|magnitude|app/main.cpp::geometry::Point::magnitude#2|17|17|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Point\")]",
        "Method|operator+|app/main.cpp::geometry::Shape::operator+#7|26|26|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Shape\")]",
        "Method|~Point|app/main.cpp::geometry::Point::~Point#3|18|18|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Point\")]",
        "Method|~Shape|app/main.cpp::geometry::Shape::~Shape#5|24|24|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Shape\")]",
        "Struct|Circle|app/main.cpp::geometry::Circle|32|38|public|[(\"is_class\", \"true\"), (\"bases\", \"Shape\")]",
        "Struct|Container|app/main.cpp::geometry::Container|54|58|public|[(\"is_class\", \"true\")]",
        "Struct|Multi|app/main.cpp::geometry::Multi|40|42|public|[(\"is_class\", \"true\"), (\"bases\", \"Shape,Point\")]",
        "Struct|Point|app/main.cpp::geometry::Point|13|19|public|[]",
        "Struct|Shape|app/main.cpp::geometry::Shape|21|30|public|[(\"is_class\", \"true\")]",
        "Struct|Value|app/main.cpp::geometry::Value|44|47|public|[]",
        "Struct|geometry|app/main.cpp::geometry|8|65|public|[(\"is_namespace\", \"true\")]",
        "TypeAlias|Distance|app/main.cpp::geometry::Distance|11|11|public|[(\"type_annotation\", \"double\")]",
    ]
}

pub(super) fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut v = Vec::new();
    v.extend(expected_refs_part0());
    v.extend(expected_refs_part1());
    v.extend(expected_refs_part2());
    v.extend(expected_refs_part3());
    v.extend(expected_refs_part4());
    v.extend(expected_refs_part5());
    v.extend(expected_refs_part6());
    v
}

fn expected_refs_part0() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Calls", "app/main.cpp::freeFunction#17", "_internal"),
        ("Calls", "app/main.cpp::freeFunction#17", "call"),
        ("Calls", "app/main.cpp::freeFunction#17", "identity"),
        ("Calls", "app/main.cpp::freeFunction#17", "method"),
        ("Calls", "app/main.cpp::freeFunction#17", "printf"),
        ("Calls", "app/main.cpp::gated#24", "helper"),
        ("Calls", "app/main.cpp::geometry::Shape::getId#8", "compute"),
        ("Defines", "app/main.cpp", "app/main.cpp::empty#23"),
        ("Defines", "app/main.cpp", "app/main.cpp::freeFunction#17"),
        ("Defines", "app/main.cpp", "app/main.cpp::gated#24"),
        ("Defines", "app/main.cpp", "app/main.cpp::geometry"),
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::Circle",
        ),
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::Color",
        ),
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::Container",
        ),
    ]
}

fn expected_refs_part1() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::Distance",
        ),
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::Length",
        ),
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::Multi",
        ),
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::Point",
        ),
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::Shape",
        ),
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::Status",
        ),
    ]
}

fn expected_refs_part2() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::Value",
        ),
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::identity#15",
        ),
        (
            "Defines",
            "app/main.cpp::geometry::Color",
            "app/main.cpp::geometry::Color::BLUE",
        ),
        (
            "Defines",
            "app/main.cpp::geometry::Color",
            "app/main.cpp::geometry::Color::GREEN",
        ),
        (
            "Defines",
            "app/main.cpp::geometry::Color",
            "app/main.cpp::geometry::Color::RED",
        ),
        (
            "Defines",
            "app/main.cpp::geometry::Status",
            "app/main.cpp::geometry::Status::FAIL",
        ),
    ]
}

fn expected_refs_part3() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Defines",
            "app/main.cpp::geometry::Status",
            "app/main.cpp::geometry::Status::OK",
        ),
        ("Extends", "app/main.cpp::geometry::Circle", "Shape"),
        ("Extends", "app/main.cpp::geometry::Multi", "Point"),
        ("Extends", "app/main.cpp::geometry::Multi", "Shape"),
        (
            "HasField",
            "app/main.cpp::geometry::Circle",
            "app/main.cpp::geometry::Circle::radius",
        ),
        (
            "HasField",
            "app/main.cpp::geometry::Point",
            "app/main.cpp::geometry::Point::x",
        ),
        (
            "HasField",
            "app/main.cpp::geometry::Point",
            "app/main.cpp::geometry::Point::y",
        ),
        (
            "HasField",
            "app/main.cpp::geometry::Shape",
            "app/main.cpp::geometry::Shape::id",
        ),
    ]
}

fn expected_refs_part4() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "HasField",
            "app/main.cpp::geometry::Value",
            "app/main.cpp::geometry::Value::f",
        ),
        (
            "HasField",
            "app/main.cpp::geometry::Value",
            "app/main.cpp::geometry::Value::i",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Circle",
            "app/main.cpp::geometry::Circle::Circle#10",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Circle",
            "app/main.cpp::geometry::Circle::area#11",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Circle",
            "app/main.cpp::geometry::Circle::area#16",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Container",
            "app/main.cpp::geometry::Container::add#13",
        ),
    ]
}

fn expected_refs_part5() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "HasMethod",
            "app/main.cpp::geometry::Container",
            "app/main.cpp::geometry::Container::get#14",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Multi",
            "app/main.cpp::geometry::Multi::draw#12",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Point",
            "app/main.cpp::geometry::Point::Point#1",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Point",
            "app/main.cpp::geometry::Point::magnitude#2",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Point",
            "app/main.cpp::geometry::Point::~Point#3",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Shape",
            "app/main.cpp::geometry::Shape::Shape#4",
        ),
    ]
}

fn expected_refs_part6() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "HasMethod",
            "app/main.cpp::geometry::Shape",
            "app/main.cpp::geometry::Shape::area#6",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Shape",
            "app/main.cpp::geometry::Shape::getId#8",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Shape",
            "app/main.cpp::geometry::Shape::operator+#7",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Shape",
            "app/main.cpp::geometry::Shape::~Shape#5",
        ),
        ("Imports", "app/main.cpp", "iostream"),
        ("Imports", "app/main.cpp", "myheader.h"),
        ("Imports", "app/main.cpp", "namespace std"),
        ("Imports", "app/main.cpp", "std::vector"),
        ("Imports", "app/main.cpp", "sys/types.h"),
    ]
}
