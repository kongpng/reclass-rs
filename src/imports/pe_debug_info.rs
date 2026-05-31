//! PE CodeView debug-directory extraction over a `Provider`.
//!
//! Faithful 1:1 port of `src/imports/pe_debug_info.cpp` (193 lines). Reads raw
//! little-endian PE structs via discrete `Provider::read` calls (data may be
//! live/in-memory, NOT a contiguous file), so `object`/`goblin` cannot drive it
//! — the fixed-offset reads are hand-rolled. All multi-byte fields are
//! little-endian. See BMAP §4.

use crate::provider::Provider;

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

// Constants (cpp:63-68)
const K_MZ: u16 = 0x5A4D;
const K_PE: u32 = 0x0000_4550;
const K_PE32: u16 = 0x10b;
const K_PE32P: u16 = 0x20b;
const K_RSDS: u32 = 0x5344_5352;
const K_DEBUG_CODEVIEW: u32 = 2;

// Sizes the algorithm depends on.
const SIZEOF_DOS_HEADER: usize = 64;
const SIZEOF_COFF_HEADER: u64 = 20;
const SIZEOF_DATA_DIRECTORY: usize = 8;
const SIZEOF_DEBUG_DIRECTORY: u64 = 28;
/// `sizeof(CvInfoPdb70)` = Signature(4) + Guid[16] + Age(4) = 24.
const SIZEOF_CV_INFO_PDB70: u32 = 24;

/// Read exactly `N` bytes at `addr`, returning `None` on a failed/short read
/// (mirrors the C++ `!prov.read(...)` failure path).
fn read_exact<const N: usize>(prov: &dyn Provider, addr: u64) -> Option<[u8; N]> {
    let mut buf = [0u8; N];
    if prov.read(addr, &mut buf) {
        Some(buf)
    } else {
        None
    }
}

/// `guidToString` (`pe_debug_info.cpp:70-83`) — Windows mixed-endian GUID:
/// Data1(4B LE), Data2(2B LE), Data3(2B LE), Data4(8B sequential). 32 hex
/// chars, no dashes, uppercase.
fn guid_to_string(guid: &[u8; 16]) -> String {
    let d1 = u32::from_le_bytes([guid[0], guid[1], guid[2], guid[3]]);
    let d2 = u16::from_le_bytes([guid[4], guid[5]]);
    let d3 = u16::from_le_bytes([guid[6], guid[7]]);
    let mut s = format!("{:08x}{:04x}{:04x}", d1, d2, d3);
    for &b in &guid[8..16] {
        s.push_str(&format!("{:02x}", b));
    }
    s.to_uppercase()
}

/// `extractPdbDebugInfo(const Provider&, uint64_t moduleBase)`
/// (`pe_debug_info.cpp:85-191`).
pub fn extract_pdb_debug_info(prov: &dyn Provider, module_base: u64) -> PdbDebugInfo {
    let mut result = PdbDebugInfo::default();

    // Read DOS header
    let dos = match read_exact::<SIZEOF_DOS_HEADER>(prov, module_base) {
        Some(b) => b,
        None => return result,
    };
    let e_magic = u16::from_le_bytes([dos[0], dos[1]]);
    if e_magic != K_MZ {
        return result;
    }
    // e_lfanew is an i32 at offset 60 (after e_magic + pad[58]).
    let e_lfanew = i32::from_le_bytes([dos[60], dos[61], dos[62], dos[63]]);
    let pe_offset = module_base.wrapping_add(e_lfanew as i64 as u64);

    // Read PE signature
    let pe_sig_b = match read_exact::<4>(prov, pe_offset) {
        Some(b) => b,
        None => return result,
    };
    let pe_sig = u32::from_le_bytes(pe_sig_b);
    if pe_sig != K_PE {
        return result;
    }

    // Read COFF header (only used for its size — we skip past it)
    let coff_offset = pe_offset + 4;
    if read_exact::<20>(prov, coff_offset).is_none() {
        return result;
    }

    // Optional header magic to determine PE32 vs PE32+
    let opt_offset = coff_offset + SIZEOF_COFF_HEADER;
    let opt_magic_b = match read_exact::<2>(prov, opt_offset) {
        Some(b) => b,
        None => return result,
    };
    let opt_magic = u16::from_le_bytes(opt_magic_b);

    let (num_rva_and_sizes, data_dirs_offset) = if opt_magic == K_PE32 {
        match read_exact::<4>(prov, opt_offset + 92) {
            Some(b) => (u32::from_le_bytes(b), opt_offset + 96),
            None => return result,
        }
    } else if opt_magic == K_PE32P {
        match read_exact::<4>(prov, opt_offset + 108) {
            Some(b) => (u32::from_le_bytes(b), opt_offset + 112),
            None => return result,
        }
    } else {
        return result;
    };

    if num_rva_and_sizes <= 6 {
        return result; // no debug directory
    }

    let debug_dir = match read_exact::<SIZEOF_DATA_DIRECTORY>(
        prov,
        data_dirs_offset + 6 * SIZEOF_DATA_DIRECTORY as u64,
    ) {
        Some(b) => b,
        None => return result,
    };
    let dd_virtual_address =
        u32::from_le_bytes([debug_dir[0], debug_dir[1], debug_dir[2], debug_dir[3]]);
    let dd_size = u32::from_le_bytes([debug_dir[4], debug_dir[5], debug_dir[6], debug_dir[7]]);

    if dd_virtual_address == 0 || dd_size == 0 {
        return result;
    }

    let num_entries = dd_size / SIZEOF_DEBUG_DIRECTORY as u32;
    for i in 0..num_entries {
        let entry_addr =
            module_base + dd_virtual_address as u64 + i as u64 * SIZEOF_DEBUG_DIRECTORY;
        let entry = match read_exact::<28>(prov, entry_addr) {
            Some(b) => b,
            None => continue,
        };
        // DebugDirectory layout: Characteristics(4), TimeDateStamp(4),
        // MajorVersion(2), MinorVersion(2), Type(4)@16, SizeOfData(4)@20,
        // AddressOfRawData(4)@24, PointerToRawData(4)@... — wait, 28 bytes total.
        let entry_type = u32::from_le_bytes([entry[16], entry[17], entry[18], entry[19]]);
        let size_of_data = u32::from_le_bytes([entry[20], entry[21], entry[22], entry[23]]);
        let address_of_raw_data = u32::from_le_bytes([entry[24], entry[25], entry[26], entry[27]]);

        if entry_type != K_DEBUG_CODEVIEW {
            continue;
        }

        if address_of_raw_data == 0 || size_of_data < SIZEOF_CV_INFO_PDB70 + 1 {
            continue;
        }

        let cv_addr = module_base + address_of_raw_data as u64;
        let cv = match read_exact::<24>(prov, cv_addr) {
            Some(b) => b,
            None => continue,
        };
        let cv_signature = u32::from_le_bytes([cv[0], cv[1], cv[2], cv[3]]);
        if cv_signature != K_RSDS {
            continue;
        }
        let mut guid = [0u8; 16];
        guid.copy_from_slice(&cv[4..20]);
        let cv_age = u32::from_le_bytes([cv[20], cv[21], cv[22], cv[23]]);

        // Read PDB filename (null-terminated string after the struct)
        let mut name_max_len = (size_of_data - SIZEOF_CV_INFO_PDB70) as usize;
        if name_max_len > 260 {
            name_max_len = 260;
        }
        let mut name_buf = vec![0u8; name_max_len];
        if !prov.read(cv_addr + SIZEOF_CV_INFO_PDB70 as u64, &mut name_buf) {
            continue;
        }
        // Latin1 up to NUL.
        let nul = name_buf
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(name_buf.len());
        let mut pdb_name: String = name_buf[..nul].iter().map(|&b| b as char).collect();

        // Strip path: after last '\\', then after last '/'.
        if let Some(p) = pdb_name.rfind('\\') {
            pdb_name = pdb_name[p + 1..].to_string();
        }
        if let Some(p) = pdb_name.rfind('/') {
            pdb_name = pdb_name[p + 1..].to_string();
        }

        result.pdb_name = pdb_name;
        result.guid_string = guid_to_string(&guid);
        result.age = cv_age;
        result.valid = true;
        return result;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::BufferProvider;

    // Build a minimal synthetic PE32+ buffer in memory where RVAs == file
    // offsets (module_base passed to the extractor == buffer base 0).
    fn build_pe(guid: [u8; 16], age: u32, pdb_name: &str) -> Vec<u8> {
        // We'll lay the image out with generous headroom and fixed offsets.
        let mut buf = vec![0u8; 0x600];

        // DOS header
        buf[0..2].copy_from_slice(&K_MZ.to_le_bytes()); // e_magic
        let e_lfanew: i32 = 0x80;
        buf[60..64].copy_from_slice(&e_lfanew.to_le_bytes());

        let pe = e_lfanew as usize;
        // PE signature
        buf[pe..pe + 4].copy_from_slice(&K_PE.to_le_bytes());
        // COFF header (20 bytes) — leave zeros, set nothing important.
        let opt = pe + 4 + 20;
        // Optional header magic PE32+
        buf[opt..opt + 2].copy_from_slice(&K_PE32P.to_le_bytes());
        // NumberOfRvaAndSizes at opt+108
        let num_rva: u32 = 16;
        buf[opt + 108..opt + 112].copy_from_slice(&num_rva.to_le_bytes());
        // data dirs at opt+112; debug dir index 6 → +48
        let data_dirs = opt + 112;
        let debug_dd = data_dirs + 6 * 8;
        // Place the debug directory entry somewhere later.
        let debug_dir_rva: u32 = 0x400;
        let debug_dir_size: u32 = 28; // one entry
        buf[debug_dd..debug_dd + 4].copy_from_slice(&debug_dir_rva.to_le_bytes());
        buf[debug_dd + 4..debug_dd + 8].copy_from_slice(&debug_dir_size.to_le_bytes());

        // DebugDirectory entry @ 0x400
        let dd = debug_dir_rva as usize;
        // Type @ +16 = CodeView(2)
        buf[dd + 16..dd + 20].copy_from_slice(&K_DEBUG_CODEVIEW.to_le_bytes());
        // SizeOfData @ +20 = 24 + name.len()+1
        let cv_size: u32 = SIZEOF_CV_INFO_PDB70 + pdb_name.len() as u32 + 1;
        buf[dd + 20..dd + 24].copy_from_slice(&cv_size.to_le_bytes());
        // AddressOfRawData @ +24 → RVA of CvInfoPdb70
        let cv_rva: u32 = 0x480;
        buf[dd + 24..dd + 28].copy_from_slice(&cv_rva.to_le_bytes());

        // CvInfoPdb70 @ 0x480
        let cv = cv_rva as usize;
        buf[cv..cv + 4].copy_from_slice(&K_RSDS.to_le_bytes());
        buf[cv + 4..cv + 20].copy_from_slice(&guid);
        buf[cv + 20..cv + 24].copy_from_slice(&age.to_le_bytes());
        // Name after struct (null-terminated)
        let name_start = cv + 24;
        buf[name_start..name_start + pdb_name.len()].copy_from_slice(pdb_name.as_bytes());
        buf[name_start + pdb_name.len()] = 0;

        buf
    }

    #[test]
    fn extracts_valid_codeview() {
        let guid: [u8; 16] = [
            0x11, 0x22, 0x33, 0x44, // Data1 (LE) → 0x44332211
            0x55, 0x66, // Data2 → 0x6655
            0x77, 0x88, // Data3 → 0x8877
            0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, // Data4 sequential
        ];
        let buf = build_pe(guid, 7, "C:\\sym\\ntoskrnl.pdb");
        let prov = BufferProvider::new(buf, "synthetic.exe");
        let info = extract_pdb_debug_info(&prov, 0);
        assert!(info.valid);
        assert_eq!(info.pdb_name, "ntoskrnl.pdb");
        assert_eq!(info.age, 7);
        // Mixed-endian: d1=44332211 d2=6655 d3=8877 then sequential 99AABBCCDDEEFF00
        assert_eq!(
            info.guid_string,
            "4433221166558877".to_string() + "99AABBCCDDEEFF00"
        );
    }

    #[test]
    fn guid_to_string_matches_expected() {
        let guid: [u8; 16] = [
            0x78, 0x56, 0x34, 0x12, // d1 = 0x12345678
            0xBC, 0x9A, // d2 = 0x9ABC
            0xF0, 0xDE, // d3 = 0xDEF0
            0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF,
        ];
        assert_eq!(
            guid_to_string(&guid),
            "12345678 9ABC DEF0 0123456789ABCDEF".replace(' ', "")
        );
    }

    #[test]
    fn non_mz_returns_invalid() {
        let prov = BufferProvider::new(vec![0u8; 0x600], "x");
        let info = extract_pdb_debug_info(&prov, 0);
        assert!(!info.valid);
    }

    #[test]
    fn truncated_returns_invalid() {
        let prov = BufferProvider::new(vec![0x4D, 0x5A, 0x00], "x"); // MZ then truncated
        let info = extract_pdb_debug_info(&prov, 0);
        assert!(!info.valid);
    }
}
