//! Manifest build/read/validate (Task 7).
//!
//! The manifest shares the handshake envelope (`protocol_version` 1,
//! `spec_version` "0.4.0", `schema_version` 3, `generator`
//! cargo-obsidian-graph) and adds `generated_files` (sorted) plus
//! `module_records` (sorted by `module_id`). [`build_manifest`] recomputes
//! filenames through [`crate::render::assign_filenames`], the same mapping
//! [`crate::render::render_all`] uses, so records always match emitted notes.
//! [`read_manifest`] returns `None` when the file is missing and `Err` on
//! invalid JSON or a handshake mismatch (unknown protocol/schema must be
//! rejected by consumers).

use crate::domain::WorkspaceGraph;
use crate::render::{assign_filenames, RenderedFile};
use crate::{GENERATOR, PROTOCOL_VERSION, SCHEMA_VERSION, SPEC_VERSION};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One module's logical ID plus its generated note filename.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleRecord {
    pub module_id: String,
    pub filename: String,
}

/// Commit-record manifest: handshake envelope plus the sorted file inventory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub protocol_version: u32,
    pub spec_version: String,
    pub schema_version: u32,
    pub generator: String,
    pub generated_files: Vec<String>,
    pub module_records: Vec<ModuleRecord>,
}

/// Build the manifest for one render: `generated_files` sorted,
/// `module_records` sorted by `module_id`.
pub fn build_manifest(rendered: &[RenderedFile], graph: &WorkspaceGraph) -> Manifest {
    let mut generated_files: Vec<String> =
        rendered.iter().map(|f| f.rel_path.clone()).collect();
    generated_files.sort();
    let (module_files, _) = assign_filenames(graph);
    let mut module_records: Vec<ModuleRecord> = graph
        .modules
        .iter()
        .map(|m| ModuleRecord {
            module_id: m.id.0.clone(),
            filename: module_files.get(m.id.0.as_str()).cloned().unwrap_or_default(),
        })
        .collect();
    module_records.sort_by(|a, b| a.module_id.cmp(&b.module_id));
    Manifest {
        protocol_version: PROTOCOL_VERSION,
        spec_version: SPEC_VERSION.to_string(),
        schema_version: SCHEMA_VERSION,
        generator: GENERATOR.to_string(),
        generated_files,
        module_records,
    }
}

/// Read a manifest file: `Ok(None)` when missing, `Err` on invalid JSON or
/// handshake mismatch.
pub fn read_manifest(path: &Path) -> Result<Option<Manifest>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("invalid manifest: {e}")),
    };
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("invalid manifest: {e}"))?;
    if manifest.protocol_version != PROTOCOL_VERSION
        || manifest.spec_version != SPEC_VERSION
        || manifest.schema_version != SCHEMA_VERSION
        || manifest.generator != GENERATOR
    {
        return Err("invalid manifest: handshake mismatch".to_string());
    }
    Ok(Some(manifest))
}
