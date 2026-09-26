// graph_store::cfg_active::file_scope: the `#[cfg]` facts of a source FILE
// (issue #366, part B).
//
// A `mod` declaration can carry a gate (`#[cfg(unix)] #[path = "unix.rs"] mod
// imp;`), and every item of the file it selects exists only under that gate,
// though no item id says so. Three `File` columns carry what the module walk
// learned (`indexer::file_scope`): `cfg_gate`, the compact conjunction of the
// gates on the declarations that lead to the file; `module_path`, the logical
// module path those declarations give it (so two files one declaration picks
// between hold twins of one item); `cfg_active`, whether the default build
// compiles the file. Only files under a gate, or compiled out, get a row; `''`
// everywhere else and on a graph written before the columns, which every reader
// treats as "nothing known about the file".

use std::collections::{BTreeMap, HashMap};

use super::super::{cypher_str, GraphStore, BULK_BATCH_SIZE};
use super::{normalize, ACTIVE_COLUMN_TYPE};
use lbug::{LogicalType, Value};

const FILE_CFG_COLUMNS: [&str; 3] = ["cfg_gate", "cfg_active", "module_path"];

/// The `#[cfg]` facts of one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileCfg {
    /// Logical module path (`src/lib.rs::imp`), `''` when the file is reached
    /// in more than one way.
    pub module_path: String,
    /// Compact conjunction of the gates leading to the file, `''` for none,
    /// `?` when one of them did not parse.
    pub gate: String,
    /// `active` / `inactive` / `unknown` under the default build.
    pub active: String,
}

impl GraphStore {
    /// Adds the three columns to `File` on a graph written before they existed.
    pub fn ensure_file_cfg_columns(&self) -> Result<(), String> {
        for column in FILE_CFG_COLUMNS {
            self.ensure_node_column("File", column, ACTIVE_COLUMN_TYPE)?;
        }
        Ok(())
    }

    /// Rewrites the three columns of every file: the values of `facts` (file id
    /// to facts), `''` for every other file.
    pub fn write_file_cfg(&self, facts: &BTreeMap<String, FileCfg>) -> Result<(), String> {
        self.ensure_file_cfg_columns()?;
        self.run("MATCH (f:File) SET f.cfg_gate = '', f.cfg_active = '', f.module_path = ''")?;
        let mut by_active: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (file, cfg) in facts {
            by_active.entry(cfg.active.as_str()).or_default().push(file);
            if cfg.gate.is_empty() && cfg.module_path.is_empty() {
                continue;
            }
            self.run(&format!(
                "MATCH (f:File {{id: {}}}) SET f.cfg_gate = {}, f.module_path = {}",
                cypher_str(file),
                cypher_str(&cfg.gate),
                cypher_str(&cfg.module_path)
            ))?;
        }
        for (value, files) in by_active {
            let cypher = format!(
                "UNWIND $rows AS rid MATCH (f:File {{id: rid}}) SET f.cfg_active = {}",
                cypher_str(&normalize(value))
            );
            for chunk in files.chunks(BULK_BATCH_SIZE) {
                let ids = chunk
                    .iter()
                    .map(|id| Value::String((*id).to_string()))
                    .collect();
                self.run_prepared(&cypher, Value::List(LogicalType::String, ids))?;
            }
        }
        Ok(())
    }

    /// The facts of every file that has some, by file id. Empty on a graph
    /// without the columns. Read-only.
    pub fn file_cfg_by_id(&self) -> HashMap<String, FileCfg> {
        let has_columns = FILE_CFG_COLUMNS
            .iter()
            .all(|c| self.node_column_exists("File", c).unwrap_or(false));
        if !has_columns {
            return HashMap::new();
        }
        let Ok(rows) = self.execute_query(
            "MATCH (f:File) WHERE f.cfg_gate <> '' OR f.cfg_active = 'inactive' \
             RETURN f.id, f.module_path, f.cfg_gate, f.cfg_active",
        ) else {
            return HashMap::new();
        };
        rows.rows
            .into_iter()
            .map(|r| {
                let cfg = FileCfg {
                    module_path: r[1].clone(),
                    gate: r[2].clone(),
                    active: normalize(&r[3]),
                };
                (r[0].clone(), cfg)
            })
            .collect()
    }
}
