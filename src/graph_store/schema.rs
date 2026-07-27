// graph_store::schema — node-label list, REL_TABLES registry, relationship
// predicates, and schema-membership queries.
//
// Extracted from graph_store.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared store
// vocabulary exactly as when this lived in one module.

use super::*;

// ---------------------------------------------------------------------------
// Schema DDL generators
// ---------------------------------------------------------------------------

pub(crate) const NODE_LABELS: &[&str] = &[
    NODE_DIRECTORY,
    NODE_FILE,
    NODE_MODULE,
    NODE_FUNCTION,
    NODE_METHOD,
    NODE_STRUCT,
    NODE_ENUM,
    NODE_VARIANT,
    NODE_TRAIT,
    NODE_FIELD,
    NODE_CONSTANT,
    NODE_TYPE_ALIAS,
    NODE_IMPORT,
    NODE_CALL_SITE,
    NODE_COMMUNITY,
    NODE_PROCESS,
    NODE_STDLIB_SYMBOL,
    NODE_COMMIT,
    NODE_VERSION,
    // Infrastructure-as-code layer (issue #63).
    NODE_IAC_RESOURCE,
    NODE_IAC_MODULE,
    NODE_IAC_IMAGE,
];

/// Single source of truth for all relationship tables: (name, from, to).
/// Used for DDL generation, endpoint lookup, and edge counting.
/// LadybugDB does not support REL TABLE GROUP in the lbug 0.15.x Rust crate
/// (no references in source). We create one rel table per (source, target) pair
/// with a naming convention: `{Kind}_{From}_{To}`.
pub const REL_TABLES: &[(&str, &str, &str)] = &[
    // Contains — source: stages/stage-3.md §schema
    ("Contains_Dir_File", NODE_DIRECTORY, NODE_FILE),
    ("Contains_Dir_Dir", NODE_DIRECTORY, NODE_DIRECTORY),
    ("Contains_File_Module", NODE_FILE, NODE_MODULE),
    // Defines — source: stages/stage-3.md §schema
    ("Defines_File_Function", NODE_FILE, NODE_FUNCTION),
    ("Defines_File_Struct", NODE_FILE, NODE_STRUCT),
    ("Defines_File_Enum", NODE_FILE, NODE_ENUM),
    ("Defines_File_Trait", NODE_FILE, NODE_TRAIT),
    ("Defines_File_Constant", NODE_FILE, NODE_CONSTANT),
    ("Defines_File_TypeAlias", NODE_FILE, NODE_TYPE_ALIAS),
    // source: B1 fix — Q9/Q14 expect File->Import edges, and resolver
    // also needs to walk a Module->Import parent for mod-nested uses.
    ("Defines_File_Import", NODE_FILE, NODE_IMPORT),
    ("Defines_Module_Import", NODE_MODULE, NODE_IMPORT),
    ("Defines_Module_Function", NODE_MODULE, NODE_FUNCTION),
    ("Defines_Module_Struct", NODE_MODULE, NODE_STRUCT),
    ("Defines_Module_Enum", NODE_MODULE, NODE_ENUM),
    ("Defines_Module_Trait", NODE_MODULE, NODE_TRAIT),
    ("Defines_Module_Constant", NODE_MODULE, NODE_CONSTANT),
    ("Defines_Module_TypeAlias", NODE_MODULE, NODE_TYPE_ALIAS),
    // HasMethod — source: stages/stage-3.md §schema
    ("HasMethod_Struct_Method", NODE_STRUCT, NODE_METHOD),
    ("HasMethod_Enum_Method", NODE_ENUM, NODE_METHOD),
    ("HasMethod_Trait_Method", NODE_TRAIT, NODE_METHOD),
    // HasField — source: stages/stage-3.md §schema
    ("HasField_Struct_Field", NODE_STRUCT, NODE_FIELD),
    ("HasField_Enum_Field", NODE_ENUM, NODE_FIELD),
    // HasVariant — source: stages/stage-3.md §schema
    ("HasVariant_Enum_Variant", NODE_ENUM, NODE_VARIANT),
    // Imports — source: stages/stage-3b.md §2, §3
    ("Imports_File_File", NODE_FILE, NODE_FILE),
    // References — all-file indexing: a non-code file (e.g. Markdown doc)
    // pointing at another file via a relative link `[text](./path)`. Distinct
    // from Imports (code dependency); this is documentation/reference cross-
    // linking so the graph connects docs to the files they describe.
    ("References_File_File", NODE_FILE, NODE_FILE),
    ("Imports_File_Module", NODE_FILE, NODE_MODULE),
    ("Imports_File_Function", NODE_FILE, NODE_FUNCTION),
    ("Imports_File_Method", NODE_FILE, NODE_METHOD),
    ("Imports_File_Struct", NODE_FILE, NODE_STRUCT),
    ("Imports_File_Enum", NODE_FILE, NODE_ENUM),
    ("Imports_File_Trait", NODE_FILE, NODE_TRAIT),
    ("Imports_File_Constant", NODE_FILE, NODE_CONSTANT),
    ("Imports_File_TypeAlias", NODE_FILE, NODE_TYPE_ALIAS),
    ("Imports_Module_Function", NODE_MODULE, NODE_FUNCTION),
    ("Imports_Module_Struct", NODE_MODULE, NODE_STRUCT),
    ("Imports_Module_Enum", NODE_MODULE, NODE_ENUM),
    ("Imports_Module_Trait", NODE_MODULE, NODE_TRAIT),
    ("Imports_Module_Constant", NODE_MODULE, NODE_CONSTANT),
    ("Imports_Module_TypeAlias", NODE_MODULE, NODE_TYPE_ALIAS),
    // Calls — source: stages/stage-3b.md §2, §3
    ("Calls_Function_Function", NODE_FUNCTION, NODE_FUNCTION),
    ("Calls_Function_Method", NODE_FUNCTION, NODE_METHOD),
    ("Calls_Method_Function", NODE_METHOD, NODE_FUNCTION),
    ("Calls_Method_Method", NODE_METHOD, NODE_METHOD),
    // source: Spike B' BUG #12 fix — the parser emits a Defines edge from
    // Function/Method to a CallSite (parser/python.rs:485) but no rel table
    // existed for it, so every insert silently dropped. CallSite nodes were
    // orphans in the graph. Adding these tables restores the linkage.
    ("Defines_Function_CallSite", NODE_FUNCTION, NODE_CALL_SITE),
    ("Defines_Method_CallSite", NODE_METHOD, NODE_CALL_SITE),
    // CallSite → callee — emitted by resolver when the callee resolves.
    ("Calls_CallSite_Function", NODE_CALL_SITE, NODE_FUNCTION),
    ("Calls_CallSite_Method", NODE_CALL_SITE, NODE_METHOD),
    (
        "Calls_CallSite_StdlibSymbol",
        NODE_CALL_SITE,
        NODE_STDLIB_SYMBOL,
    ),
    // Implements — source: stages/stage-3b.md §2, §3
    ("Implements_Struct_Trait", NODE_STRUCT, NODE_TRAIT),
    ("Implements_Enum_Trait", NODE_ENUM, NODE_TRAIT),
    // Extends — source: stages/stage-3b.md §2, §3
    ("Extends_Trait_Trait", NODE_TRAIT, NODE_TRAIT),
    // source: Spike B' BUG #9 fix — Python class inheritance (Cortex uses
    // Struct label for Python classes); resolved by resolve_extends.
    ("Extends_Struct_Struct", NODE_STRUCT, NODE_STRUCT),
    ("Extends_Enum_Enum", NODE_ENUM, NODE_ENUM),
    // Uses — source: stages/stage-3b.md §2, §3
    ("Uses_Function_Struct", NODE_FUNCTION, NODE_STRUCT),
    ("Uses_Function_Enum", NODE_FUNCTION, NODE_ENUM),
    ("Uses_Function_Trait", NODE_FUNCTION, NODE_TRAIT),
    ("Uses_Function_TypeAlias", NODE_FUNCTION, NODE_TYPE_ALIAS),
    ("Uses_Method_Struct", NODE_METHOD, NODE_STRUCT),
    ("Uses_Method_Enum", NODE_METHOD, NODE_ENUM),
    ("Uses_Method_Trait", NODE_METHOD, NODE_TRAIT),
    ("Uses_Method_TypeAlias", NODE_METHOD, NODE_TYPE_ALIAS),
    ("Uses_Struct_Struct", NODE_STRUCT, NODE_STRUCT),
    ("Uses_Struct_Enum", NODE_STRUCT, NODE_ENUM),
    ("Uses_Struct_Trait", NODE_STRUCT, NODE_TRAIT),
    ("Uses_Field_Struct", NODE_FIELD, NODE_STRUCT),
    ("Uses_Field_Enum", NODE_FIELD, NODE_ENUM),
    ("Uses_Field_Trait", NODE_FIELD, NODE_TRAIT),
    ("Uses_Field_TypeAlias", NODE_FIELD, NODE_TYPE_ALIAS),
    // 3b-v2 Layer 5 (stdlib index) + Layer 4 (macro expansion) — source:
    // stages/stage-3b-v2.md §5. Stdlib targets carry resolution_method
    // = "stdlib-index" (confidence 0.95) or "macro-expansion" (0.85).
    (
        "Calls_Function_StdlibSymbol",
        NODE_FUNCTION,
        NODE_STDLIB_SYMBOL,
    ),
    ("Calls_Method_StdlibSymbol", NODE_METHOD, NODE_STDLIB_SYMBOL),
    (
        "Implements_Struct_StdlibSymbol",
        NODE_STRUCT,
        NODE_STDLIB_SYMBOL,
    ),
    (
        "Implements_Enum_StdlibSymbol",
        NODE_ENUM,
        NODE_STDLIB_SYMBOL,
    ),
    // 3c MemberOf — source: stages/stage-3c.md §4.2
    ("MemberOf_Function_Community", NODE_FUNCTION, NODE_COMMUNITY),
    ("MemberOf_Method_Community", NODE_METHOD, NODE_COMMUNITY),
    ("MemberOf_Struct_Community", NODE_STRUCT, NODE_COMMUNITY),
    ("MemberOf_Enum_Community", NODE_ENUM, NODE_COMMUNITY),
    ("MemberOf_Trait_Community", NODE_TRAIT, NODE_COMMUNITY),
    ("MemberOf_Constant_Community", NODE_CONSTANT, NODE_COMMUNITY),
    (
        "MemberOf_TypeAlias_Community",
        NODE_TYPE_ALIAS,
        NODE_COMMUNITY,
    ),
    ("MemberOf_Module_Community", NODE_MODULE, NODE_COMMUNITY),
    // 3c EntryPointOf — source: stages/stage-3c.md §4.2
    ("EntryPointOf_Function_Process", NODE_FUNCTION, NODE_PROCESS),
    ("EntryPointOf_Method_Process", NODE_METHOD, NODE_PROCESS),
    // 3c ParticipatesIn — source: stages/stage-3c.md §4.2
    (
        "ParticipatesIn_Function_Process",
        NODE_FUNCTION,
        NODE_PROCESS,
    ),
    ("ParticipatesIn_Method_Process", NODE_METHOD, NODE_PROCESS),
    // History layer — source: second-brain history requirement.
    // Commit lineage + per-entity version spine. Every edge is read in both
    // directions by the query surface, so a consumer can walk:
    //   entity  <-VersionOf-  Version  -ChangedIn->  Commit  -PreviousVersion->  Commit
    // and the reverse (a commit's changed entities, a version's successor).
    // PreviousVersion_Version_Version chains an entity's own revisions over
    // time; PreviousVersion_Commit_Commit is the commit ancestry (first parent).
    ("PreviousVersion_Commit_Commit", NODE_COMMIT, NODE_COMMIT),
    ("VersionOf_Version_File", NODE_VERSION, NODE_FILE),
    ("VersionOf_Version_Function", NODE_VERSION, NODE_FUNCTION),
    ("VersionOf_Version_Method", NODE_VERSION, NODE_METHOD),
    ("VersionOf_Version_Struct", NODE_VERSION, NODE_STRUCT),
    ("VersionOf_Version_Enum", NODE_VERSION, NODE_ENUM),
    ("VersionOf_Version_Trait", NODE_VERSION, NODE_TRAIT),
    ("ChangedIn_Version_Commit", NODE_VERSION, NODE_COMMIT),
    (
        "PreviousVersion_Version_Version",
        NODE_VERSION,
        NODE_VERSION,
    ),
    // Temporal coupling (issue #58) — Tornhill-style git co-change. One
    // File→File edge per pair that changed together often enough; properties
    // carry the coupling strength. source: Tornhill 2015 (temporal coupling),
    // thresholds from DeusData/codebase-memory-mcp pass_githistory.c.
    ("FILE_CHANGES_WITH", NODE_FILE, NODE_FILE),
    // Runtime-observed calls (issue #58) — ingest_traces creates these where a
    // runtime caller→callee has NO static Calls edge (the divergence signal:
    // runtime truth the static resolver missed). Symbol-level, so one table per
    // (Function|Method)×(Function|Method) callable pair.
    (
        "OBSERVED_CALLS_Function_Function",
        NODE_FUNCTION,
        NODE_FUNCTION,
    ),
    ("OBSERVED_CALLS_Function_Method", NODE_FUNCTION, NODE_METHOD),
    ("OBSERVED_CALLS_Method_Function", NODE_METHOD, NODE_FUNCTION),
    ("OBSERVED_CALLS_Method_Method", NODE_METHOD, NODE_METHOD),
    // Infrastructure-as-code layer (issue #63).
    //
    // Defines_File_Iac* are structural facts (the file literally contains this
    // manifest) — the `Defines_` prefix routes them through the structural-
    // provenance shape (confidence 1.0, "iac-direct"), exactly like File→symbol.
    ("Defines_File_IacResource", NODE_FILE, NODE_IAC_RESOURCE),
    ("Defines_File_IacModule", NODE_FILE, NODE_IAC_MODULE),
    // Imports_* are the reference edges. The `Imports_` prefix is deliberate:
    // `clustering::get_impact` reverse-traverses every `Imports_*` table, so a
    // manifest that references a File/image/base-overlay is reported as a
    // reverse-dependent with zero changes to the impact walker (issue #63
    // criterion 5). All carry (confidence, resolution_method): confidence < 1.0
    // marks the heuristic cross-file name/path resolutions (misses reported, not
    // faked — issue #63 criterion 3); `resolution_method` carries the provenance
    // (e.g. "iac-name-match:ConfigMap/app-config").
    (
        "Imports_IacResource_IacImage",
        NODE_IAC_RESOURCE,
        NODE_IAC_IMAGE,
    ),
    ("Imports_IacResource_File", NODE_IAC_RESOURCE, NODE_FILE),
    ("Imports_IacModule_File", NODE_IAC_MODULE, NODE_FILE),
    (
        "Imports_IacModule_IacModule",
        NODE_IAC_MODULE,
        NODE_IAC_MODULE,
    ),
];

/// 3b resolution edge tables carry confidence + resolution_method properties.
/// source: stages/stage-3b.md §2 "Edge properties"
pub(crate) fn is_resolution_rel(name: &str) -> bool {
    name.starts_with("Imports_")
        || name.starts_with("Calls_")
        || name.starts_with("Implements_")
        || name.starts_with("Extends_")
        || name.starts_with("Uses_")
        || name.starts_with("References_")
}

/// Structural edges from the parser (Defines, HasMethod, HasField,
/// HasVariant) are ground-truth AST facts. After Spike B' BUG #4 fix
/// they also carry confidence + resolution_method so downstream
/// consumers see uniform provenance across all edge kinds — structural
/// edges default to (1.0, "direct-ast") at insert time.
///
/// source: Spike B' BUG #4 — audited ap_graph.json had confidence/reason
/// = None on all 67,427 edges including structural ones. Adding the
/// columns + populating defaults at emit time fixes that uniformly.
pub(crate) fn is_structural_provenance_rel(name: &str) -> bool {
    name.starts_with("Defines_")
        || name.starts_with("HasMethod_")
        || name.starts_with("HasField_")
        || name.starts_with("HasVariant_")
}

pub(crate) fn is_entrypoint_rel(name: &str) -> bool {
    name.starts_with("EntryPointOf_")
}

pub(crate) fn is_participates_rel(name: &str) -> bool {
    name.starts_with("ParticipatesIn_")
}

/// Temporal-coupling edge (issue #58): File→File git co-change.
pub(crate) fn is_cochange_rel(name: &str) -> bool {
    name == "FILE_CHANGES_WITH"
}

/// Runtime-observed call edge (issue #58): ingest_traces divergence signal.
pub(crate) fn is_observed_calls_rel(name: &str) -> bool {
    name.starts_with("OBSERVED_CALLS_")
}

/// Symbol-level static Calls tables that ingest_traces annotates with an
/// `observed_count` (the runtime weight on a statically-known call). Excludes
/// the CallSite-level Calls tables — traces are symbol→symbol, not call-site.
pub fn is_observable_static_calls_rel(name: &str) -> bool {
    matches!(
        name,
        "Calls_Function_Function"
            | "Calls_Function_Method"
            | "Calls_Method_Function"
            | "Calls_Method_Method"
    )
}

/// Public schema check — single source of truth for "is this a valid rel
/// table name?" Producers that dynamically format edge type names should
/// validate against this before insertion to avoid schema-mismatch aborts.
pub fn is_known_rel_table(name: &str) -> bool {
    REL_TABLES.iter().any(|(n, _, _)| *n == name)
}

/// Single source of truth for "does this node label declare a `qualified_name`
/// column?" — mirrors `node_column_types`. A read-side traversal that binds
/// `n.qualified_name` MUST gate on this: lbug raises a hard Binder exception
/// (not a NULL) when a query references a property the matched label's table
/// does not declare, which silently drops the whole query's results. Labels
/// without it (File/Directory/Field/Import/CallSite/Community/Process/
/// StdlibSymbol/Commit/IacImage) are matched by `id` alone.
// source: node_table_ddl() — these are exactly the labels whose DDL lists a
// `qualified_name STRING` column.
pub fn label_has_qualified_name(label: &str) -> bool {
    matches!(
        label,
        NODE_MODULE
            | NODE_FUNCTION
            | NODE_METHOD
            | NODE_STRUCT
            | NODE_ENUM
            | NODE_VARIANT
            | NODE_TRAIT
            | NODE_CONSTANT
            | NODE_TYPE_ALIAS
            | NODE_VERSION
            | NODE_IAC_RESOURCE
            | NODE_IAC_MODULE
    )
}
