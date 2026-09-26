// resolver::receiver::imports: issues #373 and #380. The `use` declarations of
// every Rust module, read once per resolve pass, keyed by the module that
// declares them: a file (`Defines_File_Import`) or an inline `mod`
// (`Defines_Module_Import`). A name a `use` binds is visible in that module
// only, so a lookup never mixes the imports of two modules of one file.
//
// source: The Rust Reference, "Use declarations" (a `use` binds a name in the
// module that contains it; `as` renames it; `*` imports every public name).

use super::*;
use crate::graph_store::import_roots::CrateEvidence;

/// One `use` leaf: the path as written, its `as` name ('' when none), and
/// whether it is a glob (the path then omits the `::*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::resolver) struct ImportRow {
    pub(in crate::resolver) path: String,
    pub(in crate::resolver) alias: String,
    pub(in crate::resolver) is_glob: bool,
}

impl ImportRow {
    /// The name the leaf binds: the alias, else the last segment of the path.
    pub(in crate::resolver) fn binds(&self) -> &str {
        if self.alias.is_empty() {
            self.path.rsplit("::").next().unwrap_or(&self.path)
        } else {
            &self.alias
        }
    }
}

/// The imports of each module, and the modules of each module path.
#[derive(Default)]
pub(in crate::resolver) struct ModuleImports {
    by_scope: HashMap<String, Vec<ImportRow>>,
    /// Module path (from the root of its crate) to the scopes that have it.
    by_module: HashMap<Vec<String>, Vec<String>>,
    /// Module path of every file, inline `mod` and enum (a glob of its
    /// variants) of the repository, to the files that hold it.
    modules: HashMap<Vec<String>, Vec<String>>,
}

impl ModuleImports {
    /// A graph that cannot be read yields no imports: every lookup then finds
    /// nothing, and the callers keep their behaviour from before #380.
    pub(in crate::resolver) fn load(store: &GraphStore, evidence: &CrateEvidence) -> Self {
        let mut rows = Vec::new();
        for query in [
            "MATCH (s:File)-[:Defines_File_Import]->(i:Import) WHERE i.language = 'rust' \
             RETURN s.id, i.path, i.alias, i.is_glob",
            "MATCH (s:Module)-[:Defines_Module_Import]->(i:Import) WHERE i.language = 'rust' \
             RETURN s.id, i.path, i.alias, i.is_glob",
        ] {
            if let Ok(qr) = store.execute_query(query) {
                rows.extend(qr.rows.into_iter().filter(|r| r.len() >= 4));
            }
        }
        let mut imports = ModuleImports::default();
        for query in [
            "MATCH (n:File) RETURN n.id",
            "MATCH (n:Module) RETURN n.id",
            "MATCH (n:Enum) RETURN n.qualified_name",
        ] {
            let Ok(qr) = store.execute_query(query) else {
                continue;
            };
            for row in qr.rows.iter().filter_map(|r| r.first()) {
                if extract_file_prefix_or_self(row).ends_with(".rs") {
                    imports.add_module(evidence, row);
                }
            }
        }
        for row in rows {
            imports.insert(
                evidence,
                &row[0],
                ImportRow {
                    path: row[1].clone(),
                    alias: row[2].clone(),
                    is_glob: row[3] == "true",
                },
            );
        }
        imports
    }

    pub(in crate::resolver) fn insert(
        &mut self,
        evidence: &CrateEvidence,
        scope: &str,
        row: ImportRow,
    ) {
        if !self.by_scope.contains_key(scope) {
            self.by_module
                .entry(scope_module_path(evidence, scope))
                .or_default()
                .push(scope.to_string());
        }
        self.by_scope
            .entry(scope.to_string())
            .or_default()
            .push(row);
    }

    /// Records the module (file, inline `mod` or enum) `qn` names.
    pub(in crate::resolver) fn add_module(&mut self, evidence: &CrateEvidence, qn: &str) {
        self.modules
            .entry(scope_module_path(evidence, qn))
            .or_default()
            .push(extract_file_prefix_or_self(qn));
    }

    /// The files holding a module (file, inline `mod` or enum) at `module`.
    pub(in crate::resolver) fn module_files(&self, module: &[String]) -> &[String] {
        self.modules.get(module).map_or(&[], Vec::as_slice)
    }

    /// The imports `scope` declares.
    pub(in crate::resolver) fn of_scope(&self, scope: &str) -> &[ImportRow] {
        self.by_scope.get(scope).map_or(&[], Vec::as_slice)
    }

    /// The scopes whose module path, from the root of their crate, is `module`.
    pub(in crate::resolver) fn scopes_at(&self, module: &[String]) -> &[String] {
        self.by_module.get(module).map_or(&[], Vec::as_slice)
    }
}

/// The module path of a scope: its file's, then the inline modules after it.
pub(in crate::resolver) fn scope_module_path(evidence: &CrateEvidence, scope: &str) -> Vec<String> {
    let file = extract_file_prefix_or_self(scope);
    let mut path = super::written_path::file_module_path(evidence, &file);
    path.extend(
        scope
            .strip_prefix(&file)
            .unwrap_or("")
            .split("::")
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    );
    path
}

/// The scope the caller's own `use` declarations live in: its file, then the
/// inline modules around it.
pub(in crate::resolver) fn caller_scope(idx: &SymbolIndex, caller_qn: &str) -> String {
    let mut scope = extract_file_prefix_or_self(caller_qn);
    for module in super::written_path::inline_modules(idx, caller_qn) {
        scope = format!("{scope}::{module}");
    }
    scope
}
