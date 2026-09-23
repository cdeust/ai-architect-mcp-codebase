// graph_accuracy_receiver_calls — same-class `self.m()` Calls edges the
// graph_accuracy annotations omitted while the resolver could not bind them
// (issue #290). Every row was checked pair-for-pair against CPython's `ast`
// (same-class `self.<method>(...)` calls inside each class's methods).

/// (class, calling method, called method) per fixture.
pub type ReceiverCalls = &'static [(&'static str, &'static str, &'static str)];

/// infrastructure/embedding_engine.py: 16 same-class receiver calls.
pub const EMBEDDING_ENGINE: ReceiverCalls = &[
    ("EmbeddingEngine", "_encode_vec", "_fallback_encode"),
    ("EmbeddingEngine", "_encode_vec", "_fallback_to_cpu"),
    ("EmbeddingEngine", "_encode_vec", "_normalize"),
    ("EmbeddingEngine", "_ensure_model", "_resolve_device"),
    (
        "EmbeddingEngine",
        "_ensure_model",
        "_trigger_background_install",
    ),
    ("EmbeddingEngine", "_fallback_encode", "_normalize"),
    ("EmbeddingEngine", "_fallback_to_cpu", "_ensure_model"),
    ("EmbeddingEngine", "_resolve_device", "_detect_device"),
    ("EmbeddingEngine", "encode", "_cache_key"),
    ("EmbeddingEngine", "encode", "_encode_vec"),
    ("EmbeddingEngine", "encode", "_ensure_model"),
    ("EmbeddingEngine", "encode", "_fallback_encode"),
    ("EmbeddingEngine", "encode_batch", "_ensure_model"),
    ("EmbeddingEngine", "encode_batch", "_fallback_encode"),
    ("EmbeddingEngine", "encode_batch", "_fallback_to_cpu"),
    ("EmbeddingEngine", "encode_batch", "_normalize"),
];

/// infrastructure/mcp_client.py: 13 same-class receiver calls.
pub const MCP_CLIENT: ReceiverCalls = &[
    ("MCPClient", "_idle_loop", "close"),
    ("MCPClient", "_perform_handshake", "_idle_loop"),
    ("MCPClient", "_perform_handshake", "_notify"),
    ("MCPClient", "_perform_handshake", "_send"),
    ("MCPClient", "_perform_handshake", "_touch_activity"),
    ("MCPClient", "_perform_handshake", "close"),
    ("MCPClient", "_stderr_loop", "_open_stderr_log"),
    ("MCPClient", "call", "_send"),
    ("MCPClient", "call", "_touch_activity"),
    ("MCPClient", "connect", "_perform_handshake"),
    ("MCPClient", "connect", "_read_loop"),
    ("MCPClient", "connect", "_spawn_process"),
    ("MCPClient", "connect", "_stderr_loop"),
];

/// infrastructure/pg_store.py: 40 same-class receiver calls.
pub const PG_STORE: ReceiverCalls = &[
    ("PgMemoryStore", "__init__", "_create_connection"),
    ("PgMemoryStore", "__init__", "_deallocate_all"),
    ("PgMemoryStore", "__init__", "_init_schema"),
    ("PgMemoryStore", "_execute", "_execute_on_conn"),
    ("PgMemoryStore", "_normalize_memory_row", "_vector_to_bytes"),
    ("PgMemoryStore", "_reconnect", "_create_connection"),
    ("PgMemoryStore", "batch_pool", "_open_batch_pool"),
    ("PgMemoryStore", "bump_heat_raw", "_execute"),
    ("PgMemoryStore", "delete_memory", "_execute"),
    ("PgMemoryStore", "get_embeddings_for_memories", "_execute"),
    (
        "PgMemoryStore",
        "get_embeddings_for_memories",
        "_vector_to_bytes",
    ),
    ("PgMemoryStore", "get_homeostatic_factor", "_execute"),
    ("PgMemoryStore", "get_hot_embeddings", "_execute"),
    ("PgMemoryStore", "get_hot_embeddings", "_vector_to_bytes"),
    ("PgMemoryStore", "get_memory", "_execute"),
    ("PgMemoryStore", "get_memory", "_normalize_memory_row"),
    ("PgMemoryStore", "get_temporal_co_access", "_execute"),
    ("PgMemoryStore", "get_user_mood", "_execute"),
    ("PgMemoryStore", "get_user_mood_state", "_execute"),
    ("PgMemoryStore", "insert_memory", "_bytes_to_vector"),
    ("PgMemoryStore", "insert_memory", "_execute"),
    (
        "PgMemoryStore",
        "interactive_pool",
        "_open_interactive_pool",
    ),
    ("PgMemoryStore", "mark_memory_stale", "_execute"),
    ("PgMemoryStore", "recall_memories", "_bytes_to_vector"),
    ("PgMemoryStore", "recall_memories", "_execute"),
    ("PgMemoryStore", "search_fts", "_execute"),
    ("PgMemoryStore", "search_vectors", "_bytes_to_vector"),
    ("PgMemoryStore", "search_vectors", "_execute"),
    ("PgMemoryStore", "set_homeostatic_factor", "_execute"),
    ("PgMemoryStore", "set_memory_protected", "_execute"),
    ("PgMemoryStore", "set_user_mood", "_execute"),
    ("PgMemoryStore", "spread_activation_memories", "_execute"),
    ("PgMemoryStore", "update_memories_heat_batch", "_execute"),
    ("PgMemoryStore", "update_memory_access", "_execute"),
    (
        "PgMemoryStore",
        "update_memory_compression",
        "_bytes_to_vector",
    ),
    ("PgMemoryStore", "update_memory_compression", "_execute"),
    ("PgMemoryStore", "update_memory_heat", "bump_heat_raw"),
    ("PgMemoryStore", "update_memory_importance", "_execute"),
    ("PgMemoryStore", "update_memory_metamemory", "_execute"),
    ("_MaterializedCursor", "__iter__", "fetchone"),
];

/// tests/test_http_server.py: 14 same-class receiver calls.
pub const TEST_HTTP_SERVER: ReceiverCalls = &[
    (
        "TestHandlerBehavior",
        "test_do_options_returns_204",
        "_create_handler_class",
    ),
    (
        "TestHandlerBehavior",
        "test_do_options_returns_204",
        "_make_handler",
    ),
    (
        "TestHandlerBehavior",
        "test_get_graph_returns_json",
        "_create_handler_class",
    ),
    (
        "TestHandlerBehavior",
        "test_get_graph_returns_json",
        "_make_handler",
    ),
    (
        "TestHandlerBehavior",
        "test_get_resets_idle_timer",
        "_create_handler_class",
    ),
    (
        "TestHandlerBehavior",
        "test_get_resets_idle_timer",
        "_make_handler",
    ),
    (
        "TestHandlerBehavior",
        "test_get_root_returns_html",
        "_create_handler_class",
    ),
    (
        "TestHandlerBehavior",
        "test_get_root_returns_html",
        "_make_handler",
    ),
    (
        "TestHandlerBehavior",
        "test_get_sets_no_cache",
        "_create_handler_class",
    ),
    (
        "TestHandlerBehavior",
        "test_get_sets_no_cache",
        "_make_handler",
    ),
    (
        "TestHandlerBehavior",
        "test_log_message_suppressed",
        "_create_handler_class",
    ),
    (
        "TestHandlerBehavior",
        "test_log_message_suppressed",
        "_make_handler",
    ),
    (
        "TestHandlerBehavior",
        "test_send_header_cors_is_noop",
        "_create_handler_class",
    ),
    (
        "TestHandlerBehavior",
        "test_send_header_cors_is_noop",
        "_make_handler",
    ),
];

/// Appends one relaxed (count-matched) `Calls` edge per row, mirroring
/// `push_method_to_fn_calls` in graph_accuracy.rs.
pub fn push_method_to_method_calls(
    edges: &mut Vec<super::ExpectedEdge>,
    prefix: &str,
    calls: ReceiverCalls,
) {
    for (i, (cls, caller, callee)) in calls.iter().enumerate() {
        edges.push(super::ExpectedEdge {
            kind: "Calls",
            from_qn: format!("{prefix}::{cls}::{caller}::callsite::__m2m__::{i}"),
            to_qn: format!("{prefix}::{cls}::{callee}"),
        });
    }
}
