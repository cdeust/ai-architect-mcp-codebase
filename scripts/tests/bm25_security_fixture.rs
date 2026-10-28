// source: actual BM25 writer/schema/query contract in src/search/bm25.rs.
// The 50_000_000-byte writer budget is copied from build_index, not tuned here.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let path = Path::new(&args[2]);
    if args[1] == "create" {
        std::fs::create_dir_all(path)?;
        let (layout, fields) = schema::build_schema();
        let index = Index::create_in_dir(path, layout)?;
        let mut writer: IndexWriter = index.writer(50_000_000)?;
        for (key, name, body) in [
            (
                "src/server.rs::handle_tool_call",
                "handle tool call",
                "dispatch native tool requests",
            ),
            (
                "src/store.rs::GraphStore",
                "graph store",
                "persistent graph symbol database",
            ),
            (
                "README.md",
                "readme",
                "tool call usage and graph store setup",
            ),
        ] {
            writer.add_document(
                doc!(fields.qualified_name=>key,fields.name=>name,fields.body=>body,
      fields.label=>"Function",fields.file_path=>key),
            )?;
        }
        writer.commit()?;
    }
    assert!(schema::indexes_doc_bodies(path));
    for query in ["handle_tool_call", "GraphStore", "native tool requests"] {
        let hits = query_index(path, query, 3)?;
        println!(
            "{}|{}",
            query,
            hits.iter()
                .map(|h| format!("{}:{}", h.qualified_name, h.score.to_bits()))
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    snapshot_roundtrip(&path.with_extension("snapshot"))?;
    Ok(())
}

// source: artifact.rs uses level3 for watcher snapshots and level9 for explicit export.
fn snapshot_roundtrip(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let graph = root.join("graph");
    fs::create_dir_all(&graph)?;
    let payload = b"graph snapshot compatibility fixture";
    fs::write(graph.join("nodes.bin"), payload)?;
    let sidecar = root.join("manifest.json");
    fs::write(&sidecar, b"{\"version\":1}")?;
    for level in [3, 9] {
        let packed = root.join(format!("snapshot-{level}.tar.zst"));
        write_archive(
            &graph,
            &[(sidecar.clone(), "manifest.json")],
            &packed,
            level,
        )?;
        let restored = root.join(format!("restored-{level}"));
        fs::create_dir_all(&restored)?;
        let decoder = zstd::Decoder::new(File::open(packed)?)?;
        tar::Archive::new(decoder).unpack(&restored)?;
        assert_eq!(fs::read(restored.join("graph/nodes.bin"))?, payload);
        assert_eq!(
            fs::read(restored.join("manifest.json"))?,
            fs::read(&sidecar)?
        );
    }
    Ok(())
}
