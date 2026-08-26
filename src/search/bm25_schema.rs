// search::bm25::schema — the BM25 index's field definitions, and the
// cross-version compatibility rules that go with them.
//
// Split from `bm25.rs` per §4.1 when review round 2's compatibility work
// pushed that file past 500 lines. The seam is a real one: everything here
// answers "what fields does an index have, and how do we address them
// safely", which is separable from building an index and from querying one.

use std::path::Path;
use tantivy::schema::{Field, Schema, STORED, TEXT};
use tantivy::Index;
#[cfg(test)]
use tantivy::{doc, IndexWriter};

#[cfg(test)]
use super::super::qualified_name::file_path_of;
#[cfg(test)]
use super::tokenize_symbol;

/// The field names this index is built with. Names, not ordinals, are the
/// stable identity of a field across binary versions — see [`OpenedFields`].
const F_QUALIFIED_NAME: &str = "qualified_name";
const F_NAME: &str = "name";
const F_LABEL: &str = "label";
const F_FILE_PATH: &str = "file_path";
const F_BODY: &str = "body";

pub struct Bm25Fields {
    pub qualified_name: Field,
    pub name: Field,
    pub label: Field,
    pub file_path: Field,
    pub body: Field,
}

pub fn build_schema() -> (Schema, Bm25Fields) {
    let mut builder = Schema::builder();
    let qualified_name = builder.add_text_field(F_QUALIFIED_NAME, TEXT | STORED);
    let name = builder.add_text_field(F_NAME, TEXT | STORED);
    let label = builder.add_text_field(F_LABEL, STORED);
    let file_path = builder.add_text_field(F_FILE_PATH, STORED);
    // Indexed only, never stored: a doc's full text has no business round-
    // tripping out of a search hit (the caller already has file_path to go
    // read it), and NOT storing it keeps the index itself from duplicating
    // the size of every prose file in the repo.
    let body = builder.add_text_field(F_BODY, TEXT);
    let schema = builder.build();
    (
        schema,
        Bm25Fields {
            qualified_name,
            name,
            label,
            file_path,
            body,
        },
    )
}

/// The fields of an index that was actually OPENED from disk, resolved by
/// name against that index's own persisted schema.
///
/// Why this type exists (review round 2, finding 1). A `Field` is an ordinal
/// into the schema's field vector, and tantivy resolves it unchecked:
/// `Schema::get_field_entry` is `&self.0.fields[field.field_id() as usize]`
/// (vendored tantivy-0.26.1, schema.rs:281), and `QueryParser::for_index`
/// binds the parser to `index.schema()` — the schema PERSISTED ON DISK, not
/// whatever `build_schema` composes in memory today (query_parser.rs:279).
/// Handing a query a `Field` minted from the in-memory schema is therefore
/// only safe while the two schemas agree ordinal-for-ordinal. They stop
/// agreeing the moment a field is added and an older index is still on disk,
/// and the failure is an index-out-of-bounds panic that takes down the
/// synchronous stdio request with no error surfaced to the caller.
///
/// Resolving by NAME makes the schema genuinely additive instead: a field this
/// binary knows but the index lacks reads as absent, and querying degrades to
/// the fields that index really has. That was chosen over a schema-version
/// stamp with rebuild-or-reject, because rebuild-or-reject makes every field
/// addition a hard breaking change for every existing deployment — while the
/// only thing an old index is actually missing here is doc bodies it never
/// contained, over which the honest answer is "this index has no doc content",
/// not "this index is unreadable". Callers that need to know which it is ask
/// [`indexes_doc_bodies`].
pub(super) struct OpenedFields {
    pub(super) qualified_name: Field,
    pub(super) name: Field,
    pub(super) label: Field,
    pub(super) file_path: Field,
    /// `None` for an index built before fleet-watch#112 added the field.
    pub(super) body: Option<Field>,
}

impl OpenedFields {
    pub(super) fn resolve(schema: &Schema) -> Result<Self, String> {
        let required = |name: &str| {
            schema
                .get_field(name)
                .map_err(|_| format!("bm25 index schema is missing the '{name}' field"))
        };
        Ok(OpenedFields {
            qualified_name: required(F_QUALIFIED_NAME)?,
            name: required(F_NAME)?,
            label: required(F_LABEL)?,
            file_path: required(F_FILE_PATH)?,
            body: schema.get_field(F_BODY).ok(),
        })
    }

    /// The fields an unqualified query term is matched against, in the order
    /// the parser receives them. `body` joins only when the opened index
    /// actually has it.
    pub(super) fn default_query_fields(&self) -> Vec<Field> {
        let mut fields = vec![self.name, self.qualified_name];
        fields.extend(self.body);
        fields
    }
}

/// Whether the BM25 index at `index_dir` carries doc/prose file bodies.
///
/// `false` for a directory holding no index at all, and for one built before
/// fleet-watch#112 — in both cases a doc-content query cannot be served, and
/// the caller must say so rather than answer with an empty result. Opening the
/// index reads only its `meta.json`, and this runs only when a caller actually
/// asks for doc content.
pub fn indexes_doc_bodies(index_dir: &Path) -> bool {
    if !index_dir.exists() {
        return false;
    }
    Index::open_in_dir(index_dir)
        .map(|index| index.schema().get_field(F_BODY).is_ok())
        .unwrap_or(false)
}

/// Writes a BM25 index carrying the PRE-fleet-watch#112 schema — the four
/// fields, no `body`, and the tokenized `qualified_name` that revision stored.
///
/// This is the only way to get a genuinely old index in a test: `build_index`
/// always writes today's schema, so nothing else in the suite can exercise the
/// "index built by the old binary, queried by the new one" path that CI never
/// covers. Kept beside the schema it mimics so the two are read together.
#[cfg(test)]
pub(crate) fn build_legacy_index(index_dir: &Path) {
    let mut builder = Schema::builder();
    let qualified_name = builder.add_text_field(F_QUALIFIED_NAME, TEXT | STORED);
    let name = builder.add_text_field(F_NAME, TEXT | STORED);
    let label = builder.add_text_field(F_LABEL, STORED);
    let file_path = builder.add_text_field(F_FILE_PATH, STORED);
    let schema = builder.build();

    std::fs::create_dir_all(index_dir).expect("create legacy index dir");
    let index = Index::create_in_dir(index_dir, schema).expect("create legacy index");
    let mut writer: IndexWriter = index.writer(15_000_000).expect("legacy writer");
    let qn = "src/main.rs::handle_tool_call";
    writer
        .add_document(doc!(
            qualified_name => qn.to_string(),
            name => tokenize_symbol("handle_tool_call"),
            label => "Function".to_string(),
            file_path => file_path_of(qn).to_string(),
        ))
        .expect("legacy add doc");
    writer.commit().expect("legacy commit");
}
