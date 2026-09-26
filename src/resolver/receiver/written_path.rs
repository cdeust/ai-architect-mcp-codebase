// resolver::receiver::written_path: issue #368. A receiver typed by a path
// written with more than one segment (`let s = b::Set::new();`,
// `s: &crate::a::Set`) used to reach the resolver as its last segment only
// (`Set`), so the lookup matched every `Set` of the repository and the same-file
// tiebreak picked the caller file's own `a::Set` for a binding that names
// `b::Set`. The parser now keeps the path as written, and this module decides
// which owner the path names:
//
// - `crate::R`: an owner whose module path, from the root of the caller's
//   crate, is exactly `R`.
// - `self::R`: `R`, read like a relative path.
// - `super::R` (one or more): `R` from the parent of the caller's inline
//   module; a caller that sits at the root of its file declines, because the
//   parent of a file module is not read here.
// - `<lib>::R` where `<lib>` is a library crate of the repository: an owner of
//   that library whose module path from the library root is exactly `R`.
// - any other path: an owner of the caller's crate whose module path ends with
//   the written segments.
//
// Issues #373 and #380 add two things. A one-segment type that a `use` of the
// caller's module binds is read as the path of that `use`, from that module
// (`WrittenPath::imported`). And an exact path (`crate::`, `super::`, `<lib>::`,
// or one read from a `use`) follows the `use` declarations of the module it
// ends in (`reexport`): `crate::Set` with `pub use task::Set;` at the root names
// `task::Set`, and with `pub use ext::Set;` it names a path no module of the
// repository has, so the call declines.
//
// A module path is the one the file's location gives (`src/a.rs` is `a`,
// `src/a/mod.rs` is `a`, `src/lib.rs` and every target entry file are the
// root), followed by the inline modules and the type. A `#[path]` layout is not
// followed: the call then does not resolve (a lost edge, not a wrong one). When
// several owners match, the call is ambiguous: no same-file preference applies
// to a written path.
// source: The Rust Reference, "Paths" (`crate`, `self`, `super`), "Modules"
// (a module's file follows its path) and "Use declarations"; Cargo Book,
// "Cargo Targets".

use super::imports::{caller_scope, scope_module_path, ModuleImports};
use super::reexport;
use super::*;
use crate::graph_store::import_roots::CrateEvidence;

/// The read-only facts a path is read with.
pub(in crate::resolver) struct PathFacts<'e> {
    pub(in crate::resolver) idx: &'e SymbolIndex,
    pub(in crate::resolver) evidence: &'e CrateEvidence,
    pub(in crate::resolver) imports: &'e ModuleImports,
}

/// The owners a written path admits.
pub(in crate::resolver) struct WrittenPath<'e> {
    evidence: &'e CrateEvidence,
    caller_file: String,
    /// The paths the written one leads to once `use` declarations are
    /// followed; an owner any of them names is admitted.
    anchors: Vec<Anchor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Anchor {
    /// The owner's module path is exactly these segments, from the root of the
    /// caller's crate.
    CrateRoot(Vec<String>),
    /// The owner's module path is exactly these segments, from the root of the
    /// library `lib`.
    Library { lib: String, segments: Vec<String> },
    /// The owner's module path ends with these segments.
    Suffix(Vec<String>),
}

impl Anchor {
    /// The module path and the name of an exact anchor; `None` for a suffix
    /// or an empty path.
    pub(super) fn exact_split(&self) -> Option<(&[String], &String)> {
        match self {
            Anchor::CrateRoot(s) | Anchor::Library { segments: s, .. } => {
                let (name, module) = s.split_last()?;
                Some((module, name))
            }
            Anchor::Suffix(_) => None,
        }
    }

    /// The library an exact anchor is read in; `None` for the caller's crate.
    pub(super) fn library(&self) -> Option<&str> {
        match self {
            Anchor::Library { lib, .. } => Some(lib),
            _ => None,
        }
    }

    fn written(&self) -> &[String] {
        match self {
            Anchor::CrateRoot(s) | Anchor::Suffix(s) => s,
            Anchor::Library { segments, .. } => segments,
        }
    }
}

impl<'e> WrittenPath<'e> {
    /// The admission rule of `hint`, or `None` when the path cannot be read
    /// (a `super` from the root of a file, a path reduced to nothing), which
    /// declines the call. `hint` has at least two segments.
    pub(in crate::resolver) fn of(
        facts: &PathFacts<'e>,
        caller_qn: &str,
        hint: &str,
    ) -> Option<WrittenPath<'e>> {
        let evidence = facts.evidence;
        let segments = path_segments(hint);
        let anchor = match segments.first().map(String::as_str)? {
            "crate" => Anchor::CrateRoot(segments[1..].to_vec()),
            "self" => Anchor::Suffix(segments[1..].to_vec()),
            "super" => Anchor::CrateRoot(above_caller(facts.idx, evidence, caller_qn, &segments)?),
            first if evidence.crate_names.contains(first) => Anchor::Library {
                lib: first.to_string(),
                segments: segments[1..].to_vec(),
            },
            _ => Anchor::Suffix(segments),
        };
        Self::following(facts, caller_qn, anchor)
    }

    /// The admission rule of the path a `use` of the caller's module writes
    /// (`use b::Set;` gives `b::Set`), read from that module: a relative path
    /// names a child of the module, not any module that ends with it.
    pub(in crate::resolver) fn imported(
        facts: &PathFacts<'e>,
        caller_qn: &str,
        path: &str,
    ) -> Option<WrittenPath<'e>> {
        let module = scope_module_path(facts.evidence, &caller_scope(facts.idx, caller_qn));
        let anchor = reexport::anchor_in(facts.evidence, None, &module, &path_segments(path))?;
        Self::following(facts, caller_qn, anchor)
    }

    fn following(
        facts: &PathFacts<'e>,
        caller_qn: &str,
        anchor: Anchor,
    ) -> Option<WrittenPath<'e>> {
        if anchor.written().is_empty() {
            return None;
        }
        let caller_file = extract_file_prefix_or_self(caller_qn);
        let anchors = reexport::follow(facts, &caller_file, anchor);
        Some(WrittenPath {
            evidence: facts.evidence,
            caller_file,
            anchors,
        })
    }

    /// True when `candidate` (a method) belongs to an owner the path names.
    pub(in crate::resolver) fn admits(&self, candidate: &SymbolEntry) -> bool {
        let Some((owner, _)) = candidate.qualified_name.rsplit_once("::") else {
            return false;
        };
        let owner_file = extract_file_prefix_or_self(owner);
        let mut path = file_module_path(self.evidence, &owner_file);
        path.extend(path_segments(owner.strip_prefix(&owner_file).unwrap_or("")));
        let same_crate = || same_crate(self.evidence, &self.caller_file, &owner_file);
        self.anchors.iter().any(|anchor| match anchor {
            Anchor::CrateRoot(written) => same_crate() && path.as_slice() == written.as_slice(),
            Anchor::Suffix(written) => same_crate() && path.ends_with(written),
            Anchor::Library { lib, segments } => {
                library_owns(self.evidence, lib, &owner_file)
                    && path.as_slice() == segments.as_slice()
            }
        })
    }
}

/// True when `file` belongs to the crate of `caller_file`, or when the Cargo
/// facts do not say (no `Cargo.toml`, `cargo` missing, a file no target
/// reaches).
pub(super) fn same_crate(evidence: &CrateEvidence, caller_file: &str, file: &str) -> bool {
    if !evidence.known {
        return true;
    }
    match (evidence.owners.get(caller_file), evidence.owners.get(file)) {
        (Some(a), Some(b)) => a.intersection(b).next().is_some(),
        _ => true,
    }
}

/// The module path `super::..::R` names, from the root of the caller's crate:
/// each `super` leaves one inline module around the caller. `None` when the
/// `super`s climb past the caller's file.
fn above_caller(
    idx: &SymbolIndex,
    evidence: &CrateEvidence,
    caller_qn: &str,
    segments: &[String],
) -> Option<Vec<String>> {
    let ups = segments.iter().take_while(|s| *s == "super").count();
    let inline = inline_modules(idx, caller_qn);
    let kept = inline.len().checked_sub(ups)?;
    let mut path = file_module_path(evidence, &extract_file_prefix_or_self(caller_qn));
    path.extend(inline[..kept].iter().cloned());
    path.extend(segments[ups..].iter().cloned());
    Some(path)
}

/// True when a target of the library named `lib` reaches `file`.
pub(super) fn library_owns(evidence: &CrateEvidence, lib: &str, file: &str) -> bool {
    evidence.owners.get(file).is_some_and(|owners| {
        owners
            .iter()
            .any(|o| evidence.targets.get(o).and_then(Option::as_deref) == Some(lib))
    })
}

/// The segments of a path, generic arguments removed from each (`Gen<T>` is
/// `Gen`); leading and trailing `::` are dropped.
pub(super) fn path_segments(path: &str) -> Vec<String> {
    let mut depth = 0usize;
    let mut plain = String::with_capacity(path.len());
    for ch in path.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 && !ch.is_whitespace() => plain.push(ch),
            _ => {}
        }
    }
    plain
        .split("::")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The module path a file's location gives: the root for a target entry file
/// and for `lib.rs` or `main.rs`; otherwise the path below `src/` (or below
/// `tests/`, `benches/`, `examples/`) without `.rs`, and without a final `mod`.
pub(super) fn file_module_path(evidence: &CrateEvidence, file: &str) -> Vec<String> {
    let name = file.rsplit('/').next().unwrap_or(file);
    if evidence.targets.contains_key(file) || name == "lib.rs" || name == "main.rs" {
        return Vec::new();
    }
    let below = match file.rfind("src/") {
        Some(i) if i == 0 || file[..i].ends_with('/') => &file[i + 4..],
        _ => ["tests/", "benches/", "examples/"]
            .iter()
            .find_map(|dir| file.strip_prefix(dir))
            .unwrap_or(file),
    };
    let mut segments: Vec<String> = below
        .trim_end_matches(".rs")
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if segments.last().map(String::as_str) == Some("mod") {
        segments.pop();
    }
    segments
}

/// The inline modules around the caller, outermost first: the leading
/// segments of its qualified name, after the file, that name `Module` nodes.
pub(super) fn inline_modules(idx: &SymbolIndex, caller_qn: &str) -> Vec<String> {
    let file = extract_file_prefix_or_self(caller_qn);
    let segments: Vec<&str> = caller_qn
        .strip_prefix(&file)
        .unwrap_or("")
        .split("::")
        .filter(|s| !s.is_empty())
        .collect();
    let mut modules = Vec::new();
    let mut qn = file.clone();
    for segment in segments.iter().take(segments.len().saturating_sub(1)) {
        qn = format!("{qn}::{segment}");
        match idx.by_qn.get(&qn) {
            Some(entry) if entry.label == "Module" => modules.push(segment.to_string()),
            _ => break,
        }
    }
    modules
}

#[cfg(test)]
#[path = "written_path_tests.rs"]
mod tests;
