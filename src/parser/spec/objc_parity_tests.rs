// parser::spec::objc_parity_tests — pins the Objective-C migration to the
// dedicated `objc` walker at EXACT parity with the hand-written walker it
// replaced (ADR-0055 phase 8, §5 step 3).
//
// The expected records below ARE the pre-migration `parse_objc_file` output
// (full 7-tuple per node + full ref triples), captured mechanically from the
// hand-written walker on this corpus BEFORE it was deleted — re-captured on an
// `origin/main` worktree after the corpus was extended for mutation coverage
// (37 nodes, 38 refs, 0 parse errors). Per-EdgeKind F1(new-vs-groundtruth) =
// 1.000 == F1(old-vs-groundtruth). The test parses through the crate's public
// `parse_file`, so it also covers the ObjC dispatch arm.
//
// The exact-multiset assertion (sorted `Vec`, not a set) is the mutation oracle:
// it kills every walker mutant that perturbs, adds, or drops any emitted node or
// ref — including the THREE identical `Defines(app/Shape.m → …::Shape)` edges the
// interface, implementation, and category each emit (a set would collapse them,
// so a mutant dropping one would survive; the multiset does not).
//
// The corpus exercises every ObjC concern the walker handles, plus the edge
// cases that pin specific behaviors (each preserved for parity, two documenting
// a pre-existing defect filed separately) AND the constructs added to kill
// otherwise-surviving mutants (§12), each noted `[mutant]`:
//   - `#import <…>` / `#import "…"` / `#include <…>` → `Import` (display = last
//     path segment, edge = full path); `@import UIKit;` → `Import` (display =
//     full module text, NOT a path segment).
//   - Plain C `struct`/`union` → `Struct` + `Field`s (`HasField`), including the
//     multi-declarator `int x, y;` → TWO fields with `type_annotation`.
//   - `struct Wrapper { int z; } wvar;` — a named struct wrapped in a
//     `declaration` (it has a variable), reached ONLY via the walker's
//     transparent-recursion arm; `struct { int q; } anon;` — an ANONYMOUS struct
//     (also `declaration`-wrapped) that resolves to an empty name and emits
//     NOTHING. [mutant] Wrapper kills `walk_objc_defs`'s `named_child_count() >
//     0` → `<`/`==` (they stop recursing, dropping Wrapper+z); anon pins that an
//     anonymous specifier contributes nothing (kills the `name.is_empty()` guard
//     mutant, which would otherwise emit a nameless `Struct`).
//   - `enum Dir { NORTH, SOUTH = 5 }` → `Enum` + two `Constant`s (`enum_entry`);
//     an enumerator WITH a value still resolves to its name.
//   - `typedef int MyInt;` → `Constant` (`typedef=true`). `typedef struct Node {
//     int v; } NodeT;` → `Constant|NodeT` PLUS the inline `Node` struct as
//     `Struct|Node` + `Field|v` (issue #127 fixed — the ObjC lane now recurses a
//     typedef's inline type, as C did in #107).
//   - `@protocol Drawable` → a `Trait` with NO member extraction (the `- (void)
//     draw;` inside it emits NOTHING — negative assertion).
//   - `@interface Shape : NSObject <Drawable>` → a `Struct` keyed `…::Shape`,
//     one `Extends` to `NSObject` + `bases="NSObject"` (issue #216 fix; the
//     `<Drawable>` conformance is NOT emitted); `@implementation Shape` and
//     the category `@interface Shape (Extras)` are
//     TWO more `Struct` nodes on the SAME QN (three `Defines(→…::Shape)`). The
//     category carries `is_category=true` + `category=Extras`.
//   - Methods: instance `- (void)start;` and class `+ (instancetype)shapeNamed:`
//     both → `Method` (`receiver_type = …::Shape`), keyed `…::{selector}#{seq}`.
//     The keyword-selector method `- (int)areaWithWidth:(int)w height:(int)h;`
//     resolves to name `areaWithWidth` — the grammar shapes the selector as a
//     bare `identifier` + `method_parameter`s (no `keyword_declarator`), so only
//     the first keyword is captured (pre-existing defect #128, preserved).
//   - Message sends: `[self doThingWith:1 andY:2]` → `Calls doThingWith:andY:`
//     (keyword send joins `:`), `[self compute:w]` → `Calls compute:`,
//     `[self refresh]` → `Calls refresh` (UNARY send, no `:`), `NSLog(@"hi")` →
//     `Calls NSLog`; a plain call `obj.method(b)` splits on `.` → `Calls method`,
//     `printf(…)` → `Calls printf`. [mutant] the unary `[self refresh]` kills
//     `message_selector`'s `&&` → `||` and `>` → `>=` (both would emit
//     `refresh:` instead of `refresh`). Call `seq`s are assigned in REVERSE
//     source order within a body (stack DFS), keying the `call@line:col#seq` QNs.
//   - `#ifdef DEBUG int gated(int a) { return helper(a); } #endif` — a function
//     wrapped in `preproc_ifdef`, reached ONLY via the transparent-recursion
//     arm. [mutant] a second, independent kill of `walk_objc_defs`'s recursion
//     guard (drops `gated` + its `helper` call), mirroring the C++ `#ifdef`
//     fixture (#125).
//   - One per-file `seq` keys methods, functions, AND call sites in one DFS
//     order (`start#1 areaWithWidth#2 shapeNamed#3` in the interface, then
//     `start#4` + its calls `#5 #6 #7 #8`, `areaWithWidth#9` + `compute:#10`,
//     `extra#11`, `freeFunction#12` + `method#13 printf#14`, `gated#15` +
//     `helper#16`).

use std::collections::BTreeSet;

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

const CORPUS: &str = r#"#import <Foundation/Foundation.h>
#import "MyHeader.h"
#include <stdio.h>
@import UIKit;

struct Point { int x, y; };
union Value { int i; float f; };
enum Dir { NORTH, SOUTH = 5 };
typedef int MyInt;
typedef struct Node { int v; } NodeT;
struct Wrapper { int z; } wvar;
struct { int q; } anon;

@protocol Drawable
- (void)draw;
@end

@interface Shape : NSObject <Drawable>
- (void)start;
- (int)areaWithWidth:(int)w height:(int)h;
+ (instancetype)shapeNamed:(NSString *)name;
@end

@implementation Shape
- (void)start {
    [self doThingWith:1 andY:2];
    [self refresh];
    NSLog(@"hi");
    helper();
}
- (int)areaWithWidth:(int)w height:(int)h {
    return [self compute:w];
}
@end

@interface Shape (Extras)
- (void)extra;
@end

void freeFunction(int a, int b) {
    printf("%d", a);
    obj.method(b);
}

#ifdef DEBUG
int gated(int a) {
    return helper(a);
}
#endif
"#;

const PATH: &str = "app/Shape.m";

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
        "CallSite|NSLog|app/Shape.m::Shape::start#4::call@28:5#6|28|28|public|[(\"callee_name\", \"NSLog\")]",
        "CallSite|compute:|app/Shape.m::Shape::areaWithWidth:height:#9::call@32:12#10|32|32|public|[(\"callee_name\", \"compute:\")]",
        "CallSite|doThingWith:andY:|app/Shape.m::Shape::start#4::call@26:5#8|26|26|public|[(\"callee_name\", \"doThingWith:andY:\")]",
        "CallSite|helper|app/Shape.m::Shape::start#4::call@29:5#5|29|29|public|[(\"callee_name\", \"helper\")]",
        "CallSite|helper|app/Shape.m::gated#15::call@47:12#16|47|47|public|[(\"callee_name\", \"helper\")]",
        "CallSite|method|app/Shape.m::freeFunction#12::call@42:5#13|42|42|public|[(\"callee_name\", \"method\")]",
        "CallSite|printf|app/Shape.m::freeFunction#12::call@41:5#14|41|41|public|[(\"callee_name\", \"printf\")]",
        "CallSite|refresh|app/Shape.m::Shape::start#4::call@27:5#7|27|27|public|[(\"callee_name\", \"refresh\")]",
        "Constant|MyInt|app/Shape.m::MyInt|9|9|public|[(\"typedef\", \"true\")]",
        "Constant|NORTH|app/Shape.m::Dir::NORTH|8|8|public|[(\"enum_entry\", \"true\")]",
        "Constant|NodeT|app/Shape.m::NodeT|10|10|public|[(\"typedef\", \"true\")]",
        "Constant|SOUTH|app/Shape.m::Dir::SOUTH|8|8|public|[(\"enum_entry\", \"true\")]",
        "Enum|Dir|app/Shape.m::Dir|8|8|public|[]",
        "Field|f|app/Shape.m::Value::f|7|7|public|[(\"type_annotation\", \"float\")]",
        "Field|i|app/Shape.m::Value::i|7|7|public|[(\"type_annotation\", \"int\")]",
        "Field|v|app/Shape.m::Node::v|10|10|public|[(\"type_annotation\", \"int\")]",
        "Field|x|app/Shape.m::Point::x|6|6|public|[(\"type_annotation\", \"int\")]",
        "Field|y|app/Shape.m::Point::y|6|6|public|[(\"type_annotation\", \"int\")]",
        "Field|z|app/Shape.m::Wrapper::z|11|11|public|[(\"type_annotation\", \"int\")]",
        "Function|freeFunction|app/Shape.m::freeFunction#12|40|43|public|[]",
        "Function|gated|app/Shape.m::gated#15|46|48|public|[]",
        "Import|Foundation.h|app/Shape.m::import:Foundation/Foundation.h|1|2|public|[(\"path\", \"Foundation/Foundation.h\")]",
        "Import|MyHeader.h|app/Shape.m::import:MyHeader.h|2|3|public|[(\"path\", \"MyHeader.h\")]",
        "Import|UIKit|app/Shape.m::import:UIKit|4|4|public|[(\"path\", \"UIKit\")]",
        "Import|stdio.h|app/Shape.m::import:stdio.h|3|4|public|[(\"path\", \"stdio.h\")]",
        "Method|areaWithWidth:height:|app/Shape.m::Shape::areaWithWidth:height:#2|20|20|public|[(\"receiver_type\", \"app/Shape.m::Shape\")]",
        "Method|areaWithWidth:height:|app/Shape.m::Shape::areaWithWidth:height:#9|31|33|public|[(\"receiver_type\", \"app/Shape.m::Shape\")]",
        "Method|extra|app/Shape.m::Shape::extra#11|37|37|public|[(\"receiver_type\", \"app/Shape.m::Shape\")]",
        "Method|shapeNamed:|app/Shape.m::Shape::shapeNamed:#3|21|21|public|[(\"receiver_type\", \"app/Shape.m::Shape\")]",
        "Method|start|app/Shape.m::Shape::start#1|19|19|public|[(\"receiver_type\", \"app/Shape.m::Shape\")]",
        "Method|start|app/Shape.m::Shape::start#4|25|30|public|[(\"receiver_type\", \"app/Shape.m::Shape\")]",
        "Struct|Node|app/Shape.m::Node|10|10|public|[]",
        "Struct|Point|app/Shape.m::Point|6|6|public|[]",
        "Struct|Shape|app/Shape.m::Shape|18|22|public|[(\"bases\", \"NSObject\")]",
        "Struct|Shape|app/Shape.m::Shape|24|34|public|[]",
        "Struct|Shape|app/Shape.m::Shape|36|38|public|[(\"is_category\", \"true\"), (\"category\", \"Extras\")]",
        "Struct|Value|app/Shape.m::Value|7|7|public|[]",
        "Struct|Wrapper|app/Shape.m::Wrapper|11|11|public|[]",
        "Trait|Drawable|app/Shape.m::Drawable|14|16|public|[]",
    ]
}

/// The full 38-triple ref MULTISET (with the three identical
/// `Defines(→…::Shape)` edges). Order-independent, so it is compared as a sorted
/// `Vec` to keep the duplicates.
fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Calls",
            "app/Shape.m::Shape::areaWithWidth:height:#9",
            "compute:",
        ),
        ("Calls", "app/Shape.m::Shape::start#4", "NSLog"),
        ("Calls", "app/Shape.m::Shape::start#4", "doThingWith:andY:"),
        ("Calls", "app/Shape.m::Shape::start#4", "helper"),
        ("Calls", "app/Shape.m::Shape::start#4", "refresh"),
        ("Calls", "app/Shape.m::freeFunction#12", "method"),
        ("Calls", "app/Shape.m::freeFunction#12", "printf"),
        ("Calls", "app/Shape.m::gated#15", "helper"),
        ("Defines", "app/Shape.m::Dir", "app/Shape.m::Dir::NORTH"),
        ("Defines", "app/Shape.m::Dir", "app/Shape.m::Dir::SOUTH"),
        ("Defines", "app/Shape.m", "app/Shape.m::Dir"),
        ("Defines", "app/Shape.m", "app/Shape.m::Drawable"),
        ("Defines", "app/Shape.m", "app/Shape.m::MyInt"),
        ("Defines", "app/Shape.m", "app/Shape.m::Node"),
        ("Defines", "app/Shape.m", "app/Shape.m::NodeT"),
        ("Defines", "app/Shape.m", "app/Shape.m::Point"),
        ("Defines", "app/Shape.m", "app/Shape.m::Shape"),
        ("Defines", "app/Shape.m", "app/Shape.m::Shape"),
        ("Defines", "app/Shape.m", "app/Shape.m::Shape"),
        ("Defines", "app/Shape.m", "app/Shape.m::Value"),
        ("Defines", "app/Shape.m", "app/Shape.m::Wrapper"),
        ("Defines", "app/Shape.m", "app/Shape.m::freeFunction#12"),
        ("Defines", "app/Shape.m", "app/Shape.m::gated#15"),
        ("Extends", "app/Shape.m::Shape", "NSObject"),
        ("HasField", "app/Shape.m::Node", "app/Shape.m::Node::v"),
        ("HasField", "app/Shape.m::Point", "app/Shape.m::Point::x"),
        ("HasField", "app/Shape.m::Point", "app/Shape.m::Point::y"),
        ("HasField", "app/Shape.m::Value", "app/Shape.m::Value::f"),
        ("HasField", "app/Shape.m::Value", "app/Shape.m::Value::i"),
        (
            "HasField",
            "app/Shape.m::Wrapper",
            "app/Shape.m::Wrapper::z",
        ),
        (
            "HasMethod",
            "app/Shape.m::Shape",
            "app/Shape.m::Shape::areaWithWidth:height:#2",
        ),
        (
            "HasMethod",
            "app/Shape.m::Shape",
            "app/Shape.m::Shape::areaWithWidth:height:#9",
        ),
        (
            "HasMethod",
            "app/Shape.m::Shape",
            "app/Shape.m::Shape::extra#11",
        ),
        (
            "HasMethod",
            "app/Shape.m::Shape",
            "app/Shape.m::Shape::shapeNamed:#3",
        ),
        (
            "HasMethod",
            "app/Shape.m::Shape",
            "app/Shape.m::Shape::start#1",
        ),
        (
            "HasMethod",
            "app/Shape.m::Shape",
            "app/Shape.m::Shape::start#4",
        ),
        ("Imports", "app/Shape.m", "Foundation/Foundation.h"),
        ("Imports", "app/Shape.m", "MyHeader.h"),
        ("Imports", "app/Shape.m", "UIKit"),
        ("Imports", "app/Shape.m", "stdio.h"),
    ]
}

fn parse() -> ParseResult {
    parse_file(CORPUS, PATH, Language::ObjC).expect("objc parse must not hard-fail")
}

#[test]
fn objc_spec_output_is_exact_parity() {
    let r = parse();
    assert_eq!(r.parse_errors, 0, "clean ObjC must report 0 parse errors");
    assert_eq!(
        r.nodes.len(),
        39,
        "ObjC node count diverged from ground truth"
    );
    assert_eq!(
        r.refs.len(),
        40,
        "ObjC ref count diverged from ground truth"
    );

    let mut obs: Vec<String> = r.nodes.iter().map(node_record).collect();
    obs.sort();
    let mut exp: Vec<String> = expected_node_records()
        .into_iter()
        .map(String::from)
        .collect();
    exp.sort();
    assert_eq!(
        obs, exp,
        "ObjC node multiset (full record) diverged from the hand-written walker's ground truth"
    );

    let mut obs_refs: Vec<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    obs_refs.sort();
    let mut exp_refs: Vec<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();
    exp_refs.sort();
    assert_eq!(
        obs_refs, exp_refs,
        "ObjC ref multiset diverged from the hand-written walker's ground truth"
    );
}

#[test]
fn objc_per_edge_kind_f1_is_at_parity() {
    let r = parse();
    let obs_refs: Vec<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp: Vec<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();

    for kind in [
        "Defines",
        "HasMethod",
        "HasField",
        "Extends",
        "Imports",
        "Calls",
    ] {
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
            "ObjC {kind} F1 {f1:.3} is below parity 1.000"
        );
    }
}

/// A `@protocol`'s member declarations are NOT extracted, the `<Drawable>`
/// conformance clause is NOT an edge, and an ANONYMOUS `struct { … }` emits
/// nothing. Negative assertions the multiset test covers implicitly, pinned
/// explicitly so a future walker that starts emitting any of them fails loudly
/// against parity.
#[test]
fn objc_negative_space_is_empty() {
    let r = parse();
    assert!(
        !r.nodes.iter().any(|n| n.name == "draw"),
        "a protocol's method declarations must NOT be extracted (parity)"
    );
    assert!(
        !r.refs.iter().any(|e| e.to_qualified_name == "Drawable"),
        "protocol conformance <Drawable> must NOT emit an edge (parity)"
    );
    assert!(
        !r.nodes
            .iter()
            .any(|n| n.name == "struct" || n.name == "anon" || n.name.is_empty()),
        "an anonymous struct must resolve to an empty name and emit nothing (parity)"
    );
}
