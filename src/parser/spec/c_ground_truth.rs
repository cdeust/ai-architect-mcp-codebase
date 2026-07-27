// parser::spec::c_ground_truth — the C extraction ground truth: the corpus and
// the exact node/ref sets it must produce. The assertions over it live in the
// sibling `c_parity_tests` module; keeping the tables here is what holds both
// files under the 500-line cap (coding-standards §4.1). Mirrors the C++ split
// (`cpp_ground_truth`).
//
// walker at EXACT parity with the hand-written walker it replaced
// (ADR-0055 phase 6, §5 step 3).
//
// The expected records below ARE the pre-migration `parse_c_file` output
// (full 7-tuple per node + full ref triples), captured mechanically from the
// hand-written walker on this corpus before it was deleted. Per-EdgeKind
// F1(new-vs-groundtruth) = 1.000 == F1(old-vs-groundtruth). The test parses
// through the crate's public `parse_file`, so it also covers the C dispatch arm.
//
// The corpus exercises every C concern the flat walker handles, plus the edge
// cases that pin specific behaviors (each preserved for parity, some documenting
// a pre-existing defect filed separately: naming #106, macros/inline-struct #107):
//   - `int add(int a, int b)` (def) and `int add(int a, int b);` (prototype):
//     both are named `add`. This is the ONE place this ground truth deliberately
//     DIVERGES from the deleted hand-written walker: its `find_identifier` LIFO-DFS
//     reached the parameter list before the declarator's own name and produced `b`
//     (the last parameter) for both. Issue #106 fixed that, so the expected values
//     below were updated by intent, not by rebaselining a failure. Every other row
//     is byte-identical to the pre-fix ground truth — the diff is exactly the two
//     Function rows plus the five CallSite/Calls QNs re-scoped under `add#3`.
//   - `int width, height;` — one field_declaration with TWO names; both surface.
//   - `char *name;` / `char buf[8];` / `int (*handler)(int, void *)` — pointer,
//     array, and function-pointer declarators all unwrap to the bare field name.
//   - `struct Point *next;` — the field's type_annotation is `struct Point`.
//   - `(fp)()` — a parenthesized (non-identifier) callee, DROPPED (no CallSite;
//     negative assertion). `obj.method` / `ptr->call` — member-access callees
//     resolve to the tail (`method` / `call`). `_internal()` — underscore callee kept.
//   - `#ifdef DEBUG … #endif` and `#if defined(FEATURE) … #endif` — the flat
//     walker recurses transparently through preprocessor wrappers, so `dbg`
//     (prototype), `Gated` (struct), and `gated` (function + call) inside them
//     are still extracted.
//   - `#define MAX 10` → `Constant` and `#define SQUARE(x) …` → `Function`,
//     both `macro=true` (issue #107). The deleted hand-written walker modelled
//     neither, so these rows are a DELIBERATE divergence from it, not a
//     rebaseline. No calls are scanned from a macro body: a replacement list is
//     unexpanded tokens, not an expression.
//   - `typedef struct { int ax; int ay; } Anon;` and
//     `struct Tagged { int tv; } tagged_var;` — a struct defined INLINE inside a
//     typedef or a declaration. Its body lives in the outer node's `type` field,
//     which the flat top-level scan never reached, so its FIELDS were invisible
//     (issue #107). `typedef struct Point PointT;` is the control: that is a
//     REFERENCE to an existing struct (no `body` field), and must NOT re-emit
//     `Point` — an earlier draft of the fix did exactly that and produced a
//     duplicate one-line `Point` node.
//   - `int global_var;` — a plain variable declaration, NOT a prototype (skipped).
//   - `int (*signal_handler)(int) = 0;` — a function-POINTER variable. This is the
//     ONE place this C ground truth deliberately DIVERGES from the deleted
//     hand-written walker (and its own earlier migration): a `pointer_declarator`
//     sits between the `function_declarator` and the name, so it is DATA, not a
//     callable. The old `is_c_function_prototype` saw the `function_declarator`
//     inside the `init_declarator` and mislabeled it a prototype `Function`
//     (`signal_handler#13`) — the C analog of the C++ defect #135. Issue #135
//     fixed both, so the expected values below were updated BY INTENT: the single
//     `Function|signal_handler#13` node and its one `Defines` edge are removed
//     (the flat C walker does not model file-scope variables, so a function
//     pointer emits nothing), and every other row is byte-identical. `#13` was
//     the highest `seq`, so no other QN shifts.
//   - `int counter = 5;` — an initialized variable (an `init_declarator` with NO
//     function declarator) is NOT a prototype: it emits NOTHING (negative
//     assertion; both the old walker and the flat walker skip it, pinning the
//     inner `func_declarator` test inside the `init_declarator` branch).

pub(super) const PATH: &str = "app/main.c";

pub(super) const CORPUS: &str = r#"#include <stdio.h>
#include "config.h"
#include <sys/types.h>

#define MAX 10
#define SQUARE(x) ((x) * (x))

struct Point {
    int x;
    int y;
    int width, height;
    char *name;
    char buf[8];
    int (*handler)(int, void *);
    struct Point *next;
};

union Value {
    int i;
    float f;
};

enum Color {
    RED,
    GREEN = 5,
    BLUE
};

typedef struct Point PointT;
typedef unsigned long ulong_t;
typedef int (*Callback)(int);

int global_var;

int add(int a, int b);

static int helper(void) {
    return MAX;
}

int add(int a, int b) {
    int r = helper();
    printf("%d", r);
    obj.method(a);
    ptr->call(b);
    _internal();
    (fp)();
    return a + b;
}

void empty(void) {}

#ifdef DEBUG
int dbg(void);
#endif

#if defined(FEATURE)
struct Gated {
    int flag;
};
void gated(void) {
    dbg();
}
#endif

int (*signal_handler)(int) = 0;

int counter = 5;

typedef struct {
    int ax;
    int ay;
} Anon;

struct Tagged {
    int tv;
} tagged_var;
"#;

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
