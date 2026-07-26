// parser::spec::cpp_parity_tests — pins the C++ migration to the dedicated
// `cpp` walker at EXACT parity with the hand-written walker it replaced
// (ADR-0055 phase 7, §5 step 3).
//
// The expected records below ARE the pre-migration `parse_cpp_file` output
// (full 7-tuple per node + full ref triples), captured mechanically from the
// hand-written walker on this corpus before it was deleted (41 nodes, 44 refs,
// 0 parse errors). Per-EdgeKind F1(new-vs-groundtruth) = 1.000 == F1(old-vs-
// groundtruth). The test parses through the crate's public `parse_file`, so it
// also covers the C++ dispatch arm.
//
// The exact-set assertion is the mutation oracle: it kills every walker mutant
// that perturbs any emitted node or ref. The `#ifdef DEBUG … #endif`-wrapped
// `gated` at the corpus tail is load-bearing for the default-recursion arm — a
// preprocessor conditional is not an explicit dispatch kind, so the walker only
// reaches its inner `function_definition` by recursing the wrapper (which HAS
// named children). Its presence kills the `named_child_count() > 0` → `== 0` /
// `< 0` mutants (both would stop recursing the wrapper, dropping `gated`
// (`b#19`) and its `helper` call); only `> 0` → `>= 0` survives, a documented
// EQUIVALENT (recursing a childless node emits nothing).
//
// The corpus exercises every C++ concern the hybrid walker handles, plus the
// edge cases that pin specific behaviors (each preserved for parity, some
// documenting a pre-existing defect filed separately: last-parameter naming
// #123, no-enum-members / no-constructor / data-member-as-Constant #124):
//   - `namespace geometry { … }` — a `Struct` (`is_namespace=true`) whose body
//     recurses as a NON-class scope: the inner template function `identity`
//     stays a `Function` (`geometry::x#10`), NOT a method.
//   - `struct Point` / `union Value` — `Struct` (no `is_class`); their data
//     members (`x`, `y`, `i`, `f`) are `Constant`s + `Defines`, NOT
//     `Field`/`HasField` (the C++ hand-written divergence from the flat C model).
//   - `class Shape` / `Circle` / `Multi` / `Container` — `Struct`
//     (`is_class=true`); `Circle : public Shape` → one `Extends`; `Multi :
//     public Shape, protected Point` → TWO `Extends`.
//   - Member prototypes (`virtual double area() const;`, `void draw();`) →
//     `Method` (`is_prototype=true`, receiver = enclosing class). An inline
//     definition (`int getId() { … }`) → `Method` WITHOUT `is_prototype`, and
//     its body IS scanned (the `compute` call).
//   - Constructors/destructors (`Point(int,int);`, `~Point();`, `Shape();`) are
//     `declaration` nodes, NOT `field_declaration` — the hand-written walker
//     emitted NOTHING for them; parity preserves that (#124).
//   - Operator overload `Shape operator+(const Shape& other);` → a `Method`
//     named `other` — the last-parameter naming defect the LIFO-DFS
//     `find_identifier` produces (#123); template members `add(T item)` /
//     `get(int i)` → `item` / `i` for the same reason.
//   - `enum Color { … }` and `enum class Status { … }` → a bare `Enum` with NO
//     members emitted (#124).
//   - `typedef int Length;` → `Constant` (`typedef=true`). `using Distance =
//     double;` is an `alias_declaration`, NOT `using_declaration` — the
//     hand-written walker never matched it, so it emits NOTHING (#124).
//   - `#include <iostream>` / `#include "myheader.h"` / `#include <sys/types.h>`
//     → `Import` (display = last path segment, edge = full path).
//   - `using namespace std;` / `using std::vector;` → `Import` (`using=true`).
//   - Free function `freeFunction(int a, int b)` → `Function` named `b`
//     (#123), `b#12`; its body's calls resolve by member-access tail
//     (`printf`, `obj.method`→`method`, `ptr->call`→`call`,
//     `geometry::identity`→`identity`, `_internal`), and `(fp)()` — a
//     parenthesized non-identifier callee — is DROPPED (negative assertion). The
//     call `seq`s are assigned in REVERSE source order (stack DFS), keying the
//     `call@line:col#seq` QNs.
//   - The out-of-body definition `double geometry::Circle::area() const { … }`
//     at file scope → a `Function` named `area` (`area#11`), NOT a method
//     re-attached to `Circle` (the hand-written walker scoped it to the file).

use std::collections::BTreeSet;

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

const CORPUS: &str = r#"#include <iostream>
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

const PATH: &str = "app/main.cpp";

fn node_record(n: &ExtractedNode) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{:?}",
        n.label, n.name, n.qualified_name, n.start_line, n.end_line, n.visibility, n.properties
    )
}

fn ref_triple(e: &ExtractedRef) -> (String, String, String) {
    (
        e.kind.clone(),
        e.from_qualified_name.clone(),
        e.to_qualified_name.clone(),
    )
}

fn expected_node_records() -> Vec<&'static str> {
    vec![
        "CallSite|_internal|app/main.cpp::b#12::call@77:5#13|77|77|public|[(\"callee_name\", \"_internal\")]",
        "CallSite|call|app/main.cpp::b#12::call@75:5#15|75|75|public|[(\"callee_name\", \"call\")]",
        "CallSite|compute|app/main.cpp::geometry::Shape::getId#4::call@27:26#5|27|27|public|[(\"callee_name\", \"compute\")]",
        "CallSite|helper|app/main.cpp::b#19::call@86:12#20|86|86|public|[(\"callee_name\", \"helper\")]",
        "CallSite|identity|app/main.cpp::b#12::call@76:5#14|76|76|public|[(\"callee_name\", \"identity\")]",
        "CallSite|method|app/main.cpp::b#12::call@74:5#16|74|74|public|[(\"callee_name\", \"method\")]",
        "CallSite|printf|app/main.cpp::b#12::call@73:5#17|73|73|public|[(\"callee_name\", \"printf\")]",
        "Constant|Length|app/main.cpp::geometry::Length|10|10|public|[(\"typedef\", \"true\")]",
        "Constant|f|app/main.cpp::geometry::Value::f|46|46|public|[]",
        "Constant|id|app/main.cpp::geometry::Shape::id|29|29|public|[]",
        "Constant|i|app/main.cpp::geometry::Value::i|45|45|public|[]",
        "Constant|radius|app/main.cpp::geometry::Circle::radius|37|37|public|[]",
        "Constant|x|app/main.cpp::geometry::Point::x|14|14|public|[]",
        "Constant|y|app/main.cpp::geometry::Point::y|15|15|public|[]",
        "Enum|Color|app/main.cpp::geometry::Color|49|49|public|[]",
        "Enum|Status|app/main.cpp::geometry::Status|51|51|public|[]",
        "Function|area|app/main.cpp::area#11|67|69|public|[]",
        "Function|b|app/main.cpp::b#12|71|80|public|[]",
        "Function|b|app/main.cpp::b#19|85|87|public|[]",
        "Function|empty|app/main.cpp::empty#18|82|82|public|[]",
        "Function|x|app/main.cpp::geometry::x#10|61|63|public|[]",
        "Import|iostream|app/main.cpp::include:iostream|1|2|public|[(\"path\", \"iostream\")]",
        "Import|myheader.h|app/main.cpp::include:myheader.h|2|3|public|[(\"path\", \"myheader.h\")]",
        "Import|namespace std|app/main.cpp::using:namespace std|5|5|public|[(\"using\", \"true\")]",
        "Import|std::vector|app/main.cpp::using:std::vector|6|6|public|[(\"using\", \"true\")]",
        "Import|types.h|app/main.cpp::include:sys/types.h|3|4|public|[(\"path\", \"sys/types.h\")]",
        "Method|area|app/main.cpp::geometry::Circle::area#6|35|35|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Circle\")]",
        "Method|area|app/main.cpp::geometry::Shape::area#2|25|25|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Shape\")]",
        "Method|draw|app/main.cpp::geometry::Multi::draw#7|41|41|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Multi\")]",
        "Method|getId|app/main.cpp::geometry::Shape::getId#4|27|27|public|[(\"receiver_type\", \"app/main.cpp::geometry::Shape\")]",
        "Method|item|app/main.cpp::geometry::Container::item#8|56|56|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Container\")]",
        "Method|i|app/main.cpp::geometry::Container::i#9|57|57|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Container\")]",
        "Method|magnitude|app/main.cpp::geometry::Point::magnitude#1|17|17|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Point\")]",
        "Method|other|app/main.cpp::geometry::Shape::other#3|26|26|public|[(\"is_prototype\", \"true\"), (\"receiver_type\", \"app/main.cpp::geometry::Shape\")]",
        "Struct|Circle|app/main.cpp::geometry::Circle|32|38|public|[(\"is_class\", \"true\")]",
        "Struct|Container|app/main.cpp::geometry::Container|54|58|public|[(\"is_class\", \"true\")]",
        "Struct|Multi|app/main.cpp::geometry::Multi|40|42|public|[(\"is_class\", \"true\")]",
        "Struct|Point|app/main.cpp::geometry::Point|13|19|public|[]",
        "Struct|Shape|app/main.cpp::geometry::Shape|21|30|public|[(\"is_class\", \"true\")]",
        "Struct|Value|app/main.cpp::geometry::Value|44|47|public|[]",
        "Struct|geometry|app/main.cpp::geometry|8|65|public|[(\"is_namespace\", \"true\")]",
    ]
}

fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Calls", "app/main.cpp::b#12", "_internal"),
        ("Calls", "app/main.cpp::b#12", "call"),
        ("Calls", "app/main.cpp::b#12", "identity"),
        ("Calls", "app/main.cpp::b#12", "method"),
        ("Calls", "app/main.cpp::b#12", "printf"),
        ("Calls", "app/main.cpp::b#19", "helper"),
        ("Calls", "app/main.cpp::geometry::Shape::getId#4", "compute"),
        ("Defines", "app/main.cpp", "app/main.cpp::area#11"),
        ("Defines", "app/main.cpp", "app/main.cpp::b#12"),
        ("Defines", "app/main.cpp", "app/main.cpp::b#19"),
        ("Defines", "app/main.cpp", "app/main.cpp::empty#18"),
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
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::Value",
        ),
        (
            "Defines",
            "app/main.cpp::geometry",
            "app/main.cpp::geometry::x#10",
        ),
        (
            "Defines",
            "app/main.cpp::geometry::Circle",
            "app/main.cpp::geometry::Circle::radius",
        ),
        (
            "Defines",
            "app/main.cpp::geometry::Point",
            "app/main.cpp::geometry::Point::x",
        ),
        (
            "Defines",
            "app/main.cpp::geometry::Point",
            "app/main.cpp::geometry::Point::y",
        ),
        (
            "Defines",
            "app/main.cpp::geometry::Shape",
            "app/main.cpp::geometry::Shape::id",
        ),
        (
            "Defines",
            "app/main.cpp::geometry::Value",
            "app/main.cpp::geometry::Value::f",
        ),
        (
            "Defines",
            "app/main.cpp::geometry::Value",
            "app/main.cpp::geometry::Value::i",
        ),
        ("Extends", "app/main.cpp::geometry::Circle", "Shape"),
        ("Extends", "app/main.cpp::geometry::Multi", "Point"),
        ("Extends", "app/main.cpp::geometry::Multi", "Shape"),
        (
            "HasMethod",
            "app/main.cpp::geometry::Circle",
            "app/main.cpp::geometry::Circle::area#6",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Container",
            "app/main.cpp::geometry::Container::i#9",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Container",
            "app/main.cpp::geometry::Container::item#8",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Multi",
            "app/main.cpp::geometry::Multi::draw#7",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Point",
            "app/main.cpp::geometry::Point::magnitude#1",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Shape",
            "app/main.cpp::geometry::Shape::area#2",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Shape",
            "app/main.cpp::geometry::Shape::getId#4",
        ),
        (
            "HasMethod",
            "app/main.cpp::geometry::Shape",
            "app/main.cpp::geometry::Shape::other#3",
        ),
        ("Imports", "app/main.cpp", "iostream"),
        ("Imports", "app/main.cpp", "myheader.h"),
        ("Imports", "app/main.cpp", "namespace std"),
        ("Imports", "app/main.cpp", "std::vector"),
        ("Imports", "app/main.cpp", "sys/types.h"),
    ]
}

fn parse() -> ParseResult {
    parse_file(CORPUS, PATH, Language::Cpp).expect("cpp parse must not hard-fail")
}

#[test]
fn cpp_spec_output_is_exact_parity() {
    let r = parse();
    assert_eq!(r.parse_errors, 0, "clean C++ must report 0 parse errors");

    let obs: BTreeSet<String> = r.nodes.iter().map(node_record).collect();
    let exp: BTreeSet<String> = expected_node_records()
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        obs, exp,
        "C++ node set (full record) diverged from the hand-written walker's ground truth"
    );

    let obs_refs: BTreeSet<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp_refs: BTreeSet<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();
    assert_eq!(
        obs_refs, exp_refs,
        "C++ ref set diverged from the hand-written walker's ground truth"
    );
}

#[test]
fn cpp_per_edge_kind_f1_is_at_parity() {
    let r = parse();
    let obs_refs: Vec<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp: Vec<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();

    for kind in ["Defines", "HasMethod", "Extends", "Imports", "Calls"] {
        let exp_k: BTreeSet<(String, String)> = exp
            .iter()
            .filter(|(k, _, _)| k == kind)
            .map(|(_, f, t)| (f.clone(), t.clone()))
            .collect();
        let obs_k: BTreeSet<(String, String)> = obs_refs
            .iter()
            .filter(|(k, _, _)| k == kind)
            .map(|(_, f, t)| (f.clone(), t.clone()))
            .collect();
        let tp = exp_k.intersection(&obs_k).count();
        let fp = obs_k.difference(&exp_k).count();
        let fn_ = exp_k.difference(&obs_k).count();
        let precision = if tp + fp == 0 {
            1.0
        } else {
            tp as f64 / (tp + fp) as f64
        };
        let recall = if tp + fn_ == 0 {
            1.0
        } else {
            tp as f64 / (tp + fn_) as f64
        };
        let f1 = if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        };
        println!(
            "edge[{kind:<9}] P={precision:.3} R={recall:.3} F1={f1:.3} (tp={tp} fp={fp} fn={fn_})"
        );
        assert!(
            (f1 - 1.0).abs() < f64::EPSILON,
            "C++ {kind} F1 {f1:.3} is below parity 1.000"
        );
    }
}
