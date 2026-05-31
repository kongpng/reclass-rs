//! x86/x64 disassembly + hex dump (replaces fadec with `iced-x86`).
//!
//! Faithful 1:1 port of `src/disasm.{h,cpp}` (namespace `rcx`). Two pure,
//! stateless free functions used by the editor's read-only hover popups:
//!
//! * [`disassemble`] — x86/x64 instruction disassembly (one address-prefixed
//!   line per instruction). Replaces the C `fadec` decoder/formatter
//!   (`fd_decode`/`fd_format`) with the pure-Rust `iced-x86` crate.
//! * [`hex_dump`] — classic 16-bytes-per-line hex + ASCII dump (`std` only).
//!
//! No process memory is read here (callers read via a `Provider` and pass the
//! bytes in); no Qt widgets, no threading, no global/`static` state, no I/O,
//! no platform-specific code. Every failure mode collapses to an empty
//! `String` ("nothing to show"). Gated behind the `disasm` feature.

use std::fmt::Write as _;

use iced_x86::{
    Decoder, DecoderOptions, Formatter, Instruction, IntelFormatter, MemorySizeOptions,
};

/// `QString disassemble(const QByteArray& bytes, uint64_t baseAddr, int bitness, int maxBytes = 128)`
/// (`disasm.h:10`, body `disasm.cpp:9-36`).
///
/// Disassembles up to `max_bytes` of x86 code starting at virtual address
/// `base_addr`, producing one newline-joined line per fully-decoded
/// instruction:
///
/// ```text
/// <zero-padded hex address>  <formatted instruction>
/// ```
///
/// The address field is `16` hex digits for 64-bit code and `8` for 32-bit
/// (bitness-dependent, *not* value-dependent), followed by exactly two spaces.
/// Each line uses the instruction's own absolute address (`base_addr + off`),
/// which is also fed to the decoder so RIP-relative / relative-branch targets
/// resolve to absolute addresses.
///
/// Returns an empty `String` if `bytes` is empty, if `bitness` is not exactly
/// `32` or `64`, or if the very first instruction fails to decode. A
/// mid-stream decode failure stops decoding and returns the prefix decoded so
/// far (partial, never an error) — mirroring fadec returning a negative value
/// for a truncated/invalid instruction.
pub fn disassemble(bytes: &[u8], base_addr: u64, bitness: i32, max_bytes: i32) -> String {
    // disasm.cpp:10-11 — guard: empty input OR bitness not exactly 32/64.
    if bytes.is_empty() || (bitness != 32 && bitness != 64) {
        return String::new();
    }

    // disasm.cpp:13 — window length = min(size, maxBytes). Clamp negative
    // maxBytes to 0 (the C++ `int` would underflow; clamp is the safe parity).
    let len = bytes.len().min(max_bytes.max(0) as usize);
    let window = &bytes[..len]; // disasm.cpp:14 — buffer pointer.

    // disasm.cpp:20 — `fd_decode(buf+off, len-off, bitness, baseAddr+off, &instr)`.
    // `with_ip` sets the starting IP so `instr.ip() == base_addr + off`; the
    // decoder advances the IP/position internally as instructions are decoded.
    let mut decoder = Decoder::with_ip(bitness as u32, window, base_addr, DecoderOptions::NONE);

    // disasm.cpp:25 — `fd_format(...)` → Intel syntax, lowercase. Configure the
    // iced `IntelFormatter` to emit byte-identical text to fadec (the #1
    // fidelity requirement; see the pinned mnemonics in the unit tests).
    let mut formatter = IntelFormatter::new();
    {
        let o = formatter.options_mut();
        o.set_uppercase_hex(false); // 0x20 / 0x10 lowercase
        o.set_hex_prefix("0x"); // `0x` prefix, not `h` suffix
        o.set_hex_suffix("");
        o.set_leading_zeroes(false); // 0x8/0x10/0x1105, not 0x08/0x0000…
        o.set_branch_leading_zeroes(false); // call 0x1105 / jmp 0x1012
        o.set_small_hex_numbers_in_decimal(false); // `[rsp+0x8]`, not `[rsp+8]`
        o.set_show_branch_size(false); // `jmp 0x1012`, not `jmp short 0x1012`
        o.set_rip_relative_addresses(true); // keep `[rip+0x10]` symbolic (true ⇒ rip-relative form)
        o.set_memory_size_options(MemorySizeOptions::Always); // always `qword ptr`
        o.set_space_after_operand_separator(true); // `xor eax, eax` (iced's IntelFormatter
                                                   // default is `eax,eax` — fadec inserts the space).
                                                   // uppercase_mnemonics / uppercase_registers:
                                                   // iced defaults already match (lowercase).
    }

    let mut result = String::new(); // disasm.cpp:16
    let mut instr = Instruction::default(); // reuse to avoid per-iter alloc
    let mut text = String::new(); // analogue of `char fmtBuf[128]`
    let w = if bitness == 64 { 16 } else { 8 };

    while decoder.can_decode() {
        // disasm.cpp:18
        decoder.decode_out(&mut instr); // disasm.cpp:20
        if instr.is_invalid() {
            // disasm.cpp:21-22 — fadec `ret < 0`: STOP entirely (break, never
            // continue). iced would otherwise keep going past an invalid insn.
            break;
        }

        text.clear();
        formatter.format(&instr, &mut text); // disasm.cpp:24-25

        if !result.is_empty() {
            // disasm.cpp:27-28 — join with '\n' (no trailing newline).
            result.push('\n');
        }
        // disasm.cpp:29-31 — `"%1  %2"` with the bitness-dependent zero-padded
        // address width and exactly TWO spaces between address and mnemonic.
        let _ = write!(result, "{:0w$x}  {}", instr.ip(), text);
        // disasm.cpp:33 — `off += ret` is implicit (decoder advanced the IP).
    }
    result // disasm.cpp:35
}

/// `QString hexDump(const QByteArray& bytes, uint64_t baseAddr, int maxBytes = 128)`
/// (`disasm.h:13`, body `disasm.cpp:38-74`).
///
/// Formats up to `max_bytes` of `bytes` as a classic hex dump, 16 bytes per
/// row, with an ASCII sidebar. Each row:
///
/// ```text
/// <addr><2sp> [hh' ' ×8] <1sp extra> [hh' ' ×8] <1sp> <up to 16 ascii chars>
/// ```
///
/// The address width is `16` hex digits iff `base_addr + len > 0xFFFF_FFFF`
/// (decided once for the whole dump from the *end* of the dumped range), else
/// `8`. Missing hex slots on a short final row emit three spaces to keep the
/// ASCII column aligned; the ASCII side has no padding. Printable bytes are
/// `0x20..0x7f` (space through `~`); everything else renders as `.`. Returns
/// an empty `String` for empty input. No decoder dependency (`std` only).
pub fn hex_dump(bytes: &[u8], base_addr: u64, max_bytes: i32) -> String {
    // disasm.cpp:39-40 — guard: empty input → empty (no bitness/validity check).
    if bytes.is_empty() {
        return String::new();
    }

    // disasm.cpp:42 — window length = min(size, maxBytes); clamp negative to 0.
    let len = bytes.len().min(max_bytes.max(0) as usize);

    // disasm.cpp:52 — `wide` from `baseAddr + len` (loop-invariant in C++, so
    // compute once before the loop here).
    let wide = base_addr + (len as u64) > 0xFFFF_FFFF;
    let aw = if wide { 16 } else { 8 };

    let mut result = String::new(); // disasm.cpp:43
    let mut off = 0usize;
    while off < len {
        // disasm.cpp:45
        let line_len = 16.min(len - off); // disasm.cpp:46

        if !result.is_empty() {
            // disasm.cpp:48-49
            result.push('\n');
        }

        // disasm.cpp:53 — address column: `"%1  "` (addr + two spaces).
        let _ = write!(result, "{:0aw$x}  ", base_addr + off as u64);

        // disasm.cpp:56-64 — exactly 16 hex slots.
        for i in 0..16 {
            if i < line_len {
                // disasm.cpp:57-59 — byte → 2 lowercase hex digits + 1 space.
                let _ = write!(result, "{:02x} ", bytes[off + i]);
            } else {
                // disasm.cpp:60-61 — padding placeholder: 3 spaces.
                result.push_str("   ");
            }
            if i == 7 {
                // disasm.cpp:63 — extra middle gap after the 8th byte, EVERY row.
                result.push(' ');
            }
        }

        // disasm.cpp:67 — one space before the ASCII column.
        result.push(' ');
        // disasm.cpp:68-70 — ASCII for real bytes only (no padding on the side).
        for i in 0..line_len {
            let b = bytes[off + i];
            result.push(if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '.'
            });
        }

        off += 16;
    }
    result // disasm.cpp:73
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{BufferProvider, Provider};

    /// Test-local helper mirroring `test_disasm.cpp:9-12`: split each line on
    /// the first `"  "` and take everything after it.
    fn mnemonic(line: &str) -> &str {
        match line.find("  ") {
            Some(sep) => &line[sep + 2..],
            None => line,
        }
    }

    fn split_lines(s: &str) -> Vec<&str> {
        s.split('\n').collect()
    }

    // ──────────────────────────────────────────────────
    //  disassemble() unit tests – exact mnemonic match
    // ──────────────────────────────────────────────────

    #[test]
    fn disasm64_push_mov() {
        // test_disasm.cpp:21-30
        let result = disassemble(&[0x55, 0x48, 0x89, 0xe5], 0x401000, 64, 128);
        let lines = split_lines(&result);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("0000000000401000"));
        assert!(lines[1].starts_with("0000000000401001"));
        assert_eq!(mnemonic(lines[0]), "push rbp");
        assert_eq!(mnemonic(lines[1]), "mov rbp, rsp");
    }

    #[test]
    fn disasm64_ret() {
        // test_disasm.cpp:32
        assert_eq!(mnemonic(&disassemble(&[0xc3], 0x7FF000, 64, 128)), "ret");
    }

    #[test]
    fn disasm64_nop() {
        // test_disasm.cpp:33
        assert_eq!(mnemonic(&disassemble(&[0x90], 0, 64, 128)), "nop");
    }

    #[test]
    fn disasm64_xor_eax() {
        // test_disasm.cpp:34
        assert_eq!(
            mnemonic(&disassemble(&[0x31, 0xc0], 0, 64, 128)),
            "xor eax, eax"
        );
    }

    #[test]
    fn disasm64_sub_rsp() {
        // test_disasm.cpp:35
        assert_eq!(
            mnemonic(&disassemble(&[0x48, 0x83, 0xec, 0x20], 0, 64, 128)),
            "sub rsp, 0x20"
        );
    }

    #[test]
    fn disasm64_int3() {
        // test_disasm.cpp:36
        assert_eq!(mnemonic(&disassemble(&[0xcc], 0, 64, 128)), "int3");
    }

    #[test]
    fn disasm64_push_rdi() {
        // test_disasm.cpp:37
        assert_eq!(mnemonic(&disassemble(&[0x57], 0, 64, 128)), "push rdi");
    }

    #[test]
    fn disasm64_pop_rsi() {
        // test_disasm.cpp:38
        assert_eq!(mnemonic(&disassemble(&[0x5e], 0, 64, 128)), "pop rsi");
    }

    #[test]
    fn disasm64_test_eax() {
        // test_disasm.cpp:39
        assert_eq!(
            mnemonic(&disassemble(&[0x85, 0xc0], 0, 64, 128)),
            "test eax, eax"
        );
    }

    #[test]
    fn disasm64_lea_rip_rel() {
        // test_disasm.cpp:41-44
        assert_eq!(
            mnemonic(&disassemble(
                &[0x48, 0x8d, 0x05, 0x10, 0x00, 0x00, 0x00],
                0x1000,
                64,
                128
            )),
            "lea rax, [rip+0x10]"
        );
    }

    #[test]
    fn disasm64_call_rel() {
        // test_disasm.cpp:45-49 — target = 0x1000 + 5 + 0x100 = 0x1105
        assert_eq!(
            mnemonic(&disassemble(
                &[0xe8, 0x00, 0x01, 0x00, 0x00],
                0x1000,
                64,
                128
            )),
            "call 0x1105"
        );
    }

    #[test]
    fn disasm64_jmp_rel() {
        // test_disasm.cpp:50-54 — target = 0x1000 + 2 + 0x10 = 0x1012
        assert_eq!(
            mnemonic(&disassemble(&[0xeb, 0x10], 0x1000, 64, 128)),
            "jmp 0x1012"
        );
    }

    #[test]
    fn disasm64_mov_mem_read() {
        // test_disasm.cpp:55-58
        assert_eq!(
            mnemonic(&disassemble(&[0x48, 0x8b, 0x43, 0x10], 0, 64, 128)),
            "mov rax, qword ptr [rbx+0x10]"
        );
    }

    #[test]
    fn disasm64_mov_mem_write() {
        // test_disasm.cpp:59-62
        assert_eq!(
            mnemonic(&disassemble(&[0x48, 0x89, 0x4c, 0x24, 0x08], 0, 64, 128)),
            "mov qword ptr [rsp+0x8], rcx"
        );
    }

    #[test]
    fn disasm64_function_prologue() {
        // test_disasm.cpp:64-73
        let result = disassemble(
            &[0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x20, 0xc3],
            0x140001000,
            64,
            128,
        );
        let lines = split_lines(&result);
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("0000000140001000"));
        assert_eq!(mnemonic(lines[0]), "push rbp");
        assert_eq!(mnemonic(lines[1]), "mov rbp, rsp");
        assert_eq!(mnemonic(lines[2]), "sub rsp, 0x20");
        assert_eq!(mnemonic(lines[3]), "ret");
    }

    #[test]
    fn disasm64_multiple_nops() {
        // test_disasm.cpp:75-82
        let result = disassemble(&[0x90; 5], 0x1000, 64, 128);
        let lines = split_lines(&result);
        assert_eq!(lines.len(), 5);
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(mnemonic(line), "nop");
            assert!(line.starts_with(&format!("{:016x}", 0x1000u64 + i as u64)));
        }
    }

    #[test]
    fn disasm32_push_mov() {
        // test_disasm.cpp:84-91
        let result = disassemble(&[0x55, 0x89, 0xe5], 0x401000, 32, 128);
        let lines = split_lines(&result);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("00401000"));
        assert_eq!(mnemonic(lines[0]), "push ebp");
        assert_eq!(mnemonic(lines[1]), "mov ebp, esp");
    }

    #[test]
    fn disasm_empty() {
        // test_disasm.cpp:93
        assert!(disassemble(&[], 0, 64, 128).is_empty());
        assert!(disassemble(&[], 0, 32, 128).is_empty());
    }

    #[test]
    fn disasm_invalid_bitness() {
        // test_disasm.cpp:94 (16) plus extra parity coverage (0, 48, -1).
        assert!(disassemble(&[0x90], 0, 16, 128).is_empty());
        assert!(disassemble(&[0x90], 0, 0, 128).is_empty());
        assert!(disassemble(&[0x90], 0, 48, 128).is_empty());
        assert!(disassemble(&[0x90], 0, -1, 128).is_empty());
    }

    #[test]
    fn disasm_max_bytes() {
        // test_disasm.cpp:95 — 200 nops, maxBytes=128 → exactly 128 lines.
        let result = disassemble(&[0x90; 200], 0, 64, 128);
        assert_eq!(result.matches('\n').count() + 1, 128);
    }

    #[test]
    fn disasm64_addr_width() {
        // test_disasm.cpp:96
        assert_eq!(disassemble(&[0x90], 0, 64, 128).find("  "), Some(16));
    }

    #[test]
    fn disasm32_addr_width() {
        // test_disasm.cpp:97
        assert_eq!(disassemble(&[0x90], 0, 32, 128).find("  "), Some(8));
    }

    // ──────────────────────────────────────────────────
    //  hexDump() unit tests
    // ──────────────────────────────────────────────────

    #[test]
    fn hexdump_basic() {
        // test_disasm.cpp:103-108
        let data: Vec<u8> = (0..32u8).collect();
        let r = hex_dump(&data, 0x1000, 128);
        assert_eq!(r.matches('\n').count() + 1, 2);
        assert!(r.starts_with("00001000"));
    }

    #[test]
    fn hexdump_ascii() {
        // test_disasm.cpp:109-111
        assert!(hex_dump(b"Hello, World!xx", 0, 128).contains("Hello"));
    }

    #[test]
    fn hexdump_non_printable() {
        // test_disasm.cpp:112-115 — A + 14 dots (for the 14 NULs) + Z.
        let mut d = [0u8; 16];
        d[0] = b'A';
        d[15] = b'Z';
        assert!(hex_dump(&d, 0, 128).contains("A..............Z"));
    }

    #[test]
    fn hexdump_empty() {
        // test_disasm.cpp:116
        assert!(hex_dump(&[], 0, 128).is_empty());
    }

    #[test]
    fn hexdump_max_bytes() {
        // test_disasm.cpp:117 — 200 bytes, maxBytes=64 → 4 rows.
        let r = hex_dump(&[0xAA; 200], 0, 64);
        assert_eq!(r.matches('\n').count() + 1, 4);
    }

    #[test]
    fn hexdump_wide_addr() {
        // test_disasm.cpp:118 — 0x100000000 + 16 > 0xFFFFFFFF → 16-digit addr.
        assert!(hex_dump(&[0u8; 16], 0x100000000, 128).starts_with("0000000100000000"));
    }

    #[test]
    fn hexdump_hex_values() {
        // test_disasm.cpp:119-123
        let mut d = vec![0xDE, 0xAD, 0xBE, 0xEF];
        d.resize(16, 0);
        assert!(hex_dump(&d, 0, 128).contains("de ad be ef"));
    }

    #[test]
    fn hexdump_second_line_addr() {
        // test_disasm.cpp:124-128 — 32 bytes at 0x2000 → line[1] @ 0x2010.
        let r = hex_dump(&[0x42; 32], 0x2000, 128);
        let lines = split_lines(&r);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].starts_with("00002010"));
    }

    // ──────────────────────────────────────────────────
    //  End-to-end: VTable / hover-flow (disasm-through-provider portions).
    //  The COMPOSE-dependent assertions (composed offsetAddr) require the
    //  `compose` subsystem, which is still a skeleton; those full tests are
    //  ported and #[ignore]d below. The provider+disasm core paths run here.
    // ──────────────────────────────────────────────────

    fn w64(mem: &mut [u8], off: usize, val: u64) {
        mem[off..off + 8].copy_from_slice(&val.to_le_bytes());
    }

    #[test]
    fn vtable_disasm_through_provider() {
        // Disasm-relevant core of test_disasm.cpp:135-302 (testVTableDisasm_-
        // composedAddress), exercised directly through BufferProvider (the
        // composed-address checks live in the #[ignore]d full test below).
        let mut mem = vec![0u8; 4096];
        w64(&mut mem, 0x00, 0x100); // root __vptr -> vtable
        w64(&mut mem, 0x100, 0x200); // vtable[0] -> func0
        w64(&mut mem, 0x108, 0x300); // vtable[1] -> func1
        mem[0x200] = 0x55; // func0: push rbp; ret
        mem[0x201] = 0xc3;
        mem[0x300] = 0x31; // func1: xor eax, eax; ret
        mem[0x301] = 0xc0;
        mem[0x302] = 0xc3;

        let prov = BufferProvider::new(mem, "mem");

        // func0 at vtable slot 0x100 → 0x200.
        let ptr0 = prov.read_u64(0x100);
        assert_eq!(ptr0, 0x200);
        let code0 = prov.read_bytes(ptr0, 128);
        let asm0 = disassemble(&code0, ptr0, 64, 128);
        assert!(!asm0.is_empty());
        let l0 = split_lines(&asm0);
        assert!(l0.len() >= 2);
        assert_eq!(mnemonic(l0[0]), "push rbp");
        assert_eq!(mnemonic(l0[1]), "ret");
        assert!(l0[0].contains("200"));

        // func1 at vtable slot 0x108 → 0x300.
        let ptr1 = prov.read_u64(0x108);
        assert_eq!(ptr1, 0x300);
        let code1 = prov.read_bytes(ptr1, 128);
        let asm1 = disassemble(&code1, ptr1, 64, 128);
        assert!(!asm1.is_empty());
        let l1 = split_lines(&asm1);
        assert!(l1.len() >= 2);
        assert_eq!(mnemonic(l1[0]), "xor eax, eax");
        assert_eq!(mnemonic(l1[1]), "ret");
        assert!(l1[0].contains("300"));

        // node.offset (the WRONG way) reads the vptr, not the func pointers.
        assert_eq!(prov.read_u64(0), 0x100);
        assert_ne!(prov.read_u64(0), 0x200);
        assert_ne!(prov.read_u64(8), 0x300);
    }

    #[test]
    fn vtable_disasm_wrong_address() {
        // test_disasm.cpp:304-344 — exercises only the provider + disasm paths
        // (no compose), so it runs as-is.
        let mut mem = vec![0u8; 1024];
        w64(&mut mem, 0x00, 0x80); // root vptr -> 0x80
        w64(&mut mem, 0x80, 0x100); // vtable -> func @ 0x100
                                    // code @ 0x100: sub rsp, 0x28; nop; ret
        mem[0x100] = 0x48;
        mem[0x101] = 0x83;
        mem[0x102] = 0xec;
        mem[0x103] = 0x28;
        mem[0x104] = 0x90;
        mem[0x105] = 0xc3;

        let prov = BufferProvider::new(mem, "mem");

        // WRONG: read from node.offset=0 (root's vptr value).
        assert_eq!(prov.read_u64(0), 0x80);
        // RIGHT: read from composed address (vtable + 0).
        assert_eq!(prov.read_u64(0x80), 0x100);

        // Disassemble the RIGHT target.
        let right_code = prov.read_bytes(0x100, 128);
        let right_asm = disassemble(&right_code, 0x100, 64, 128);
        let right_lines = split_lines(&right_asm);
        assert!(right_lines.len() >= 3);
        assert_eq!(mnemonic(right_lines[0]), "sub rsp, 0x28");
        assert_eq!(mnemonic(right_lines[1]), "nop");
        assert_eq!(mnemonic(right_lines[2]), "ret");

        // Disassemble the WRONG target (vtable data) — must NOT yield sub rsp.
        let wrong_code = prov.read_bytes(0x80, 128);
        let wrong_asm = disassemble(&wrong_code, 0x80, 64, 128);
        assert!(
            !wrong_asm.contains("sub rsp"),
            "Wrong address should NOT produce sub rsp: {wrong_asm}"
        );
    }

    #[test]
    fn hover_flow_real_vs_snapshot() {
        // Disasm-relevant core of test_disasm.cpp:346-463 (testHoverFlow_full-
        // Simulation): the two-provider split (snapshot has only tree-data
        // pages, real provider has the code pages) + disassembly. The
        // compose() driven line-walk is in the #[ignore]d full test below.
        let mut mem = vec![0u8; 8192];
        w64(&mut mem, 0x000, 0x100); // __vptr
        w64(&mut mem, 0x100, 0x1000); // vtable[0] -> func0
        w64(&mut mem, 0x108, 0x1800); // vtable[1] -> func1
                                      // func0: push rbp; mov rbp, rsp; sub rsp, 0x20; ret
        mem[0x1000..0x1009]
            .copy_from_slice(&[0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x20, 0xc3]);
        // func1: xor eax, eax; ret
        mem[0x1800..0x1803].copy_from_slice(&[0x31, 0xc0, 0xc3]);

        let real_prov = BufferProvider::new(mem.clone(), "real");
        // Snapshot has only the first 0x200 bytes (root + vtable pages), NOT code.
        let snap_prov = BufferProvider::new(mem[..0x200].to_vec(), "snap");

        // func0: pointer value lives in the snapshot at vtable+0.
        let prov_addr0 = 0x100u64;
        assert!(snap_prov.is_readable(prov_addr0, 8));
        let ptr0 = snap_prov.read_u64(prov_addr0);
        assert_ne!(ptr0, 0);
        // Snapshot does NOT have the code page; the real provider does.
        assert!(!snap_prov.is_readable(ptr0, 1));
        let mut code0 = vec![0u8; 128];
        assert!(real_prov.read(ptr0, &mut code0));
        let asm0 = disassemble(&code0, ptr0, 64, 128);
        assert!(!asm0.is_empty());
        let l0 = split_lines(&asm0);
        assert!(l0.len() >= 4);
        assert_eq!(mnemonic(l0[0]), "push rbp");
        assert_eq!(mnemonic(l0[1]), "mov rbp, rsp");
        assert_eq!(mnemonic(l0[2]), "sub rsp, 0x20");
        assert_eq!(mnemonic(l0[3]), "ret");

        // func1.
        let prov_addr1 = 0x108u64;
        assert!(snap_prov.is_readable(prov_addr1, 8));
        let ptr1 = snap_prov.read_u64(prov_addr1);
        assert_ne!(ptr1, 0);
        assert!(!snap_prov.is_readable(ptr1, 1));
        let mut code1 = vec![0u8; 128];
        assert!(real_prov.read(ptr1, &mut code1));
        let asm1 = disassemble(&code1, ptr1, 64, 128);
        assert!(!asm1.is_empty());
        let l1 = split_lines(&asm1);
        assert!(l1.len() >= 2);
        assert_eq!(mnemonic(l1[0]), "xor eax, eax");
        assert_eq!(mnemonic(l1[1]), "ret");
    }

    // TODO(compose): un-ignore these full ports once compose::compose lands
    // (it is still a `todo!()` skeleton). The disasm-specific assertions they
    // carry are already covered by the three provider-driven tests above; what
    // remains is the compose() offsetAddr line-walk. See the editor/compose
    // workflow.

    #[test]
    #[ignore = "TODO(compose): un-ignore once compose::compose lands"]
    fn vtable_disasm_composed_address() {
        // Full port of test_disasm.cpp:135-302 — needs compose(tree, prov)
        // to produce LineMeta.offsetAddr for the pointer-expanded VTable.
        unimplemented!("requires compose::compose (skeleton)");
    }

    #[test]
    #[ignore = "TODO(compose): un-ignore once compose::compose lands"]
    fn hover_flow_full_simulation() {
        // Full port of test_disasm.cpp:346-463 — needs compose(tree, snapProv).
        unimplemented!("requires compose::compose (skeleton)");
    }
}
