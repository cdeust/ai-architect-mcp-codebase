// resolver::receiver::qualified: issue #398. A call written with a path
// (`a::dup()`, `crate::a::dup()`, `Set::new()`) reached the lookup by name as
// its last segment, with the whole spelling as import evidence. That evidence
// is matched against qualified names built from file paths (`src/a.rs::dup`),
// which never end with a module path (`a::dup`), so a path that names exactly
// one function left it ambiguous; and a type name brought in by nothing but a
// glob of another crate (`use ext::*; Set::new()`) matched the repository's
// own `Set::new`, a namesake.
//
// The qualifier (everything before the last `::`) is read with the rules the
// receiver types already use (`written_path`, `binding`):
//
// - `crate::`, `self::`, `super::` or a library of the repository: the owner
//   whose module path is exactly the one written, `use` declarations
//   followed; no owner declines the call.
// - one other segment: what the caller's module binds that name to
//   (`binding::bind`: a type of the module, a `use`, a glob); when nothing
//   binds it, a child module or type of the caller's module of that name.
// - several other segments: a path from the caller's module.
//
// The last two keep the lookup by name when no owner matches (a module the
// index places elsewhere, a crate reached some other way), so they add no
// declined call of their own.
// source: The Rust Reference, "Paths" (a path is read from the current module,
// `crate`, `self`, `super`), "Use declarations".

use super::binding::{bind, Binding, Defines};
use super::imports::{caller_scope, scope_module_path};
use super::reexport::anchor_in;
use super::written_path::{path_segments, Anchor, PathFacts, WrittenPath};
use super::*;

/// How the qualifier of a path call is read.
pub(in crate::resolver) enum QualifierRule<'e> {
    /// No rule applies: the lookup by name.
    ByName,
    /// Only the owners `path` admits. `fallback`: when none does, the lookup by
    /// name applies.
    Path {
        path: WrittenPath<'e>,
        fallback: bool,
    },
    /// The path names nothing of the repository: the call declines.
    Decline,
}

/// The rule for `qualifier`, written in a call of `caller_qn`. `admits_any`
/// says whether a path admits one of the call's candidates.
pub(in crate::resolver) fn qualifier_rule<'e>(
    facts: &PathFacts<'e>,
    caller_qn: &str,
    qualifier: &str,
    admits_any: &dyn Fn(&str, &WrittenPath<'e>) -> bool,
) -> QualifierRule<'e> {
    let segments = path_segments(qualifier);
    let Some(first) = segments.first().map(String::as_str) else {
        return QualifierRule::ByName;
    };
    let caller_file = extract_file_prefix_or_self(caller_qn);
    let module = scope_module_path(facts.evidence, &caller_scope(facts.idx, caller_qn));
    let anchored =
        ["crate", "self", "super"].contains(&first) || facts.evidence.crate_names.contains(first);
    if anchored {
        return match anchor_in(facts.evidence, None, &module, &segments) {
            Some(anchor) => QualifierRule::Path {
                path: owner_path(facts, &caller_file, anchor),
                fallback: false,
            },
            None => QualifierRule::Decline,
        };
    }
    if let [name] = segments.as_slice() {
        match bind(facts, caller_qn, name, admits_any, Defines::TypesAndModules) {
            Binding::Decline => return QualifierRule::Decline,
            Binding::Path { path, fallback, .. } => return QualifierRule::Path { path, fallback },
            Binding::ByName => {}
        }
    }
    let relative = Anchor::CrateRoot([module.as_slice(), segments.as_slice()].concat());
    QualifierRule::Path {
        path: owner_path(facts, &caller_file, relative),
        fallback: true,
    }
}

/// The owners `anchor` admits. The root of a crate (`crate::f`, a library's
/// `lib::f`) is a path with no segment: it names its module as written, with no
/// `use` to follow.
fn owner_path<'e>(facts: &PathFacts<'e>, caller_file: &str, anchor: Anchor) -> WrittenPath<'e> {
    if anchor.written().is_empty() {
        return WrittenPath::exact(facts, caller_file, anchor);
    }
    let exact = anchor.clone();
    WrittenPath::following(facts, caller_file, anchor)
        .unwrap_or_else(|| WrittenPath::exact(facts, caller_file, exact))
}
