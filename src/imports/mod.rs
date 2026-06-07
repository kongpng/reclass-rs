//! Import / export — C/C++ source parse, ReClass XML in/out, PDB import, and PE
//! debug-directory extraction.
//!
//! Faithful 1:1 port of `src/imports/*` (`import_source.cpp`,
//! `import_reclass_xml.cpp`, `export_reclass_xml.cpp`, `pe_debug_info.cpp`,
//! `import_pdb.cpp`). Gated behind the `imports` feature.
//!
//! > Note: RCX native JSON (`NodeTree::to_json`/`from_json`) lives in `core`
//! > (it is `core.h`, not `imports/`). C/C++ *export* (`renderCpp`) lives in
//! > `generator`. This module only does the *import* direction for C/C++
//! > source, plus the XML in/out and PDB/PE importers.

use std::path::Path;

use crate::core::kind::NodeKind;
use crate::core::NodeTree;
use crate::provider::Provider;

mod pdb;
mod pe_debug_info;
mod reclass_xml;
mod source;

pub use pdb::{PdbSymbol, PdbSymbolResult, PdbTypeInfo, ProgressCb};
pub use pe_debug_info::PdbDebugInfo;

/// Error type for all importers/exporters.
///
/// The C++ public fns take a `QString* errorMsg` (optional out-param) and
/// return an **empty `NodeTree` / `false` / `{}` on failure**. The C++ tests
/// only check `!err.isEmpty()`, so the exact message is not asserted — but the
/// *failure conditions* are identical (BMAP §6).
#[derive(Debug, Clone, thiserror::Error)]
pub enum ImportError {
    #[error("Empty source code")]
    EmptySource,
    #[error("No struct or enum definitions found")]
    NoDefinitions,
    #[error("No nodes generated from source")]
    NoNodes,
    #[error("Cannot open file: {0}")]
    CannotOpen(String),
    #[error("Cannot open file for writing: {0}")]
    CannotOpenWrite(String),
    #[error("XML parse error at line {line}: {msg}")]
    XmlParse { line: u64, msg: String },
    #[error("No classes found in file")]
    NoClasses,
    #[error("No nodes to export")]
    NoNodesToExport,
    #[error("No struct classes found to export")]
    NoClassesToExport,
    // PDB
    #[error("PDB file not found")]
    PdbNotFound,
    #[error("Failed to memory-map PDB")]
    PdbMapFailed,
    #[error("Invalid PDB file")]
    PdbInvalid,
    #[error("PDB has no valid TPI stream")]
    PdbNoTpi,
    #[error("PDB has no valid DBI stream")]
    PdbNoDbi,
    #[error("No types imported")]
    PdbNoTypesImported,
    #[error("Type '{0}' not found in PDB")]
    PdbTypeNotFound(String),
    #[error("No types found in PDB")]
    PdbNoTypes,
    #[error("Symbol has no associated type")]
    SymbolNoType,
    #[error("Type at index {0} resolves to a primitive")]
    ResolvesToPrimitive(u32),
    #[error("Failed to import type at index {0}")]
    FailedImportType(u32),
    #[error("PDB error: {0}")]
    Pdb(String),
    #[error("IO error: {0}")]
    Io(String),
}

/// Deferred name→id reference resolution (`PendingRef`, shared by `source.rs`
/// and `reclass_xml.rs`; cpp: `import_source.cpp:306`, `import_reclass_xml.cpp:137`).
///
/// Nodes that reference a class/struct/enum by **name** during building push a
/// `PendingRef`; a post-pass sets `ref_id`.
#[derive(Clone, Debug)]
pub(crate) struct PendingRef {
    pub node_id: u64,
    pub class_name: String,
}

/// The shared resolution post-pass (identical in both importers).
///
/// Unresolved refs leave `ref_id = 0` (do NOT error).
pub(crate) fn resolve_pending_refs(
    tree: &mut NodeTree,
    pending: &[PendingRef],
    class_ids: &std::collections::HashMap<String, u64>,
) {
    for r in pending {
        let idx = tree.index_of_id(r.node_id);
        if idx < 0 {
            continue;
        }
        if let Some(&id) = class_ids.get(&r.class_name) {
            tree.nodes[idx as usize].ref_id = id;
        }
    }
}

/// Pick the largest evenly-dividing hex cell `(kind, cell_size)` for a `size`-byte
/// run, walking the divisor ladder Hex64/32/16/8 (shared by `source.rs`'s
/// `emit_hex_padding` and `reclass_xml.rs`'s custom-type expansion).
pub(crate) fn largest_hex_cell_for_run(size: i32) -> (NodeKind, i32) {
    if size >= 8 && size % 8 == 0 {
        (NodeKind::Hex64, 8)
    } else if size >= 4 && size % 4 == 0 {
        (NodeKind::Hex32, 4)
    } else if size >= 2 && size % 2 == 0 {
        (NodeKind::Hex16, 2)
    } else {
        (NodeKind::Hex8, 1)
    }
}

// ── Public API (mirrors the C++ signatures, idiomatic Rust errors) ──

/// `NodeTree importFromSource(const QString&, QString*, int pointerSize=8)`
/// (`import_source.cpp:1509`).
pub fn import_from_source(source: &str, pointer_size: i32) -> Result<NodeTree, ImportError> {
    source::import_from_source(source, pointer_size)
}

/// `NodeTree importReclassXml(const QString& path, QString*, int ptrSize=8)`
/// (`import_reclass_xml.cpp:142`).
pub fn import_reclass_xml(path: &Path, pointer_size: i32) -> Result<NodeTree, ImportError> {
    reclass_xml::import_reclass_xml(path, pointer_size)
}

/// `bool exportReclassXml(const NodeTree&, const QString& path, QString*)`
/// (`export_reclass_xml.cpp:63`).
pub fn export_reclass_xml(tree: &NodeTree, path: &Path) -> Result<(), ImportError> {
    reclass_xml::export_reclass_xml(tree, path)
}

/// `PdbDebugInfo extractPdbDebugInfo(const Provider&, uint64_t moduleBase)`
/// (`pe_debug_info.cpp:85`).
pub fn extract_pdb_debug_info(prov: &dyn Provider, module_base: u64) -> PdbDebugInfo {
    pe_debug_info::extract_pdb_debug_info(prov, module_base)
}

/// `PdbSymbolResult extractPdbSymbols(const QString& pdbPath, QString*)`
/// (`import_pdb.cpp:948`).
pub fn extract_pdb_symbols(path: &Path) -> Result<PdbSymbolResult, ImportError> {
    pdb::extract_pdb_symbols(path)
}

/// `QVector<PdbTypeInfo> enumeratePdbTypes(const QString& pdbPath, QString*)`
/// (`import_pdb.cpp:1065`).
pub fn enumerate_pdb_types(path: &Path) -> Result<Vec<PdbTypeInfo>, ImportError> {
    pdb::enumerate_pdb_types(path)
}

/// `NodeTree importPdbSelected(const QString&, const QVector<uint32_t>&, QString*, ProgressCb)`
/// (`import_pdb.cpp:1164`).
pub fn import_pdb_selected(
    path: &Path,
    type_indices: &[u32],
    progress: Option<&mut ProgressCb>,
) -> Result<NodeTree, ImportError> {
    pdb::import_pdb_selected(path, type_indices, progress)
}

/// `NodeTree importPdb(const QString&, const QString& structFilter, QString*)`
/// (`import_pdb.cpp:1211`). `struct_filter == ""` means import all.
pub fn import_pdb(path: &Path, struct_filter: &str) -> Result<NodeTree, ImportError> {
    pdb::import_pdb(path, struct_filter)
}

/// `NodeTree importTypeForSymbol(const QString&, uint32_t, QString*, QString*)`
/// (`import_pdb.cpp:1266`).
pub fn import_type_for_symbol(
    path: &Path,
    type_index: u32,
    type_name_out: &mut String,
) -> Result<NodeTree, ImportError> {
    pdb::import_type_for_symbol(path, type_index, type_name_out)
}
