//! Import / export — `.rcx` JSON, C/C++ source parse, ReClass XML in/out, PDB
//! import, and PE debug-directory extraction.
//!
//! Port of `src/imports/*`. **SKELETON** — the source tokenizer/parser, the XML
//! reader/writer, PDB import, and the PE-over-`Provider` decode are filled in by
//! the dedicated `imports` workflow (ARCHITECTURE.md §9). Gated behind the
//! `imports` feature.

use crate::core::NodeTree;
use crate::provider::Provider;

/// `importFromSource(sourceCode, &err, pointerSize=8)` (`import_source.h:13-14`)
/// — parse C/C++ struct definitions into a [`NodeTree`]. SKELETON.
pub fn import_from_source(_source_code: &str, _pointer_size: i32) -> Result<NodeTree, String> {
    todo!("port import_source.cpp (workflow: imports)")
}

/// `importReclassXml(filePath, &err, pointerSize=8)` (`import_reclass_xml.h:9-10`)
/// — parse a ReClass .NET / ReClassEx XML file. SKELETON.
pub fn import_reclass_xml(_file_path: &str, _pointer_size: i32) -> Result<NodeTree, String> {
    todo!("port import_reclass_xml.cpp (workflow: imports)")
}

/// `exportReclassXml(tree, filePath, &err)` (`export_reclass_xml.h:8`). SKELETON.
pub fn export_reclass_xml(_tree: &NodeTree, _file_path: &str) -> Result<(), String> {
    todo!("port export_reclass_xml.cpp (workflow: imports)")
}

/// `struct PdbSymbol` (`import_pdb.h:11-15`).
#[derive(Clone, Debug, Default)]
pub struct PdbSymbol {
    pub name: String,
    pub rva: u32,
    /// TPI type index (0 = unknown / public symbol).
    pub type_index: u32,
}

/// `struct PdbSymbolResult` (`import_pdb.h:17-20`).
#[derive(Clone, Debug, Default)]
pub struct PdbSymbolResult {
    /// derived from PDB filename (e.g. "ntoskrnl").
    pub module_name: String,
    pub symbols: Vec<PdbSymbol>,
}

/// `extractPdbSymbols(pdbPath, &err)` (`import_pdb.h:24-25`) — public/global
/// symbols (name → RVA) from a PDB. SKELETON.
pub fn extract_pdb_symbols(_pdb_path: &str) -> Result<PdbSymbolResult, String> {
    todo!("port import_pdb.cpp extractPdbSymbols via pdb2 (workflow: imports)")
}

/// `struct PdbDebugInfo` (`pe_debug_info.h:9-14`).
#[derive(Clone, Debug, Default)]
pub struct PdbDebugInfo {
    /// e.g. "ntoskrnl.pdb".
    pub pdb_name: String,
    /// 32 hex chars, no dashes, uppercase.
    pub guid_string: String,
    pub age: u32,
    pub valid: bool,
}

/// `extractPdbDebugInfo(prov, moduleBase)` (`pe_debug_info.h:18`) — hand-decode
/// DOS → PE → debug directory → CodeView RSDS over the abstract `Provider`.
/// SKELETON.
pub fn extract_pdb_debug_info(_prov: &dyn Provider, _module_base: u64) -> PdbDebugInfo {
    todo!("port pe_debug_info.cpp extractPdbDebugInfo (workflow: imports)")
}
