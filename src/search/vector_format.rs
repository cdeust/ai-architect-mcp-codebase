// search::vector_format — the sparse binary on-disk format for the TF-IDF
// index.
//
// Split from `search/vector.rs`, which was over the §4.1 cap. Reading and
// writing the format is a concern of its own: the two halves must agree field
// for field, and keeping them adjacent is what makes a mismatch visible.

use super::vector::{VectorDoc, VectorIndex, VECTOR_MAGIC, VECTOR_VERSION};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

// ---------------------------------------------------------------------------
// Persistence — sparse binary format
// ---------------------------------------------------------------------------

pub(super) fn save_index(index: &VectorIndex, dir: &Path) -> Result<(), String> {
    let path = dir.join("vector_index.bin");
    let file = std::fs::File::create(&path).map_err(|e| format!("create vector index: {e}"))?;
    let mut w = BufWriter::new(file);

    write_u32(&mut w, VECTOR_MAGIC)?;
    write_u32(&mut w, VECTOR_VERSION)?;
    write_u32(&mut w, index.vocabulary.len() as u32)?;
    write_u32(&mut w, index.docs.len() as u32)?;

    for term in &index.vocabulary {
        write_string(&mut w, term)?;
    }
    for doc in &index.docs {
        write_doc(&mut w, doc)?;
    }
    w.flush().map_err(|e| format!("flush vector index: {e}"))?;
    Ok(())
}

fn write_doc<W: Write>(w: &mut W, doc: &VectorDoc) -> Result<(), String> {
    write_string(w, &doc.qualified_name)?;
    write_string(w, &doc.name)?;
    write_string(w, &doc.label)?;
    write_string(w, &doc.file_path)?;
    write_u32(w, doc.terms.len() as u32)?;
    for (id, weight) in &doc.terms {
        write_u32(w, *id)?;
        write_f32(w, *weight)?;
    }
    Ok(())
}

pub(super) fn load_index(dir: &Path) -> Result<VectorIndex, String> {
    let path = dir.join("vector_index.bin");
    let file = std::fs::File::open(&path).map_err(|e| format!("open vector index: {e}"))?;
    let mut r = BufReader::new(file);

    let magic = read_u32(&mut r)?;
    if magic != VECTOR_MAGIC {
        return Err(format!("bad vector index magic: 0x{magic:08x}"));
    }
    let version = read_u32(&mut r)?;
    if version != VECTOR_VERSION {
        return Err(format!("unsupported vector index version: {version}"));
    }
    let vocab_size = read_u32(&mut r)? as usize;
    let doc_count = read_u32(&mut r)? as usize;

    let mut vocabulary = Vec::with_capacity(vocab_size);
    for _ in 0..vocab_size {
        vocabulary.push(read_string(&mut r)?);
    }

    let mut docs = Vec::with_capacity(doc_count);
    for _ in 0..doc_count {
        docs.push(read_doc(&mut r)?);
    }
    Ok(VectorIndex { docs, vocabulary })
}

fn read_doc<R: Read>(r: &mut R) -> Result<VectorDoc, String> {
    let qualified_name = read_string(r)?;
    let name = read_string(r)?;
    let label = read_string(r)?;
    let file_path = read_string(r)?;
    let nnz = read_u32(r)? as usize;
    let mut terms = Vec::with_capacity(nnz);
    for _ in 0..nnz {
        let id = read_u32(r)?;
        let w = read_f32(r)?;
        terms.push((id, w));
    }
    Ok(VectorDoc {
        qualified_name,
        name,
        label,
        file_path,
        terms,
    })
}

// ---------------------------------------------------------------------------
// Little-endian IO primitives (no external crate needed).
// ---------------------------------------------------------------------------

fn write_u32<W: Write>(w: &mut W, v: u32) -> Result<(), String> {
    w.write_all(&v.to_le_bytes())
        .map_err(|e| format!("write u32: {e}"))
}

fn write_f32<W: Write>(w: &mut W, v: f32) -> Result<(), String> {
    w.write_all(&v.to_le_bytes())
        .map_err(|e| format!("write f32: {e}"))
}

fn write_string<W: Write>(w: &mut W, s: &str) -> Result<(), String> {
    let bytes = s.as_bytes();
    write_u32(w, bytes.len() as u32)?;
    w.write_all(bytes).map_err(|e| format!("write string: {e}"))
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32, String> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(|e| format!("read u32: {e}"))?;
    Ok(u32::from_le_bytes(b))
}

fn read_f32<R: Read>(r: &mut R) -> Result<f32, String> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(|e| format!("read f32: {e}"))?;
    Ok(f32::from_le_bytes(b))
}

fn read_string<R: Read>(r: &mut R) -> Result<String, String> {
    let len = read_u32(r)? as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)
        .map_err(|e| format!("read string: {e}"))?;
    String::from_utf8(buf).map_err(|e| format!("utf8 string: {e}"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------
