// parser::spec::ts_parity_tests — pins the TypeScript migration to the dedicated
// `ts` walker at EXACT parity with the hand-written walker it replaced
// (ADR-0055 phase 7, §5 step 3).
//
// The expected records below ARE the pre-migration `parse_typescript_file`
// output — full 7-tuple per node, full triple per ref, **in emission order** —
// captured mechanically from the hand-written walker on this corpus before it was
// deleted (46 nodes / 60 refs / 0 parse errors on the `.ts` corpus; 4 nodes / 6
// refs / 0 errors on the `.tsx` corpus). The old-vs-new diff on that capture was
// empty, so per-EdgeKind F1(new-vs-groundtruth) = 1.000 == F1(old-vs-groundtruth).
// The test parses through the crate's public `parse_file`, so it also covers the
// TypeScript dispatch arm and the dialect selection.
//
// Comparison is on ORDERED vectors, not sets: TypeScript legitimately emits
// duplicate records (a getter/setter pair shares one QN, so `Animal::label`
// appears twice with two `HasMethod` refs), which a set would silently collapse.
// The ordered equality is also the mutation oracle — it kills every walker mutant
// that perturbs any emitted node, ref, or their order.
//
// The corpus exercises every TypeScript concern the walker handles, plus the edge
// cases that pin specific behaviors (each preserved for parity):
//   - Imports, all four shapes: side-effect (`import 'reflect-metadata'` → the
//     module path is the display name), default (`defaultExport` → path
//     `package::default`), named with and without an alias (`foo` displays as its
//     path `.::module::foo`; `bar as baz` displays as `baz`), and namespace
//     (`* as utils` → `is_glob=true`, aliased `utils`). Paths are `/`→`::`
//     normalized and quote-stripped; every import edge is `Defines`, not
//     `Imports`.
//   - `export`-as-wrapper visibility: `export function greet` → `pub` (its
//     previous sibling IS the `export` token), and `export default function main`
//     → `pub` only via the INHERITED wrapper flag (its previous sibling is
//     `default`). A non-exported `function* gen` / `const notExported` → empty
//     visibility.
//   - `const`/`let`/`var`: `export const MAX_RETRIES: number = 3` → `Constant`
//     (`type_annotation` = `": number"`), `const localFlag` → `Constant` with an
//     EMPTY `type_annotation` (the property is always present), while
//     `let mutable = 0` and `var legacy = 1` emit NOTHING (negative assertions).
//   - Arrow functions: `export const handler = async (req) => {…}` → a
//     `Function` whose line span is the DECLARATOR's, `is_async=true` read off
//     the ARROW, and whose body IS call-scanned.
//   - Classes: `abstract class Base` → `Struct`; `class Animal extends Base
//     implements Serializable` → one `Extends` + one `Implements`;
//     `class Service<T> extends Container<T>` → `Extends Container` (the
//     generic's `name` field). A class node carries NO `bases` property.
//   - Members: `public`/`private`/`protected` fields → `Field`/`HasField` with
//     modifier visibility; `constructor`, `get`/`set label`, `async speak`,
//     `static create` → `Method`/`HasMethod` with `is_async` + `receiver_type`.
//     The getter/setter pair pins the NON-deduplicated QN. `abstract area():
//     number;` is an `abstract_method_signature` and emits NOTHING.
//   - Decorators: `@Injectable()` on a class and `@observe()` on a method are
//     dropped, and neither breaks the dispatch of the node they decorate.
//   - Interfaces: `interface Serializable extends Base, Comparable<string>` →
//     `Trait` + ONE `Extends` (`Base`) — the generic `Comparable<string>` is
//     dropped, the class/interface asymmetry the hand-written walker had.
//     `serialize()` → `Method` (`is_async=false`), `readonly id: number` →
//     `Field`.
//   - Enums: `enum_assignment` members (`Red = "RED"`) and BARE members
//     (`enum Bare { A, B }` → `property_identifier`) both → `Variant` +
//     `HasVariant`.
//   - Type aliases: `target_type` from the `value` field, including a function
//     type (`(input: T) => void`).
//   (The TSX dialect's own parity records and the per-extension grammar
//   selection live in the sibling `ts_dialect_tests`, which reuses this module's
//   record helpers.)
//   - Calls: emitted in REVERSE source order (stack DFS), each with a
//     `(start_byte, end_byte)`-keyed QN, TWO refs (a `Defines` to the call-site
//     node and a `Calls` to the callee TAIL), and the full callee text as the
//     name — `utils.log` keeps its receiver in `callee_name` but edges as `log`.
//     A chained `factory()()` yields TWO call sites sharing a start byte (the
//     outer callee text is literally `factory()`), which is why the byte SPAN,
//     not the start, keys the QN. `new Animal(name)` is a `new_expression` and
//     yields NO call site (negative assertion).

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

const CORPUS: &str = r#"import 'reflect-metadata';
import defaultExport from 'package';
import { foo, bar as baz } from './module';
import * as utils from '../utils/helpers';

export const MAX_RETRIES: number = 3;
const localFlag = true;
let mutable = 0;
var legacy = 1;

export function greet(name: string): string {
    return `Hello, ${name}`;
}

export async function fetchData(url: string): Promise<Response> {
    return fetch(url);
}

function* gen() {
    yield 1;
}

export default function main() {
    greet("world");
    utils.log(baz());
    factory()();
}

export const handler = async (req: Request) => {
    greet("handler");
    foo();
};

const notExported = (x: number) => x + 1;

export abstract class Base {
    protected id: number = 0;
    abstract area(): number;
}

export class Animal extends Base implements Serializable {
    public name: string;
    private age: number;

    constructor(name: string) {
        super();
        this.name = name;
    }

    get label(): string {
        return this.name;
    }

    set label(v: string) {
        this.name = v;
    }

    async speak(): Promise<string> {
        return this.label;
    }

    static create(name: string): Animal {
        return new Animal(name);
    }
}

@Injectable()
export class Service<T> extends Container<T> {
    @observe()
    run(): void {
        this.execute();
    }
}

export interface Serializable extends Base, Comparable<string> {
    serialize(): string;
    readonly id: number;
}

export enum Color {
    Red = "RED",
    Green = "GREEN",
}

enum Bare {
    A,
    B,
}

export type StringOrNumber = string | number;
type Handler<T> = (input: T) => void;
"#;

const PATH: &str = "app/src/main.ts";

pub(super) fn node_record(n: &ExtractedNode) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{:?}",
        n.label, n.name, n.qualified_name, n.start_line, n.end_line, n.visibility, n.properties
    )
}

/// A ref as one `kind|from|to` string. Single-line records (rather than tuples)
/// keep this pinned ground truth legible and this file inside the §4.1 cap.
pub(super) fn ref_record(e: &ExtractedRef) -> String {
    format!(
        "{}|{}|{}",
        e.kind, e.from_qualified_name, e.to_qualified_name
    )
}

fn expected_node_records() -> Vec<&'static str> {
    vec![
    "Import|reflect-metadata|app/src/main.ts::reflect-metadata|1|1||[(\"path\", \"reflect-metadata\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
    "Import|defaultExport|app/src/main.ts::defaultExport|2|2||[(\"path\", \"package::default\"), (\"alias\", \"defaultExport\"), (\"is_glob\", \"false\")]",
    "Import|.::module::foo|app/src/main.ts::.::module::foo|3|3||[(\"path\", \".::module::foo\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
    "Import|baz|app/src/main.ts::baz|3|3||[(\"path\", \".::module::bar\"), (\"alias\", \"baz\"), (\"is_glob\", \"false\")]",
    "Import|utils|app/src/main.ts::utils|4|4||[(\"path\", \"..::utils::helpers\"), (\"alias\", \"utils\"), (\"is_glob\", \"true\")]",
    "Constant|MAX_RETRIES|app/src/main.ts::MAX_RETRIES|6|6|pub|[(\"type_annotation\", \": number\")]",
    "Constant|localFlag|app/src/main.ts::localFlag|7|7||[(\"type_annotation\", \"\")]",
    "Function|greet|app/src/main.ts::greet|11|13|pub|[(\"is_async\", \"false\")]",
    "Function|fetchData|app/src/main.ts::fetchData|15|17|pub|[(\"is_async\", \"true\")]",
    "CallSite|fetch|app/src/main.ts::fetchData::call@16:11#403-413|16|16||[(\"callee_name\", \"fetch\"), (\"caller_qn\", \"app/src/main.ts::fetchData\")]",
    "Function|gen|app/src/main.ts::gen|19|21||[(\"is_async\", \"false\")]",
    "Function|main|app/src/main.ts::main|23|27|pub|[(\"is_async\", \"false\")]",
    "CallSite|factory()|app/src/main.ts::main::call@26:4#531-542|26|26||[(\"callee_name\", \"factory()\"), (\"caller_qn\", \"app/src/main.ts::main\")]",
    "CallSite|factory|app/src/main.ts::main::call@26:4#531-540|26|26||[(\"callee_name\", \"factory\"), (\"caller_qn\", \"app/src/main.ts::main\")]",
    "CallSite|utils.log|app/src/main.ts::main::call@25:4#509-525|25|25||[(\"callee_name\", \"utils.log\"), (\"caller_qn\", \"app/src/main.ts::main\")]",
    "CallSite|baz|app/src/main.ts::main::call@25:14#519-524|25|25||[(\"callee_name\", \"baz\"), (\"caller_qn\", \"app/src/main.ts::main\")]",
    "CallSite|greet|app/src/main.ts::main::call@24:4#489-503|24|24||[(\"callee_name\", \"greet\"), (\"caller_qn\", \"app/src/main.ts::main\")]",
    "Function|handler|app/src/main.ts::handler|29|32|pub|[(\"is_async\", \"true\")]",
    "CallSite|foo|app/src/main.ts::handler::call@31:4#622-627|31|31||[(\"callee_name\", \"foo\"), (\"caller_qn\", \"app/src/main.ts::handler\")]",
    "CallSite|greet|app/src/main.ts::handler::call@30:4#600-616|30|30||[(\"callee_name\", \"greet\"), (\"caller_qn\", \"app/src/main.ts::handler\")]",
    "Function|notExported|app/src/main.ts::notExported|34|34||[(\"is_async\", \"false\")]",
    "Struct|Base|app/src/main.ts::Base|36|39|pub|[]",
    "Field|id|app/src/main.ts::Base::id|37|37|protected|[(\"type_annotation\", \": number\")]",
    "Struct|Animal|app/src/main.ts::Animal|41|65|pub|[]",
    "Field|name|app/src/main.ts::Animal::name|42|42|public|[(\"type_annotation\", \": string\")]",
    "Field|age|app/src/main.ts::Animal::age|43|43|private|[(\"type_annotation\", \": number\")]",
    "Method|constructor|app/src/main.ts::Animal::constructor|45|48||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/src/main.ts::Animal\")]",
    "CallSite|super|app/src/main.ts::Animal::constructor::call@46:8#917-924|46|46||[(\"callee_name\", \"super\"), (\"caller_qn\", \"app/src/main.ts::Animal::constructor\")]",
    "Method|label|app/src/main.ts::Animal::label|50|52||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/src/main.ts::Animal\")]",
    "Method|label|app/src/main.ts::Animal::label|54|56||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/src/main.ts::Animal\")]",
    "Method|speak|app/src/main.ts::Animal::speak|58|60||[(\"is_async\", \"true\"), (\"receiver_type\", \"app/src/main.ts::Animal\")]",
    "Method|create|app/src/main.ts::Animal::create|62|64||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/src/main.ts::Animal\")]",
    "Struct|Service|app/src/main.ts::Service|68|73|pub|[]",
    "Method|run|app/src/main.ts::Service::run|70|72||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/src/main.ts::Service\")]",
    "CallSite|this.execute|app/src/main.ts::Service::run::call@71:8#1332-1346|71|71||[(\"callee_name\", \"this.execute\"), (\"caller_qn\", \"app/src/main.ts::Service::run\")]",
    "Trait|Serializable|app/src/main.ts::Serializable|75|78|pub|[]",
    "Method|serialize|app/src/main.ts::Serializable::serialize|76|76||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/src/main.ts::Serializable\")]",
    "Field|id|app/src/main.ts::Serializable::id|77|77||[(\"type_annotation\", \": number\")]",
    "Enum|Color|app/src/main.ts::Color|80|83|pub|[]",
    "Variant|Red|app/src/main.ts::Color::Red|81|81||[]",
    "Variant|Green|app/src/main.ts::Color::Green|82|82||[]",
    "Enum|Bare|app/src/main.ts::Bare|85|88||[]",
    "Variant|A|app/src/main.ts::Bare::A|86|86||[]",
    "Variant|B|app/src/main.ts::Bare::B|87|87||[]",
    "TypeAlias|StringOrNumber|app/src/main.ts::StringOrNumber|90|90|pub|[(\"target_type\", \"string | number\")]",
    "TypeAlias|Handler|app/src/main.ts::Handler|91|91||[(\"target_type\", \"(input: T) => void\")]",
    ]
}

fn expected_ref_records() -> Vec<&'static str> {
    vec![
    "Defines|app/src/main.ts|app/src/main.ts::reflect-metadata",
    "Defines|app/src/main.ts|app/src/main.ts::defaultExport",
    "Defines|app/src/main.ts|app/src/main.ts::.::module::foo",
    "Defines|app/src/main.ts|app/src/main.ts::baz",
    "Defines|app/src/main.ts|app/src/main.ts::utils",
    "Defines|app/src/main.ts|app/src/main.ts::MAX_RETRIES",
    "Defines|app/src/main.ts|app/src/main.ts::localFlag",
    "Defines|app/src/main.ts|app/src/main.ts::greet",
    "Defines|app/src/main.ts|app/src/main.ts::fetchData",
    "Defines|app/src/main.ts::fetchData|app/src/main.ts::fetchData::call@16:11#403-413",
    "Calls|app/src/main.ts::fetchData|fetch",
    "Defines|app/src/main.ts|app/src/main.ts::gen",
    "Defines|app/src/main.ts|app/src/main.ts::main",
    "Defines|app/src/main.ts::main|app/src/main.ts::main::call@26:4#531-542",
    "Calls|app/src/main.ts::main|factory()",
    "Defines|app/src/main.ts::main|app/src/main.ts::main::call@26:4#531-540",
    "Calls|app/src/main.ts::main|factory",
    "Defines|app/src/main.ts::main|app/src/main.ts::main::call@25:4#509-525",
    "Calls|app/src/main.ts::main|log",
    "Defines|app/src/main.ts::main|app/src/main.ts::main::call@25:14#519-524",
    "Calls|app/src/main.ts::main|baz",
    "Defines|app/src/main.ts::main|app/src/main.ts::main::call@24:4#489-503",
    "Calls|app/src/main.ts::main|greet",
    "Defines|app/src/main.ts|app/src/main.ts::handler",
    "Defines|app/src/main.ts::handler|app/src/main.ts::handler::call@31:4#622-627",
    "Calls|app/src/main.ts::handler|foo",
    "Defines|app/src/main.ts::handler|app/src/main.ts::handler::call@30:4#600-616",
    "Calls|app/src/main.ts::handler|greet",
    "Defines|app/src/main.ts|app/src/main.ts::notExported",
    "Defines|app/src/main.ts|app/src/main.ts::Base",
    "HasField|app/src/main.ts::Base|app/src/main.ts::Base::id",
    "Defines|app/src/main.ts|app/src/main.ts::Animal",
    "Extends|app/src/main.ts::Animal|Base",
    "Implements|app/src/main.ts::Animal|Serializable",
    "HasField|app/src/main.ts::Animal|app/src/main.ts::Animal::name",
    "HasField|app/src/main.ts::Animal|app/src/main.ts::Animal::age",
    "HasMethod|app/src/main.ts::Animal|app/src/main.ts::Animal::constructor",
    "Defines|app/src/main.ts::Animal::constructor|app/src/main.ts::Animal::constructor::call@46:8#917-924",
    "Calls|app/src/main.ts::Animal::constructor|super",
    "HasMethod|app/src/main.ts::Animal|app/src/main.ts::Animal::label",
    "HasMethod|app/src/main.ts::Animal|app/src/main.ts::Animal::label",
    "HasMethod|app/src/main.ts::Animal|app/src/main.ts::Animal::speak",
    "HasMethod|app/src/main.ts::Animal|app/src/main.ts::Animal::create",
    "Defines|app/src/main.ts|app/src/main.ts::Service",
    "Extends|app/src/main.ts::Service|Container",
    "HasMethod|app/src/main.ts::Service|app/src/main.ts::Service::run",
    "Defines|app/src/main.ts::Service::run|app/src/main.ts::Service::run::call@71:8#1332-1346",
    "Calls|app/src/main.ts::Service::run|execute",
    "Defines|app/src/main.ts|app/src/main.ts::Serializable",
    "Extends|app/src/main.ts::Serializable|Base",
    "HasMethod|app/src/main.ts::Serializable|app/src/main.ts::Serializable::serialize",
    "HasField|app/src/main.ts::Serializable|app/src/main.ts::Serializable::id",
    "Defines|app/src/main.ts|app/src/main.ts::Color",
    "HasVariant|app/src/main.ts::Color|app/src/main.ts::Color::Red",
    "HasVariant|app/src/main.ts::Color|app/src/main.ts::Color::Green",
    "Defines|app/src/main.ts|app/src/main.ts::Bare",
    "HasVariant|app/src/main.ts::Bare|app/src/main.ts::Bare::A",
    "HasVariant|app/src/main.ts::Bare|app/src/main.ts::Bare::B",
    "Defines|app/src/main.ts|app/src/main.ts::StringOrNumber",
    "Defines|app/src/main.ts|app/src/main.ts::Handler",
    ]
}

pub(super) fn parse_ts(source: &str, path: &str) -> ParseResult {
    parse_file(source, path, Language::TypeScript).expect("TypeScript parse must not hard-fail")
}

/// Asserts the observed nodes/refs equal `exp_nodes`/`exp_refs` exactly, in
/// order. `what` names the corpus in the failure message.
pub(super) fn assert_records(
    r: &ParseResult,
    exp_nodes: Vec<&str>,
    exp_refs: Vec<&str>,
    what: &str,
) {
    let obs_nodes: Vec<String> = r.nodes.iter().map(node_record).collect();
    let exp_nodes: Vec<String> = exp_nodes.into_iter().map(String::from).collect();
    assert_eq!(
        obs_nodes, exp_nodes,
        "{what}: node records (full 7-tuple, in order) diverged from the \
         hand-written walker's ground truth"
    );
    let obs_refs: Vec<String> = r.refs.iter().map(ref_record).collect();
    let exp_refs: Vec<String> = exp_refs.into_iter().map(String::from).collect();
    assert_eq!(
        obs_refs, exp_refs,
        "{what}: ref records (in order, duplicates included) diverged from the \
         hand-written walker's ground truth"
    );
}

#[test]
fn ts_spec_output_is_exact_parity() {
    let r = parse_ts(CORPUS, PATH);
    assert_eq!(
        r.parse_errors, 0,
        "clean TypeScript must report 0 parse errors"
    );
    assert!(
        r.error_ranges.is_empty(),
        "a clean parse must report no error ranges"
    );
    assert_records(
        &r,
        expected_node_records(),
        expected_ref_records(),
        "TypeScript .ts corpus",
    );
}

#[test]
fn ts_per_edge_kind_f1_is_at_parity() {
    let r = parse_ts(CORPUS, PATH);
    let obs: Vec<String> = r.refs.iter().map(ref_record).collect();
    let exp: Vec<String> = expected_ref_records()
        .into_iter()
        .map(String::from)
        .collect();

    for kind in [
        "Defines",
        "HasMethod",
        "HasField",
        "HasVariant",
        "Extends",
        "Implements",
        "Calls",
    ] {
        let prefix = format!("{kind}|");
        let of_kind = |rows: &[String]| -> std::collections::BTreeSet<String> {
            rows.iter()
                .filter(|row| row.starts_with(&prefix))
                .cloned()
                .collect()
        };
        let exp_k = of_kind(&exp);
        let obs_k = of_kind(&obs);
        assert!(
            !exp_k.is_empty(),
            "{kind}: the ground truth has no edge of this kind, so its F1 would \
             be vacuously 1.000"
        );
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
            "edge[{kind:<10}] P={precision:.3} R={recall:.3} F1={f1:.3} (tp={tp} fp={fp} fn={fn_})"
        );
        assert!(
            (f1 - 1.0).abs() < f64::EPSILON,
            "TypeScript {kind} F1 {f1:.3} is below parity 1.000"
        );
    }
}

/// The constructs the hand-written walker deliberately did NOT extract, asserted
/// as absences. An extra walker arm (a `let` binding as a `Constant`, an
/// `abstract_method_signature` as a `Method`, a `new_expression` as a call) is
/// caught here by name rather than only as an ordering shift.
#[test]
fn ts_negative_cases_stay_unextracted() {
    let r = parse_ts(CORPUS, PATH);
    for absent in ["mutable", "legacy", "area"] {
        assert!(
            !r.nodes.iter().any(|n| n.name == absent),
            "{absent} must NOT be extracted (let/var binding or abstract signature)"
        );
    }
    assert!(
        !r.nodes
            .iter()
            .any(|n| n.label == "CallSite" && n.name == "Animal"),
        "`new Animal(name)` is a new_expression, not a call_expression: no CallSite"
    );
}
