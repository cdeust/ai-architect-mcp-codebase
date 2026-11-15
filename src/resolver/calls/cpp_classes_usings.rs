// resolver::calls::cpp_classes::usings: the using-declarations and
// using-directives of the C++ sources, and when one can re-point a name the
// lookup by scope found (issue #412).

use super::*;

/// A `using ns::X;` (a declaration) or `using namespace ns;` (a directive): the
/// scope it is written in, the file that writes it, and what it introduces.
pub(super) struct Using {
    /// The path of the scope that holds it (`a::b`), empty at file scope.
    pub(super) scope: String,
    /// The file that writes it.
    pub(super) file: String,
    /// `X` of `using ns::X;`; empty for a directive.
    pub(super) name: String,
    /// `ns` of `using namespace ns;`; empty for a declaration.
    pub(super) namespace: String,
}

impl CppClasses {
    /// Reads the using-declarations and using-directives of the C++ sources. The
    /// `Import` node of a `using` has no column of its own for it: its id is
    /// `<file>::<scope>::using:<text>` (`parser::spec::cpp::using_entry`), the text
    /// being `ns::X` for a declaration and `namespace ns` for a directive.
    pub(super) fn load_usings(&mut self, store: &GraphStore) {
        let Ok(qr) = store.execute_query("MATCH (i:Import) RETURN i.id, i.language") else {
            return;
        };
        self.usings = qr
            .rows
            .iter()
            .filter(|r| r.len() >= 2 && r[1] == "cpp")
            .filter_map(|r| using_of(&r[0]))
            .collect();
    }

    /// True when a `using` the file `from` sees, written in one of the scopes
    /// `inner` (the scopes of the caller from its own out to the one that holds
    /// the class found), can make `name` mean another class: a using-declaration
    /// of that name, or a using-directive on a namespace that holds a class of
    /// that name. C++ then names what the `using` introduces; the graph holds no
    /// edge from a `using` to its target, so the lookup by scope is not used.
    pub(super) fn a_using_can_hide(&self, name: &str, from: &str, inner: &[&str]) -> bool {
        self.usings
            .iter()
            .filter(|u| inner.contains(&u.scope.as_str()))
            .filter(|u| u.name == name || self.a_namespace_can_hold(&u.namespace, name))
            .any(|u| self.sees(from, &u.file))
    }

    /// True when a class `name` of a namespace written `namespace` may exist: a
    /// directive's namespace is written relative to its scope, so every class
    /// whose path ends with `namespace::name` counts.
    fn a_namespace_can_hold(&self, namespace: &str, name: &str) -> bool {
        !namespace.is_empty()
            && self
                .by_last
                .get(name)
                .into_iter()
                .flatten()
                .any(|path| names_class(path, &qualify(namespace, name)))
    }
}

/// The `using` an `Import` node's id spells, `None` for any other import: the
/// id is `<file>::<scope>::using:<text>`, the text `ns::X` for a declaration and
/// `namespace ns` for a directive (`parser::spec::cpp::using_entry`).
pub(super) fn using_of(import_id: &str) -> Option<Using> {
    let path = path_without_file(import_id);
    let (scope, text) = match path.strip_prefix("using:") {
        Some(text) => ("", text),
        None => path.split_once("::using:")?,
    };
    let (name, namespace) = match text.trim().strip_prefix("namespace ") {
        Some(ns) => (String::new(), class_path(ns)),
        None => (last_segment(&class_path(text)).to_string(), String::new()),
    };
    (!name.is_empty() || !namespace.is_empty()).then(|| Using {
        scope: scope.to_string(),
        file: extract_file_prefix(import_id).unwrap_or_default(),
        name,
        namespace,
    })
}
