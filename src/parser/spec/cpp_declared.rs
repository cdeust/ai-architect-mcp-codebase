// parser::spec::cpp_declared — which declaration of a name a C++ call sees (issue #412).
//
// `declared_type` walks the scopes around a call, innermost first, and answers with the
// first scope that declares the receiver's name: the type that declaration writes, or
// none when it declares the name without a type the source writes (`auto [e, c] = p`,
// `[e = f()]`, `using ns::e;`, a declaration under `#if`). Such a name still hides every
// outer declaration of the same name: a reader that skips a declaring form lets the walk
// reach an outer one (a catch parameter, a parameter) the call does not refer to.
//
// source: tree-sitter-cpp 0.23.4 node-types.json; each reader below names the nodes
// it reads, and `Reader::bindings_of` is the list of scopes.

use tree_sitter::Node;

use crate::parser::generic_args::strip_generic_groups;
use crate::parser::node_text;

use super::cpp_unreadable::scope_has_unreadable_statement;

/// The type the innermost enclosing scope declares for `name`, read at `at`.
/// The search stops at the enclosing function: a global or a field is not read.
/// A name declared without a written type gives none, and hides the outer ones.
/// The type of a catch parameter, which `main` never read, is used only when no
/// statement between the handler and the call could declare the name in a form this
/// reader cannot name (`scope_has_unreadable_statement`).
pub(super) fn declared_type(source: &str, at: Node, name: &str) -> Option<String> {
    let mut between = Vec::new();
    let mut scope = at.parent();
    while let Some(s) = scope {
        if let Some(found) = binding_in(source, s, at, name) {
            let untrusted = s.kind() == "catch_clause"
                && between
                    .iter()
                    .any(|b| scope_has_unreadable_statement(source, (*b, at), name));
            return found.filter(|_| !untrusted);
        }
        between.push(s);
        if s.kind() == "function_definition" {
            return None;
        }
        scope = s.parent();
    }
    None
}

/// One declaration of the name looked for: `decl` is the node whose `type` field
/// types it (none: the source writes no type for the name) and `from` the byte
/// from which the name is visible.
struct Binding<'t> {
    decl: Option<Node<'t>>,
    from: usize,
}

/// `Some(type)` when `scope` declares `name` before `call` (the last such
/// declaration wins); the inner `None` is a declaration whose type the source
/// does not write. `None` when `scope` does not declare the name.
fn binding_in(source: &str, scope: Node, call: Node, name: &str) -> Option<Option<String>> {
    let reader = Reader { source, name };
    let mut found = None;
    for binding in reader.bindings_of(scope) {
        if binding.from < call.start_byte() {
            found = Some(binding.decl.and_then(|d| written_type(source, d)));
        }
    }
    found
}

/// Reads the declarations of one name.
struct Reader<'a> {
    source: &'a str,
    name: &'a str,
}

impl Reader<'_> {
    /// The declarations of the name that `scope` makes, by kind of scope: the
    /// parameters of a function, a lambda (also its template parameters and its
    /// init-captures), a `requires` expression or a handler; the loop variable and
    /// init-statement of a range-for; the condition of an `if`, `switch` or
    /// `while`; the children of anything else (a block, a `for`, a label).
    fn bindings_of<'t>(&self, scope: Node<'t>) -> Vec<Binding<'t>> {
        match scope.kind() {
            "function_definition" => self.parameters(parameters_of(scope)),
            "lambda_expression" => {
                let mut out = self.captures(scope.child_by_field_name("captures"));
                out.extend(self.parameters(parameters_of(scope)));
                out.extend(self.parameters(scope.child_by_field_name("template_parameters")));
                out
            }
            "requires_expression" | "catch_clause" => {
                self.parameters(scope.child_by_field_name("parameters"))
            }
            "for_range_loop" => {
                let mut out = self.init_statement(scope.child_by_field_name("initializer"));
                out.extend(self.declaration(scope, true));
                out
            }
            "if_statement" | "while_statement" | "switch_statement" => {
                self.condition(scope.child_by_field_name("condition"))
            }
            _ => self.block(scope, false),
        }
    }

    /// `parameter_list` / `template_parameter_list` children that name a value:
    /// parameter_declaration, optional_parameter_declaration,
    /// variadic_parameter_declaration.
    fn parameters<'t>(&self, list: Option<Node<'t>>) -> Vec<Binding<'t>> {
        let Some(list) = list else {
            return Vec::new();
        };
        let mut cursor = list.walk();
        list.named_children(&mut cursor)
            .filter(|p| {
                matches!(
                    p.kind(),
                    "parameter_declaration"
                        | "optional_parameter_declaration"
                        | "variadic_parameter_declaration"
                )
            })
            .flat_map(|p| self.declaration(p, true))
            .collect()
    }

    /// lambda_capture_initializer (`[e = x]`): a name the lambda declares without
    /// a type, visible after its initializer (which is read in the enclosing scope).
    fn captures<'t>(&self, specifier: Option<Node<'t>>) -> Vec<Binding<'t>> {
        let Some(specifier) = specifier else {
            return Vec::new();
        };
        let mut cursor = specifier.walk();
        specifier
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "lambda_capture_initializer")
            .filter(|c| {
                c.child_by_field_name("left")
                    .is_some_and(|l| node_text(self.source, l) == self.name)
            })
            .map(|c| Binding {
                decl: None,
                from: c.end_byte(),
            })
            .collect()
    }

    /// condition_clause.initializer (`if (T x; c)`, `switch (T x; c)`) and
    /// condition_clause.value when it is a declaration (`if (T x = f())`).
    fn condition<'t>(&self, clause: Option<Node<'t>>) -> Vec<Binding<'t>> {
        let Some(clause) = clause else {
            return Vec::new();
        };
        let mut out = self.init_statement(clause.child_by_field_name("initializer"));
        if let Some(value) = clause
            .child_by_field_name("value")
            .filter(|v| v.kind() == "declaration")
        {
            out.extend(self.declaration(value, true));
        }
        out
    }

    /// init_statement: its `declaration` child (the others declare no value).
    fn init_statement<'t>(&self, init: Option<Node<'t>>) -> Vec<Binding<'t>> {
        let Some(init) = init else {
            return Vec::new();
        };
        let mut cursor = init.walk();
        init.named_children(&mut cursor)
            .filter(|c| c.kind() == "declaration")
            .flat_map(|c| self.declaration(c, true))
            .collect()
    }

    /// The declarations among the children of a block (or of any scope without a
    /// reading of its own): declaration, using_declaration, and, through
    /// labeled_statement and case_statement, the declaration under a label. A
    /// preproc_if / ifdef / elif / elifdef / else child is read as `conditional`:
    /// which branch is compiled is not read, so its names carry no type.
    fn block<'t>(&self, scope: Node<'t>, conditional: bool) -> Vec<Binding<'t>> {
        let mut out = Vec::new();
        let mut cursor = scope.walk();
        for child in scope.named_children(&mut cursor) {
            match child.kind() {
                "declaration" => out.extend(self.declaration(child, !conditional)),
                "using_declaration" => out.extend(self.using(child)),
                "labeled_statement" | "case_statement" => {
                    out.extend(self.block(child, conditional));
                }
                "preproc_if" | "preproc_ifdef" | "preproc_elif" | "preproc_elifdef"
                | "preproc_else" => out.extend(self.block(child, true)),
                _ => {}
            }
        }
        out
    }

    /// using_declaration: `using ns::e;` makes `e` visible in the block (the `name`
    /// of its qualified_identifier, or its identifier), without a type read here.
    fn using<'t>(&self, decl: Node<'t>) -> Vec<Binding<'t>> {
        let mut cursor = decl.walk();
        let names_it = decl.named_children(&mut cursor).any(|c| {
            let mut last = c;
            while last.kind() == "qualified_identifier" {
                let Some(name) = last.child_by_field_name("name") else {
                    break;
                };
                last = name;
            }
            last.kind() == "identifier" && node_text(self.source, last) == self.name
        });
        if !names_it {
            return Vec::new();
        }
        vec![Binding {
            decl: None,
            from: decl.start_byte(),
        }]
    }

    /// The `declarator` fields of `decl` (a declaration, a parameter, a range-for)
    /// that declare the name. `typed` is false when the declaration is read without
    /// its type.
    fn declaration<'t>(&self, decl: Node<'t>, typed: bool) -> Vec<Binding<'t>> {
        let mut out = Vec::new();
        let mut cursor = decl.walk();
        for declarator in decl.children_by_field_name("declarator", &mut cursor) {
            self.declarator(declarator, (decl, typed), &mut out);
        }
        out
    }

    /// Walks a declarator down to the names it declares: identifier, init_declarator,
    /// pointer / reference / array / parenthesized / attributed / variadic
    /// declarators, and the function declarator of a declaration (`B e(y);`). A structured_binding_declarator declares names whatever type the
    /// declaration writes: they get none.
    fn declarator<'t>(&self, node: Node<'t>, of: (Node<'t>, bool), out: &mut Vec<Binding<'t>>) {
        let (decl, typed) = of;
        match node.kind() {
            "identifier" if node_text(self.source, node) == self.name => {
                out.push(Binding {
                    decl: typed.then_some(decl),
                    from: decl.start_byte(),
                });
            }
            "structured_binding_declarator" => {
                let mut cursor = node.walk();
                let named = node
                    .named_children(&mut cursor)
                    .any(|c| c.kind() == "identifier" && node_text(self.source, c) == self.name);
                if named {
                    out.push(Binding {
                        decl: None,
                        from: decl.start_byte(),
                    });
                }
            }
            "function_declarator" if decl.kind() == "declaration" => {
                // `B e(y);` is a function declarator to the grammar and a variable
                // of type `B` to the compiler whenever `y` is not a type.
                if let Some(inner) = node.child_by_field_name("declarator") {
                    self.declarator(inner, of, out);
                }
            }
            "init_declarator" | "pointer_declarator" | "array_declarator" => {
                if let Some(inner) = node.child_by_field_name("declarator") {
                    self.declarator(inner, of, out);
                }
            }
            "reference_declarator"
            | "parenthesized_declarator"
            | "attributed_declarator"
            | "variadic_declarator" => {
                let mut cursor = node.walk();
                for inner in node.named_children(&mut cursor) {
                    self.declarator(inner, of, out);
                }
            }
            _ => {}
        }
    }
}

/// The parameter list of a function or lambda, through its wrapping declarators.
fn parameters_of(callable: Node) -> Option<Node> {
    let mut declarator = callable.child_by_field_name("declarator");
    while let Some(d) = declarator {
        if let Some(parameters) = d.child_by_field_name("parameters") {
            return Some(parameters);
        }
        declarator = inner_declarator(d);
    }
    None
}

/// The declarator a wrapping declarator (pointer, reference, array, init,
/// parenthesis) applies to. A reference declarator names it without a field.
fn inner_declarator(node: Node) -> Option<Node> {
    node.child_by_field_name("declarator")
        .or_else(|| node.named_child(u32::try_from(node.named_child_count().checked_sub(1)?).ok()?))
}

/// The class a declaration's `type` names, as written: `Bloom` of `const
/// Bloom*`, `ns::Bloom` of `ns::Bloom&`, `Map` of `Map<K, V>`. `auto`,
/// `decltype` and the builtin types give `None`.
fn written_type(source: &str, decl: Node) -> Option<String> {
    let ty = decl.child_by_field_name("type")?;
    let text = match ty.kind() {
        "type_identifier" | "qualified_identifier" | "template_type" => node_text(source, ty),
        "struct_specifier" | "class_specifier" | "union_specifier" => {
            node_text(source, ty.child_by_field_name("name")?)
        }
        _ => return None,
    };
    let name = plain_type_text(&text);
    let first = name.split("::").next().unwrap_or(&name);
    if name.is_empty() || is_template_parameter(source, decl, first) {
        return None;
    }
    Some(name)
}

/// True when `name` is a parameter of a template that encloses `at`
/// (`template <class T>`, `typename... Ts`, `template <class> class C`): a type
/// written with it names no class of the repository.
fn is_template_parameter(source: &str, at: Node, name: &str) -> bool {
    let mut scope = at.parent();
    while let Some(s) = scope {
        if s.kind() == "template_declaration" {
            if let Some(list) = s.child_by_field_name("parameters") {
                if template_parameter_names(source, list)
                    .iter()
                    .any(|n| n == name)
                {
                    return true;
                }
            }
        }
        scope = s.parent();
    }
    false
}

/// The names a `template_parameter_list` declares for types: `T` of `class T`,
/// `typename... Ts` or `class T = Default`, and `C` of `template <class> class
/// C`. A non-type parameter (`int N`) names no type.
fn template_parameter_names(source: &str, list: Node) -> Vec<String> {
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter_map(|parameter| type_parameter_name(source, parameter))
        .collect()
}

/// The first `type_identifier` of a type parameter is its name (a default type
/// follows it).
fn type_parameter_name(source: &str, parameter: Node) -> Option<String> {
    match parameter.kind() {
        "type_parameter_declaration"
        | "variadic_type_parameter_declaration"
        | "optional_type_parameter_declaration" => {
            let mut cursor = parameter.walk();
            let name = parameter
                .named_children(&mut cursor)
                .find(|c| c.kind() == "type_identifier")
                .map(|c| node_text(source, c));
            name
        }
        "template_template_parameter_declaration" => {
            let mut cursor = parameter.walk();
            let name = parameter
                .named_children(&mut cursor)
                .find_map(|c| type_parameter_name(source, c));
            name
        }
        _ => None,
    }
}

/// `text` without its `<...>` groups and whitespace.
pub(super) fn plain_type_text(text: &str) -> String {
    strip_generic_groups(text)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}
