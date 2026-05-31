//! x86/x64 disassembly + hex dump (replaces fadec with `iced-x86`).
//!
//! Port of `src/disasm.{h,cpp}`. **SKELETON** — the `iced-x86` decode/format
//! loop and the hex-dump formatter are filled in by the dedicated `disasm`
//! workflow (ARCHITECTURE.md §9). Gated behind the `disasm` feature.

/// `disassemble(bytes, baseAddr, bitness, maxBytes=128)` (`disasm.h:9`) — one
/// formatted asm line per instruction, each prefixed with its offset.
/// `bitness` is 32 or 64. SKELETON.
pub fn disassemble(_bytes: &[u8], _base_addr: u64, _bitness: i32, _max_bytes: i32) -> String {
    todo!("port disasm.cpp disassemble via iced-x86 (workflow: disasm)")
}

/// `hexDump(bytes, baseAddr, maxBytes=128)` (`disasm.h:13`) — 16 bytes per line
/// with an ASCII sidebar. SKELETON.
pub fn hex_dump(_bytes: &[u8], _base_addr: u64, _max_bytes: i32) -> String {
    todo!("port disasm.cpp hexDump (workflow: disasm)")
}
