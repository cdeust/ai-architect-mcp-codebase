// lib.rs — library entry point for integration tests.
//
// Re-exports the modules that integration tests need. The binary entry
// point remains src/main.rs. Cargo discovers this automatically via
// convention (src/lib.rs + src/main.rs coexist in the same crate).

pub mod ambiguity_policy;
pub mod artifact;
pub mod bridge;
pub mod call_evidence;
pub mod clustering;
pub mod cochange;
pub mod epistemic;
pub mod git_diff;
pub mod graph_store;
pub mod history;
pub mod hook_augment;
pub mod host_install;
pub mod indexer;
pub mod language_provider;
pub mod lsp_client;
pub mod lsp_resolver;
pub mod macro_expansion;
pub mod parser;
pub mod prd_input;
pub mod prd_validator;
pub mod resolver;
pub mod resolver_layers;
pub mod search;
pub mod security_gates;
pub mod semantic_diff;
pub mod stdlib_index;
pub mod token_surface;
pub mod tool_schemas;
