// parser::spec::typescript_parity_tests::data — the TypeScript parity
// corpus and the walker's ground truth (full 7-tuple node records + ref
// triples), split from the test harness (mod.rs) to keep both files under the
// §4.1 500-line cap. The records began as the hand-written walker's captured
// output (ADR-0055 phase 9, §5 step 1) and were then updated for the three
// deliberate defect fixes #140/#141/#142 (deltas marked inline). See the parent
// module doc.

pub(super) const CORPUS: &str = r#"import { Router, Request } from 'express';
import { foo, bar as baz } from './module';
import * as utils from '../utils';
import defaultExport from 'package';
import type { Config } from './types';
import './side-effect';

export const MAX_RETRIES = 3;
export const TIMEOUT: number = 5000;
const localConst = 42;
let mutableVar = 10;

export function greet(name: string): string {
    return format(name);
}

export async function fetchData(url: string): Promise<Response> {
    return fetch(url);
}

export function* gen(): Generator<number> {
    yield 1;
}

export const handler = (req: Request) => {
    greet("world");
};

export const asyncArrow = async () => {
    await fetchData("x");
};

function internalFn(): void {
    helper();
}

export abstract class Base {
    abstract compute(): number;
}

export class Animal extends Base implements Serializable {
    public name: string;
    private age: number;
    protected kind: string;
    readonly id: number;

    constructor(name: string) {
        super();
        this.name = name;
    }

    compute(): number {
        return this.age;
    }

    get displayName(): string {
        return this.name;
    }

    set displayName(v: string) {
        this.name = v;
    }

    async speak(): Promise<string> {
        return await this.fetch();
    }

    private fetch(): Promise<string> {
        return Promise.resolve("");
    }
}

export class Dog extends Animal {
    speak(): Promise<string> {
        return Promise.resolve("Woof");
    }
}

export class Container<T> extends Wrapper<T> {
    value: T;
}

export interface Serializable extends Base {
    serialize(): string;
    readonly id: number;
    count: number;
}

export enum Color {
    Red = "RED",
    Green = "GREEN",
    Blue = "BLUE",
}

export enum Direction {
    Up,
    Down,
}

export type StringOrNumber = string | number;
type LocalAlias = Map<string, number>;

@Component({ selector: 'app' })
export class Widget {
    label: string;
    render(): void {
        draw();
    }
}

export const api = {
    get(url: string) { return fetch(url); },
    post: (url: string) => send(url),
};

export default function main(): void {
    greet("main");
}

export { foo, baz };
export * from './reexport';
"#;

pub(super) const PATH: &str = "app/mod.ts";

pub(super) fn expected_node_records() -> Vec<&'static str> {
    vec![
        "CallSite|Promise.resolve|app/mod.ts::Animal::fetch::call@69:15#1371-1390|69|69||[(\"callee_name\", \"Promise.resolve\"), (\"caller_qn\", \"app/mod.ts::Animal::fetch\")]",
        "CallSite|Promise.resolve|app/mod.ts::Dog::speak::call@75:15#1481-1504|75|75||[(\"callee_name\", \"Promise.resolve\"), (\"caller_qn\", \"app/mod.ts::Dog::speak\")]",
        "CallSite|draw|app/mod.ts::Widget::render::call@107:8#2009-2015|107|107||[(\"callee_name\", \"draw\"), (\"caller_qn\", \"app/mod.ts::Widget::render\")]",
        // #142: object-literal `api` method + arrow-property bodies scanned for calls.
        "CallSite|fetch|app/mod.ts::api::call@112:30#2077-2087|112|112||[(\"callee_name\", \"fetch\"), (\"caller_qn\", \"app/mod.ts::api\")]",
        "CallSite|send|app/mod.ts::api::call@113:27#2119-2128|113|113||[(\"callee_name\", \"send\"), (\"caller_qn\", \"app/mod.ts::api\")]",
        "CallSite|fetchData|app/mod.ts::asyncArrow::call@30:10#678-692|30|30||[(\"callee_name\", \"fetchData\"), (\"caller_qn\", \"app/mod.ts::asyncArrow\")]",
        "CallSite|fetch|app/mod.ts::fetchData::call@18:11#486-496|18|18||[(\"callee_name\", \"fetch\"), (\"caller_qn\", \"app/mod.ts::fetchData\")]",
        "CallSite|format|app/mod.ts::greet::call@14:11#392-404|14|14||[(\"callee_name\", \"format\"), (\"caller_qn\", \"app/mod.ts::greet\")]",
        "CallSite|greet|app/mod.ts::handler::call@26:4#608-622|26|26||[(\"callee_name\", \"greet\"), (\"caller_qn\", \"app/mod.ts::handler\")]",
        "CallSite|greet|app/mod.ts::main::call@117:4#2177-2190|117|117||[(\"callee_name\", \"greet\"), (\"caller_qn\", \"app/mod.ts::main\")]",
        "CallSite|helper|app/mod.ts::internalFn::call@34:4#732-740|34|34||[(\"callee_name\", \"helper\"), (\"caller_qn\", \"app/mod.ts::internalFn\")]",
        "CallSite|super|app/mod.ts::Animal::constructor::call@48:8#1012-1019|48|48||[(\"callee_name\", \"super\"), (\"caller_qn\", \"app/mod.ts::Animal::constructor\")]",
        "CallSite|this.fetch|app/mod.ts::Animal::speak::call@65:21#1296-1308|65|65||[(\"callee_name\", \"this.fetch\"), (\"caller_qn\", \"app/mod.ts::Animal::speak\")]",
        "Constant|MAX_RETRIES|app/mod.ts::MAX_RETRIES|8|8|pub|[(\"type_annotation\", \"\")]",
        "Constant|TIMEOUT|app/mod.ts::TIMEOUT|9|9|pub|[(\"type_annotation\", \"number\")]",
        "Constant|api|app/mod.ts::api|111|114|pub|[(\"type_annotation\", \"\")]",
        "Constant|localConst|app/mod.ts::localConst|10|10||[(\"type_annotation\", \"\")]",
        "Enum|Color|app/mod.ts::Color|89|93|pub|[]",
        "Enum|Direction|app/mod.ts::Direction|95|98|pub|[]",
        "Field|age|app/mod.ts::Animal::age|43|43|private|[(\"type_annotation\", \"number\")]",
        "Field|count|app/mod.ts::Serializable::count|86|86||[(\"type_annotation\", \"number\")]",
        "Field|id|app/mod.ts::Animal::id|45|45||[(\"type_annotation\", \"number\")]",
        "Field|id|app/mod.ts::Serializable::id|85|85||[(\"type_annotation\", \"number\")]",
        "Field|kind|app/mod.ts::Animal::kind|44|44|protected|[(\"type_annotation\", \"string\")]",
        "Field|label|app/mod.ts::Widget::label|105|105||[(\"type_annotation\", \"string\")]",
        "Field|name|app/mod.ts::Animal::name|42|42|public|[(\"type_annotation\", \"string\")]",
        "Field|value|app/mod.ts::Container::value|80|80||[(\"type_annotation\", \"T\")]",
        "Function|asyncArrow|app/mod.ts::asyncArrow|29|31|pub|[(\"is_async\", \"true\")]",
        "Function|fetchData|app/mod.ts::fetchData|17|19|pub|[(\"is_async\", \"true\"), (\"return_type\", \"Promise<Response>\")]",
        "Function|gen|app/mod.ts::gen|21|23|pub|[(\"is_async\", \"false\"), (\"return_type\", \"Generator<number>\")]",
        "Function|greet|app/mod.ts::greet|13|15|pub|[(\"is_async\", \"false\"), (\"return_type\", \"string\")]",
        "Function|handler|app/mod.ts::handler|25|27|pub|[(\"is_async\", \"false\")]",
        "Function|internalFn|app/mod.ts::internalFn|33|35||[(\"is_async\", \"false\"), (\"return_type\", \"void\")]",
        "Function|main|app/mod.ts::main|116|118|pub|[(\"is_async\", \"false\"), (\"return_type\", \"void\")]",
        "Import|.::module::foo|app/mod.ts::.::module::foo|2|2||[(\"path\", \".::module::foo\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|.::side-effect|app/mod.ts::.::side-effect|6|6||[(\"path\", \".::side-effect\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|.::types::Config|app/mod.ts::.::types::Config|5|5||[(\"path\", \".::types::Config\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|baz|app/mod.ts::baz|2|2||[(\"path\", \".::module::bar\"), (\"alias\", \"baz\"), (\"is_glob\", \"false\")]",
        "Import|defaultExport|app/mod.ts::defaultExport|4|4||[(\"path\", \"package::default\"), (\"alias\", \"defaultExport\"), (\"is_glob\", \"false\")]",
        "Import|express::Request|app/mod.ts::express::Request|1|1||[(\"path\", \"express::Request\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|express::Router|app/mod.ts::express::Router|1|1||[(\"path\", \"express::Router\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|utils|app/mod.ts::utils|3|3||[(\"path\", \"..::utils\"), (\"alias\", \"utils\"), (\"is_glob\", \"true\")]",
        "Method|compute|app/mod.ts::Animal::compute|52|54||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/mod.ts::Animal\"), (\"return_type\", \"number\")]",
        // #141: the `abstract compute(): number;` requirement in `Base`'s body.
        "Method|compute|app/mod.ts::Base::compute|38|38||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/mod.ts::Base\")]",
        "Method|constructor|app/mod.ts::Animal::constructor|47|50||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/mod.ts::Animal\")]",
        "Method|displayName|app/mod.ts::Animal::displayName|56|58||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/mod.ts::Animal\"), (\"return_type\", \"string\")]",
        "Method|displayName|app/mod.ts::Animal::displayName|60|62||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/mod.ts::Animal\")]",
        "Method|fetch|app/mod.ts::Animal::fetch|68|70|private|[(\"is_async\", \"false\"), (\"receiver_type\", \"app/mod.ts::Animal\"), (\"return_type\", \"Promise<string>\")]",
        "Method|render|app/mod.ts::Widget::render|106|108||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/mod.ts::Widget\"), (\"return_type\", \"void\")]",
        "Method|serialize|app/mod.ts::Serializable::serialize|84|84||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/mod.ts::Serializable\")]",
        "Method|speak|app/mod.ts::Animal::speak|64|66||[(\"is_async\", \"true\"), (\"receiver_type\", \"app/mod.ts::Animal\"), (\"return_type\", \"Promise<string>\")]",
        "Method|speak|app/mod.ts::Dog::speak|74|76||[(\"is_async\", \"false\"), (\"receiver_type\", \"app/mod.ts::Dog\"), (\"return_type\", \"Promise<string>\")]",
        "Struct|Animal|app/mod.ts::Animal|41|71|pub|[]",
        "Struct|Base|app/mod.ts::Base|37|39|pub|[]",
        "Struct|Container|app/mod.ts::Container|79|81|pub|[]",
        "Struct|Dog|app/mod.ts::Dog|73|77|pub|[]",
        "Struct|Widget|app/mod.ts::Widget|104|109|pub|[]",
        "Trait|Serializable|app/mod.ts::Serializable|83|87|pub|[]",
        "TypeAlias|LocalAlias|app/mod.ts::LocalAlias|101|101||[(\"target_type\", \"Map<string, number>\")]",
        "TypeAlias|StringOrNumber|app/mod.ts::StringOrNumber|100|100|pub|[(\"target_type\", \"string | number\")]",
        "Variant|Blue|app/mod.ts::Color::Blue|92|92||[]",
        "Variant|Down|app/mod.ts::Direction::Down|97|97||[]",
        "Variant|Green|app/mod.ts::Color::Green|91|91||[]",
        "Variant|Red|app/mod.ts::Color::Red|90|90||[]",
        "Variant|Up|app/mod.ts::Direction::Up|96|96||[]",
    ]
}

/// The full 78-triple ref MULTISET (with the two identical
/// `HasMethod(→…::Animal::displayName)` edges for the getter/setter pair).
pub(super) fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Calls", "app/mod.ts::Animal::constructor", "super"),
        ("Calls", "app/mod.ts::Animal::fetch", "resolve"),
        ("Calls", "app/mod.ts::Animal::speak", "fetch"),
        ("Calls", "app/mod.ts::Dog::speak", "resolve"),
        ("Calls", "app/mod.ts::Widget::render", "draw"),
        // #142: calls found inside the `api` object literal's member bodies.
        ("Calls", "app/mod.ts::api", "fetch"),
        ("Calls", "app/mod.ts::api", "send"),
        ("Calls", "app/mod.ts::asyncArrow", "fetchData"),
        ("Calls", "app/mod.ts::fetchData", "fetch"),
        ("Calls", "app/mod.ts::greet", "format"),
        ("Calls", "app/mod.ts::handler", "greet"),
        ("Calls", "app/mod.ts::internalFn", "helper"),
        ("Calls", "app/mod.ts::main", "greet"),
        (
            "Defines",
            "app/mod.ts::Animal::constructor",
            "app/mod.ts::Animal::constructor::call@48:8#1012-1019",
        ),
        (
            "Defines",
            "app/mod.ts::Animal::fetch",
            "app/mod.ts::Animal::fetch::call@69:15#1371-1390",
        ),
        (
            "Defines",
            "app/mod.ts::Animal::speak",
            "app/mod.ts::Animal::speak::call@65:21#1296-1308",
        ),
        (
            "Defines",
            "app/mod.ts::Dog::speak",
            "app/mod.ts::Dog::speak::call@75:15#1481-1504",
        ),
        (
            "Defines",
            "app/mod.ts::Widget::render",
            "app/mod.ts::Widget::render::call@107:8#2009-2015",
        ),
        // #142: the `Defines` companions of the two `api` call sites.
        (
            "Defines",
            "app/mod.ts::api",
            "app/mod.ts::api::call@112:30#2077-2087",
        ),
        (
            "Defines",
            "app/mod.ts::api",
            "app/mod.ts::api::call@113:27#2119-2128",
        ),
        (
            "Defines",
            "app/mod.ts::asyncArrow",
            "app/mod.ts::asyncArrow::call@30:10#678-692",
        ),
        (
            "Defines",
            "app/mod.ts::fetchData",
            "app/mod.ts::fetchData::call@18:11#486-496",
        ),
        (
            "Defines",
            "app/mod.ts::greet",
            "app/mod.ts::greet::call@14:11#392-404",
        ),
        (
            "Defines",
            "app/mod.ts::handler",
            "app/mod.ts::handler::call@26:4#608-622",
        ),
        (
            "Defines",
            "app/mod.ts::internalFn",
            "app/mod.ts::internalFn::call@34:4#732-740",
        ),
        (
            "Defines",
            "app/mod.ts::main",
            "app/mod.ts::main::call@117:4#2177-2190",
        ),
        ("Defines", "app/mod.ts", "app/mod.ts::.::module::foo"),
        ("Defines", "app/mod.ts", "app/mod.ts::.::side-effect"),
        ("Defines", "app/mod.ts", "app/mod.ts::.::types::Config"),
        ("Defines", "app/mod.ts", "app/mod.ts::Animal"),
        ("Defines", "app/mod.ts", "app/mod.ts::Base"),
        ("Defines", "app/mod.ts", "app/mod.ts::Color"),
        ("Defines", "app/mod.ts", "app/mod.ts::Container"),
        ("Defines", "app/mod.ts", "app/mod.ts::Direction"),
        ("Defines", "app/mod.ts", "app/mod.ts::Dog"),
        ("Defines", "app/mod.ts", "app/mod.ts::LocalAlias"),
        ("Defines", "app/mod.ts", "app/mod.ts::MAX_RETRIES"),
        ("Defines", "app/mod.ts", "app/mod.ts::Serializable"),
        ("Defines", "app/mod.ts", "app/mod.ts::StringOrNumber"),
        ("Defines", "app/mod.ts", "app/mod.ts::TIMEOUT"),
        ("Defines", "app/mod.ts", "app/mod.ts::Widget"),
        ("Defines", "app/mod.ts", "app/mod.ts::api"),
        ("Defines", "app/mod.ts", "app/mod.ts::asyncArrow"),
        ("Defines", "app/mod.ts", "app/mod.ts::baz"),
        ("Defines", "app/mod.ts", "app/mod.ts::defaultExport"),
        ("Defines", "app/mod.ts", "app/mod.ts::express::Request"),
        ("Defines", "app/mod.ts", "app/mod.ts::express::Router"),
        ("Defines", "app/mod.ts", "app/mod.ts::fetchData"),
        ("Defines", "app/mod.ts", "app/mod.ts::gen"),
        ("Defines", "app/mod.ts", "app/mod.ts::greet"),
        ("Defines", "app/mod.ts", "app/mod.ts::handler"),
        ("Defines", "app/mod.ts", "app/mod.ts::internalFn"),
        ("Defines", "app/mod.ts", "app/mod.ts::localConst"),
        ("Defines", "app/mod.ts", "app/mod.ts::main"),
        ("Defines", "app/mod.ts", "app/mod.ts::utils"),
        ("Extends", "app/mod.ts::Animal", "Base"),
        ("Extends", "app/mod.ts::Container", "Wrapper"),
        ("Extends", "app/mod.ts::Dog", "Animal"),
        ("Extends", "app/mod.ts::Serializable", "Base"),
        ("HasField", "app/mod.ts::Animal", "app/mod.ts::Animal::age"),
        ("HasField", "app/mod.ts::Animal", "app/mod.ts::Animal::id"),
        ("HasField", "app/mod.ts::Animal", "app/mod.ts::Animal::kind"),
        ("HasField", "app/mod.ts::Animal", "app/mod.ts::Animal::name"),
        (
            "HasField",
            "app/mod.ts::Container",
            "app/mod.ts::Container::value",
        ),
        (
            "HasField",
            "app/mod.ts::Serializable",
            "app/mod.ts::Serializable::count",
        ),
        (
            "HasField",
            "app/mod.ts::Serializable",
            "app/mod.ts::Serializable::id",
        ),
        (
            "HasField",
            "app/mod.ts::Widget",
            "app/mod.ts::Widget::label",
        ),
        (
            "HasMethod",
            "app/mod.ts::Animal",
            "app/mod.ts::Animal::compute",
        ),
        // #141: `Base` now owns its abstract `compute` requirement.
        ("HasMethod", "app/mod.ts::Base", "app/mod.ts::Base::compute"),
        (
            "HasMethod",
            "app/mod.ts::Animal",
            "app/mod.ts::Animal::constructor",
        ),
        (
            "HasMethod",
            "app/mod.ts::Animal",
            "app/mod.ts::Animal::displayName",
        ),
        (
            "HasMethod",
            "app/mod.ts::Animal",
            "app/mod.ts::Animal::displayName",
        ),
        (
            "HasMethod",
            "app/mod.ts::Animal",
            "app/mod.ts::Animal::fetch",
        ),
        (
            "HasMethod",
            "app/mod.ts::Animal",
            "app/mod.ts::Animal::speak",
        ),
        ("HasMethod", "app/mod.ts::Dog", "app/mod.ts::Dog::speak"),
        (
            "HasMethod",
            "app/mod.ts::Serializable",
            "app/mod.ts::Serializable::serialize",
        ),
        (
            "HasMethod",
            "app/mod.ts::Widget",
            "app/mod.ts::Widget::render",
        ),
        ("HasVariant", "app/mod.ts::Color", "app/mod.ts::Color::Blue"),
        (
            "HasVariant",
            "app/mod.ts::Color",
            "app/mod.ts::Color::Green",
        ),
        ("HasVariant", "app/mod.ts::Color", "app/mod.ts::Color::Red"),
        (
            "HasVariant",
            "app/mod.ts::Direction",
            "app/mod.ts::Direction::Down",
        ),
        (
            "HasVariant",
            "app/mod.ts::Direction",
            "app/mod.ts::Direction::Up",
        ),
        ("Implements", "app/mod.ts::Animal", "Serializable"),
    ]
}
