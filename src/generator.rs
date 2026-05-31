//! Source-code emission from the node tree (C/C++, Rust, #define offsets, C#,
//! Python ctypes).
//!
//! Port of `src/generator.{h,cpp}`. **SKELETON** — the per-backend renderers are
//! filled in by the dedicated `generator` workflow (ARCHITECTURE.md §9). The
//! `CodeFormat`/`CodeScope` enums + the public dispatch signatures are in place.

use std::collections::HashMap;

use crate::core::{NodeKind, NodeTree};

/// `enum class CodeFormat : int` (`generator.h:11-18`).
#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CodeFormat {
    CppHeader = 0,
    RustStruct,
    DefineOffsets,
    CSharpStruct,
    PythonCtypes,
}

/// `enum class CodeScope : int` (`generator.h:20-25`).
#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CodeScope {
    /// just the selected struct.
    Current = 0,
    /// selected struct + all referenced types.
    WithChildren,
    /// all root-level structs.
    FullSdk,
}

/// Per-`NodeKind` display-name overrides passed to the renderers.
pub type TypeAliases = HashMap<NodeKind, String>;

/// `codeFormatName(fmt)` (`generator.h:27`). SKELETON.
pub fn code_format_name(_fmt: CodeFormat) -> &'static str {
    todo!("port generator.cpp codeFormatName (workflow: generator)")
}

/// `codeScopeName(scope)` (`generator.h:29`). SKELETON.
pub fn code_scope_name(_scope: CodeScope) -> &'static str {
    todo!("port generator.cpp codeScopeName (workflow: generator)")
}

/// `renderCode(fmt, tree, rootStructId, typeAliases, emitAsserts)`
/// (`generator.h:33-35`) — format-aware dispatch. SKELETON.
pub fn render_code(
    _fmt: CodeFormat,
    _tree: &NodeTree,
    _root_struct_id: u64,
    _type_aliases: Option<&TypeAliases>,
    _emit_asserts: bool,
) -> String {
    todo!("port generator.cpp renderCode (workflow: generator)")
}

/// `renderCodeTree(...)` (`generator.h:38-40`). SKELETON.
pub fn render_code_tree(
    _fmt: CodeFormat,
    _tree: &NodeTree,
    _root_struct_id: u64,
    _type_aliases: Option<&TypeAliases>,
    _emit_asserts: bool,
) -> String {
    todo!("port generator.cpp renderCodeTree (workflow: generator)")
}

/// `renderCodeAll(...)` (`generator.h:42-44`). SKELETON.
pub fn render_code_all(
    _fmt: CodeFormat,
    _tree: &NodeTree,
    _type_aliases: Option<&TypeAliases>,
    _emit_asserts: bool,
) -> String {
    todo!("port generator.cpp renderCodeAll (workflow: generator)")
}
