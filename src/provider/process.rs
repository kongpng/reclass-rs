//! First-party local Process Memory provider.
//!
//! Target strings mirror the C++ plugin: `"pid:name"` or plain `"pid"`.

use std::path::Path;
use std::sync::Arc;

use crate::plugin::contract::ProcessInfo;
use arc_swap::ArcSwapOption;

#[cfg(not(any(target_os = "linux", windows)))]
use super::read_pages_in_runs;
#[cfg(any(target_os = "linux", test))]
use super::RegionType;
use super::{
    normalize_page_list, MemoryRegion, ModuleEntry, ModuleLookup, PageMap, Provider, ThreadInfo,
    K_MAX_BULK_READ_PAGES, K_PAGE_SIZE,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessTarget {
    pub pid: u32,
    pub name: String,
}

impl ProcessTarget {
    pub fn parse(target: &str) -> Result<Self, String> {
        let trimmed = target.trim();
        if trimmed.is_empty() {
            return Err("process target is empty".to_string());
        }
        let (pid_text, name) = trimmed
            .split_once(':')
            .map(|(pid, name)| (pid, name.to_string()))
            .unwrap_or((trimmed, String::new()));
        let pid = pid_text
            .parse::<u32>()
            .map_err(|_| format!("invalid process target PID: {pid_text}"))?;
        if pid == 0 {
            return Err("invalid process target PID: 0".to_string());
        }
        Ok(ProcessTarget { pid, name })
    }

    pub fn to_target(&self) -> String {
        if self.name.is_empty() {
            self.pid.to_string()
        } else {
            format!("{}:{}", self.pid, self.name)
        }
    }
}

pub struct LocalProcessProvider {
    inner: platform::Inner,
    readable_ranges: ArcSwapOption<Vec<(u64, u64)>>,
    module_lookup: ModuleLookup,
}

impl LocalProcessProvider {
    pub fn attach(target: &str) -> Result<Self, String> {
        let parsed = ProcessTarget::parse(target)?;
        platform::Inner::attach(parsed).map(|inner| {
            let modules = inner.enumerate_modules();
            Self {
                inner,
                readable_ranges: ArcSwapOption::from(None),
                module_lookup: ModuleLookup::new(modules),
            }
        })
    }

    pub fn can_handle(target: &str) -> bool {
        ProcessTarget::parse(target).is_ok()
    }

    pub fn enumerate_processes() -> Vec<ProcessInfo> {
        platform::enumerate_processes()
    }

    fn refresh_readable_ranges(&self, regions: &[MemoryRegion]) {
        self.readable_ranges
            .store(Some(Arc::new(readable_ranges_from_regions(regions))));
    }

    fn cached_is_readable(&self, addr: u64, len: i32) -> Option<bool> {
        let cached = self.readable_ranges.load();
        let ranges = cached.as_ref()?;
        (!ranges.is_empty()).then(|| readable_ranges_contains(ranges, addr, len))
    }

    fn cached_page_read_plan(&self, pages: &[u64]) -> Option<(Vec<u64>, PageMap)> {
        let cached = self.readable_ranges.load();
        let ranges = cached.as_ref()?;
        if ranges.is_empty() {
            return None;
        }

        let normalized = normalize_page_list(pages);
        let mut readable_pages = Vec::new();
        let mut out = PageMap::new();
        out.reserve(normalized.as_slice().len());
        let mut range_idx = 0usize;
        for &page_addr in normalized.as_slice() {
            while ranges
                .get(range_idx)
                .is_some_and(|(_, range_end)| *range_end <= page_addr)
            {
                range_idx += 1;
            }
            let page_end = page_addr.saturating_add(K_PAGE_SIZE);
            let readable = ranges
                .get(range_idx)
                .is_some_and(|(start, range_end)| page_addr >= *start && page_end <= *range_end);
            if readable {
                readable_pages.push(page_addr);
            } else {
                out.insert(page_addr, vec![0u8; K_PAGE_SIZE as usize].into());
            }
        }
        Some((readable_pages, out))
    }

    fn sparse_page_request(pages: &[u64]) -> bool {
        if pages.len() < K_MAX_BULK_READ_PAGES {
            return false;
        }
        let first = pages[0] & !(K_PAGE_SIZE - 1);
        let last = pages[pages.len() - 1] & !(K_PAGE_SIZE - 1);
        let Some(span) = (pages.len() as u64 - 1).checked_mul(K_PAGE_SIZE) else {
            return true;
        };
        let compact_end = first.checked_add(span);
        compact_end != Some(last)
    }

    fn sparse_uncached_page_read(&self, pages: &[u64]) -> Option<PageMap> {
        if !Self::sparse_page_request(pages) {
            return None;
        }
        let regions = self
            .inner
            .enumerate_regions_with_lookup(&self.module_lookup);
        self.refresh_readable_ranges(&regions);
        let (readable_pages, mut out) = self.cached_page_read_plan(pages)?;
        if !readable_pages.is_empty() {
            out.extend(self.inner.read_pages(&readable_pages));
        }
        Some(out)
    }
}

impl Provider for LocalProcessProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.inner.read(addr, buf)
    }

    fn read_pages(&self, pages: &[u64]) -> PageMap {
        if let Some((readable_pages, mut out)) = self.cached_page_read_plan(pages) {
            if !readable_pages.is_empty() {
                out.extend(self.inner.read_pages(&readable_pages));
            }
            return out;
        }
        if let Some(out) = self.sparse_uncached_page_read(pages) {
            return out;
        }

        #[cfg(target_os = "linux")]
        {
            return self.inner.read_pages(pages);
        }
        #[cfg(windows)]
        {
            return self.inner.read_pages(pages);
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        read_pages_in_runs(pages, |addr, buf| self.inner.read(addr, buf))
    }

    fn size(&self) -> i32 {
        self.inner.size()
    }

    fn write(&self, addr: u64, data: &[u8]) -> bool {
        self.inner.write(addr, data)
    }

    fn is_writable(&self) -> bool {
        self.inner.is_writable()
    }

    fn name(&self) -> String {
        self.inner.name()
    }

    fn is_live(&self) -> bool {
        true
    }

    fn prefers_coalesced_rescan_reads(&self) -> bool {
        true
    }

    fn kind(&self) -> String {
        "LocalProcess".to_string()
    }

    fn pointer_size(&self) -> i32 {
        self.inner.pointer_size()
    }

    fn base(&self) -> u64 {
        self.inner.base()
    }

    fn get_symbol(&self, addr: u64) -> String {
        self.module_lookup.symbol_for_addr_lower(addr)
    }

    fn symbol_to_address(&self, name: &str) -> u64 {
        self.module_lookup.symbol_to_address(name)
    }

    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        let regions = self
            .inner
            .enumerate_regions_with_lookup(&self.module_lookup);
        self.refresh_readable_ranges(&regions);
        regions
    }

    fn trusts_enumerated_region_readability(&self) -> bool {
        true
    }

    fn is_readable(&self, addr: u64, len: i32) -> bool {
        if len <= 0 {
            return len == 0;
        }
        match self.cached_is_readable(addr, len) {
            Some(true) => return true,
            Some(false) => return false,
            None => {}
        }

        let regions = self
            .inner
            .enumerate_regions_with_lookup(&self.module_lookup);
        let ranges = readable_ranges_from_regions(&regions);
        let readable = readable_ranges_contains(&ranges, addr, len);
        self.readable_ranges.store(Some(Arc::new(ranges)));
        readable
    }

    fn peb(&self) -> u64 {
        self.inner.peb()
    }

    fn tebs(&self) -> Vec<ThreadInfo> {
        self.inner.tebs()
    }

    fn enumerate_modules(&self) -> Vec<ModuleEntry> {
        self.module_lookup.clone_modules()
    }
}

fn module_name_from_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

fn readable_ranges_from_regions(regions: &[MemoryRegion]) -> Vec<(u64, u64)> {
    let mut ranges: Vec<_> = regions
        .iter()
        .filter(|region| region.readable && region.size > 0)
        .filter_map(|region| {
            let end = region.base.saturating_add(region.size);
            (end > region.base).then_some((region.base, end))
        })
        .collect();
    ranges.sort_unstable_by_key(|(start, _)| *start);

    merge_readable_ranges(ranges)
}

fn merge_readable_ranges(ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if let Some((_, last_end)) = merged.last_mut() {
            if start <= *last_end {
                *last_end = (*last_end).max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    merged
}

#[doc(hidden)]
pub fn bench_readable_ranges_from_regions(regions: &[MemoryRegion]) -> Vec<(u64, u64)> {
    readable_ranges_from_regions(regions)
}

fn readable_ranges_contains(ranges: &[(u64, u64)], addr: u64, len: i32) -> bool {
    if len <= 0 {
        return len == 0;
    }
    let Some(end) = addr.checked_add(len as u64) else {
        return false;
    };
    let idx = ranges.partition_point(|(_, range_end)| *range_end <= addr);
    ranges
        .get(idx)
        .is_some_and(|(start, range_end)| addr >= *start && end <= *range_end)
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Debug)]
struct MapEntry {
    start: u64,
    end: u64,
    perms: String,
    path: String,
}

#[cfg(any(target_os = "linux", test))]
fn parse_maps_text(text: &str) -> Vec<MapEntry> {
    text.lines().filter_map(parse_map_line).collect()
}

#[cfg(any(target_os = "linux", test))]
fn parse_map_line(line: &str) -> Option<MapEntry> {
    let mut parts = line.split_whitespace();
    let range = parts.next()?;
    let perms = parts.next()?.to_string();
    let _offset = parts.next()?;
    let _dev = parts.next()?;
    let _inode = parts.next()?;
    let path = parts.collect::<Vec<_>>().join(" ");
    let (start, end) = range.split_once('-')?;
    Some(MapEntry {
        start: u64::from_str_radix(start, 16).ok()?,
        end: u64::from_str_radix(end, 16).ok()?,
        perms,
        path,
    })
}

#[cfg(any(target_os = "linux", test))]
fn module_entries_from_maps(maps: &[MapEntry]) -> (Vec<ModuleEntry>, u64) {
    use std::collections::BTreeMap;

    let mut ranges: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut base = 0;
    for map in maps {
        let path = map.path.trim();
        if path.is_empty()
            || !path.starts_with('/')
            || path.starts_with("/dev/")
            || path.starts_with("/memfd:")
        {
            continue;
        }
        if base == 0 && map.perms.as_bytes().get(2) == Some(&b'x') {
            base = map.start;
        }
        ranges
            .entry(path.to_string())
            .and_modify(|(lo, hi)| {
                *lo = (*lo).min(map.start);
                *hi = (*hi).max(map.end);
            })
            .or_insert((map.start, map.end));
    }

    let modules = ranges
        .into_iter()
        .map(|(path, (base, end))| ModuleEntry {
            name: module_name_from_path(&path),
            full_path: path,
            base,
            size: end.saturating_sub(base),
        })
        .collect();
    (modules, base)
}

#[cfg(any(target_os = "linux", test))]
fn regions_from_maps(maps: &[MapEntry]) -> Vec<MemoryRegion> {
    maps.iter()
        .filter_map(|map| {
            if map.end <= map.start || map.perms.len() < 3 {
                return None;
            }
            let readable = map.perms.as_bytes().first() == Some(&b'r');
            if !readable {
                return None;
            }
            let writable = map.perms.as_bytes().get(1) == Some(&b'w');
            let executable = map.perms.as_bytes().get(2) == Some(&b'x');
            let path = map.path.trim();
            let mut module_name = String::new();
            let mut region_type = RegionType::Private;
            if path.starts_with('/') && !path.starts_with("/dev/") && !path.starts_with("/memfd:") {
                module_name = module_name_from_path(path);
                region_type = if executable {
                    RegionType::Image
                } else {
                    RegionType::Mapped
                };
            }
            Some(MemoryRegion {
                base: map.start,
                size: map.end - map.start,
                readable,
                writable,
                executable,
                module_name,
                region_type,
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs::{self, File, OpenOptions};
    use std::io::{IoSlice, IoSliceMut, Read};
    use std::os::unix::fs::FileExt;
    use std::sync::Mutex;

    use nix::sys::uio::{process_vm_readv, process_vm_writev, RemoteIoVec};
    use nix::unistd::Pid;

    use crate::plugin::contract::ProcessInfo;
    use crate::provider::{
        MemoryRegion, ModuleEntry, ModuleLookup, PageMap, ThreadInfo, K_PAGE_SIZE,
    };

    use super::{
        module_entries_from_maps, parse_maps_text, regions_from_maps, MapEntry, ProcessTarget,
    };

    pub struct Inner {
        pid: u32,
        name: String,
        mem: Mutex<File>,
        writable: bool,
        pointer_size: i32,
        base: u64,
        modules: Vec<ModuleEntry>,
    }

    impl Inner {
        pub fn attach(target: ProcessTarget) -> Result<Self, String> {
            let proc = procfs::process::Process::new(target.pid as i32)
                .map_err(|err| format!("open /proc/{}: {err}", target.pid))?;
            let name = if target.name.is_empty() {
                proc.stat()
                    .map(|s| s.comm)
                    .unwrap_or_else(|_| target.pid.to_string())
            } else {
                target.name
            };
            let mem_path = format!("/proc/{}/mem", target.pid);
            let (mem, writable) = match OpenOptions::new().read(true).write(true).open(&mem_path) {
                Ok(file) => (file, true),
                Err(_) => (
                    OpenOptions::new()
                        .read(true)
                        .open(&mem_path)
                        .map_err(|err| format!("open {mem_path}: {err}"))?,
                    false,
                ),
            };
            let pointer_size = detect_pointer_size(target.pid).unwrap_or(8);
            let maps = read_maps(target.pid);
            let (modules, base) = module_entries_from_maps(&maps);
            Ok(Self {
                pid: target.pid,
                name,
                mem: Mutex::new(mem),
                writable,
                pointer_size,
                base,
                modules,
            })
        }

        pub fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            if buf.is_empty() {
                return false;
            }
            let mut local = [IoSliceMut::new(buf)];
            let remote = [RemoteIoVec {
                base: addr as usize,
                len: local[0].len(),
            }];
            if process_vm_readv(Pid::from_raw(self.pid as i32), &mut local, &remote)
                .map(|n| n == local[0].len())
                .unwrap_or(false)
            {
                return true;
            }
            let Ok(mem) = self.mem.lock() else {
                buf.fill(0);
                return false;
            };
            match mem.read_at(buf, addr) {
                Ok(n) if n == buf.len() => true,
                Ok(n) => {
                    buf[n..].fill(0);
                    false
                }
                Err(_) => {
                    buf.fill(0);
                    false
                }
            }
        }

        pub fn read_pages(&self, pages: &[u64]) -> PageMap {
            let mut out = PageMap::new();
            if pages.is_empty() {
                return out;
            }

            let normalized_pages = super::normalize_page_list(pages);
            let pages = normalized_pages.as_slice();
            out.reserve(pages.len());
            let compact_runs = pages.len() <= super::super::K_MAX_BULK_READ_PAGES;

            let mut run_start = 0usize;
            while run_start < pages.len() {
                let mut run_end = run_start + 1;
                while run_end < pages.len()
                    && run_end - run_start < super::super::K_MAX_BULK_READ_PAGES
                    && pages[run_end - 1]
                        .checked_add(K_PAGE_SIZE)
                        .is_some_and(|next| next == pages[run_end])
                {
                    run_end += 1;
                }
                self.read_page_run(&pages[run_start..run_end], compact_runs, &mut out);
                run_start = run_end;
            }

            out
        }

        fn read_page_run(&self, pages: &[u64], compact: bool, out: &mut PageMap) {
            if pages.is_empty() {
                return;
            }
            if pages.len() == 1 {
                let mut bytes = vec![0u8; K_PAGE_SIZE as usize];
                let _ = self.read(pages[0], &mut bytes);
                out.insert(pages[0], bytes.into());
                return;
            }

            if !compact {
                let mut page_bytes: Vec<Vec<u8>> = (0..pages.len())
                    .map(|_| vec![0u8; K_PAGE_SIZE as usize])
                    .collect();
                let mut local: Vec<IoSliceMut<'_>> = page_bytes
                    .iter_mut()
                    .map(|bytes| IoSliceMut::new(bytes.as_mut_slice()))
                    .collect();
                let remote = [RemoteIoVec {
                    base: pages[0] as usize,
                    len: pages.len() * K_PAGE_SIZE as usize,
                }];
                let read = process_vm_readv(Pid::from_raw(self.pid as i32), &mut local, &remote)
                    .unwrap_or(0);
                let full_pages = (read / K_PAGE_SIZE as usize).min(pages.len());
                drop(local);
                if full_pages > 0 {
                    for (page_addr, bytes) in pages
                        .iter()
                        .copied()
                        .take(full_pages)
                        .zip(page_bytes.drain(..full_pages))
                    {
                        out.insert(page_addr, bytes.into());
                    }
                    if full_pages == pages.len() {
                        return;
                    }
                }

                for &page_addr in &pages[full_pages..] {
                    let mut bytes = vec![0u8; K_PAGE_SIZE as usize];
                    let _ = self.read(page_addr, &mut bytes);
                    out.insert(page_addr, bytes.into());
                }
                return;
            }

            let run_len = pages.len() * K_PAGE_SIZE as usize;
            let mut bytes = vec![0u8; run_len];
            let mut local = [IoSliceMut::new(bytes.as_mut_slice())];
            let remote = [RemoteIoVec {
                base: pages[0] as usize,
                len: run_len,
            }];
            let read =
                process_vm_readv(Pid::from_raw(self.pid as i32), &mut local, &remote).unwrap_or(0);
            let full_pages = (read / K_PAGE_SIZE as usize).min(pages.len());
            if full_pages > 0 {
                for (idx, &page_addr) in pages.iter().take(full_pages).enumerate() {
                    let start = idx * K_PAGE_SIZE as usize;
                    let end = start + K_PAGE_SIZE as usize;
                    out.insert(page_addr, bytes[start..end].to_vec().into());
                }
                if full_pages == pages.len() {
                    return;
                }
            }

            for &page_addr in &pages[full_pages..] {
                let mut bytes = vec![0u8; K_PAGE_SIZE as usize];
                let _ = self.read(page_addr, &mut bytes);
                out.insert(page_addr, bytes.into());
            }
        }

        pub fn write(&self, addr: u64, data: &[u8]) -> bool {
            if data.is_empty() || !self.writable {
                return false;
            }
            let local = [IoSlice::new(data)];
            let remote = [RemoteIoVec {
                base: addr as usize,
                len: data.len(),
            }];
            if process_vm_writev(Pid::from_raw(self.pid as i32), &local, &remote)
                .map(|n| n == data.len())
                .unwrap_or(false)
            {
                return true;
            }
            let Ok(mem) = self.mem.lock() else {
                return false;
            };
            mem.write_at(data, addr)
                .map(|n| n == data.len())
                .unwrap_or(false)
        }

        pub fn size(&self) -> i32 {
            0x10000
        }

        pub fn is_writable(&self) -> bool {
            self.writable
        }

        pub fn name(&self) -> String {
            self.name.clone()
        }

        pub fn pointer_size(&self) -> i32 {
            self.pointer_size
        }

        pub fn base(&self) -> u64 {
            self.base
        }

        pub fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            regions_from_maps(&read_maps(self.pid))
        }

        pub fn enumerate_regions_with_lookup(
            &self,
            _module_lookup: &ModuleLookup,
        ) -> Vec<MemoryRegion> {
            self.enumerate_regions()
        }

        pub fn peb(&self) -> u64 {
            0
        }

        pub fn tebs(&self) -> Vec<ThreadInfo> {
            Vec::new()
        }

        pub fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            self.modules.clone()
        }
    }

    pub fn enumerate_processes() -> Vec<ProcessInfo> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir("/proc") else {
            return out;
        };
        for entry in entries.flatten() {
            let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Ok(pid) = file_name.parse::<u32>() else {
                continue;
            };
            if pid == 0 {
                continue;
            }
            let mem_path = format!("/proc/{pid}/mem");
            if fs::File::open(&mem_path).is_err() {
                continue;
            }
            let proc = match procfs::process::Process::new(pid as i32) {
                Ok(proc) => proc,
                Err(_) => continue,
            };
            let name = proc
                .stat()
                .map(|s| s.comm)
                .unwrap_or_else(|_| pid.to_string());
            if name.is_empty() {
                continue;
            }
            let path = fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .and_then(|p| p.to_str().map(str::to_string))
                .unwrap_or_default();
            let is_32bit = detect_pointer_size(pid) == Some(4);
            out.push(ProcessInfo {
                pid,
                name,
                path,
                is_32bit,
            });
        }
        out.sort_by(|a, b| b.pid.cmp(&a.pid));
        out
    }

    fn read_maps(pid: u32) -> Vec<MapEntry> {
        std::fs::read_to_string(format!("/proc/{pid}/maps"))
            .map(|text| parse_maps_text(&text))
            .unwrap_or_default()
    }

    fn detect_pointer_size(pid: u32) -> Option<i32> {
        let mut file = File::open(format!("/proc/{pid}/exe")).ok()?;
        let mut header = [0u8; 5];
        file.read_exact(&mut header).ok()?;
        if &header[..4] == b"\x7fELF" && header[4] == 1 {
            Some(4)
        } else {
            Some(8)
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::mem::{size_of, transmute, zeroed};
    use std::ptr::null_mut;

    use crate::plugin::contract::ProcessInfo;
    use crate::provider::{
        MemoryRegion, ModuleEntry, ModuleLookup, PageMap, RegionType, ThreadInfo, K_PAGE_SIZE,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, HANDLE, HMODULE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    use windows_sys::Win32::System::Memory::{
        VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_IMAGE, MEM_MAPPED, PAGE_EXECUTE,
        PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD,
        PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        EnumProcessModulesEx, GetModuleBaseNameW, GetModuleFileNameExW, GetModuleInformation,
        LIST_MODULES_ALL, MODULEINFO,
    };
    use windows_sys::Win32::System::Threading::{
        IsWow64Process, OpenProcess, OpenThread, QueryFullProcessImageNameW,
        PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_OPERATION,
        PROCESS_VM_READ, PROCESS_VM_WRITE, THREAD_QUERY_LIMITED_INFORMATION,
    };

    use super::{module_name_from_path, ProcessTarget};

    type NtStatus = i32;
    type NtQueryInformationProcessFn =
        unsafe extern "system" fn(HANDLE, u32, *mut c_void, u32, *mut u32) -> NtStatus;
    type NtQuerySystemInformationFn =
        unsafe extern "system" fn(u32, *mut c_void, u32, *mut u32) -> NtStatus;
    type NtQueryInformationThreadFn =
        unsafe extern "system" fn(HANDLE, u32, *mut c_void, u32, *mut u32) -> NtStatus;

    const STATUS_INFO_LENGTH_MISMATCH: NtStatus = 0xC000_0004u32 as NtStatus;
    const PROCESS_BASIC_INFORMATION_CLASS: u32 = 0;
    const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
    const THREAD_BASIC_INFORMATION_CLASS: u32 = 0;

    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *mut u16,
    }

    #[repr(C)]
    struct ClientId {
        unique_process: HANDLE,
        unique_thread: HANDLE,
    }

    #[repr(C)]
    struct SystemThreadInformation {
        kernel_time: i64,
        user_time: i64,
        create_time: i64,
        wait_time: u32,
        start_address: *mut c_void,
        client_id: ClientId,
        priority: i32,
        base_priority: i32,
        context_switches: u32,
        thread_state: u32,
        wait_reason: u32,
    }

    #[repr(C)]
    struct SystemProcessInformation {
        next_entry_offset: u32,
        number_of_threads: u32,
        working_set_private_size: i64,
        hard_fault_count: u32,
        number_of_threads_high_watermark: u32,
        cycle_time: u64,
        create_time: i64,
        user_time: i64,
        kernel_time: i64,
        image_name: UnicodeString,
        base_priority: i32,
        unique_process_id: HANDLE,
        inherited_from_unique_process_id: *mut c_void,
        handle_count: u32,
        session_id: u32,
        unique_process_key: usize,
        peak_virtual_size: usize,
        virtual_size: usize,
        page_fault_count: u32,
        _pad0: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_page_count: usize,
        read_operation_count: i64,
        write_operation_count: i64,
        other_operation_count: i64,
        read_transfer_count: i64,
        write_transfer_count: i64,
        other_transfer_count: i64,
    }

    #[repr(C, align(8))]
    struct ThreadBasicInformation {
        exit_status: NtStatus,
        _pad: u32,
        teb_base_address: *mut c_void,
        client_id: ClientId,
        affinity_mask: usize,
        priority: i32,
        base_priority: i32,
    }

    #[repr(C)]
    struct ProcessBasicInformation {
        reserved1: *mut c_void,
        peb_base_address: *mut c_void,
        reserved2: [*mut c_void; 2],
        unique_process_id: usize,
        reserved3: *mut c_void,
    }

    pub struct Inner {
        pid: u32,
        handle: HANDLE,
        name: String,
        writable: bool,
        pointer_size: i32,
        base: u64,
        peb: u64,
        modules: Vec<ModuleEntry>,
    }

    unsafe impl Send for Inner {}
    unsafe impl Sync for Inner {}

    impl Inner {
        pub fn attach(target: ProcessTarget) -> Result<Self, String> {
            unsafe {
                let full_access = PROCESS_QUERY_INFORMATION
                    | PROCESS_QUERY_LIMITED_INFORMATION
                    | PROCESS_VM_READ
                    | PROCESS_VM_WRITE
                    | PROCESS_VM_OPERATION;
                let mut handle = OpenProcess(full_access, 0, target.pid);
                let mut writable = !handle.is_null();
                if handle.is_null() {
                    handle = OpenProcess(
                        PROCESS_QUERY_INFORMATION
                            | PROCESS_QUERY_LIMITED_INFORMATION
                            | PROCESS_VM_READ,
                        0,
                        target.pid,
                    );
                    writable = false;
                }
                if handle.is_null() {
                    return Err(format!(
                        "OpenProcess failed for PID {} (error {})",
                        target.pid,
                        GetLastError()
                    ));
                }
                let name = if target.name.is_empty() {
                    query_process_path(handle)
                        .map(|p| module_name_from_path(&p))
                        .unwrap_or_else(|| target.pid.to_string())
                } else {
                    target.name
                };
                let pointer_size = detect_pointer_size(handle);
                let peb = query_peb(handle);
                let modules = enumerate_modules_for(handle);
                let base = modules.first().map(|m| m.base).unwrap_or(0);
                Ok(Self {
                    pid: target.pid,
                    handle,
                    name,
                    writable,
                    pointer_size,
                    base,
                    peb,
                    modules,
                })
            }
        }
        pub fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            if buf.is_empty() || self.handle.is_null() {
                return false;
            }
            unsafe {
                let mut read = 0usize;
                let _ = ReadProcessMemory(
                    self.handle,
                    addr as *const c_void,
                    buf.as_mut_ptr() as *mut c_void,
                    buf.len(),
                    &mut read,
                );
                if read < buf.len() {
                    buf[read..].fill(0);
                }
                read > 0
            }
        }

        pub fn read_pages(&self, pages: &[u64]) -> PageMap {
            let mut out = PageMap::new();
            if pages.is_empty() {
                return out;
            }

            let normalized_pages = super::normalize_page_list(pages);
            let pages = normalized_pages.as_slice();
            out.reserve(pages.len());

            let mut run_start = 0usize;
            while run_start < pages.len() {
                let mut run_end = run_start + 1;
                while run_end < pages.len()
                    && run_end - run_start < super::super::K_MAX_BULK_READ_PAGES
                    && pages[run_end - 1]
                        .checked_add(K_PAGE_SIZE)
                        .is_some_and(|next| next == pages[run_end])
                {
                    run_end += 1;
                }
                self.read_page_run(&pages[run_start..run_end], &mut out);
                run_start = run_end;
            }

            out
        }

        fn read_page_run(&self, pages: &[u64], out: &mut PageMap) {
            if pages.is_empty() {
                return;
            }
            if pages.len() == 1 {
                let mut bytes = vec![0u8; K_PAGE_SIZE as usize];
                let _ = self.read_full(pages[0], &mut bytes);
                out.insert(pages[0], bytes.into());
                return;
            }

            let run_len = pages.len() * K_PAGE_SIZE as usize;
            let mut bytes = vec![0u8; run_len];
            let read = self.read_available(pages[0], &mut bytes);
            let full_pages = (read / K_PAGE_SIZE as usize).min(pages.len());
            if full_pages > 0 {
                for (idx, &page_addr) in pages.iter().take(full_pages).enumerate() {
                    let start = idx * K_PAGE_SIZE as usize;
                    let end = start + K_PAGE_SIZE as usize;
                    out.insert(page_addr, bytes[start..end].to_vec().into());
                }
                if full_pages == pages.len() {
                    return;
                }
            }

            for &page_addr in &pages[full_pages..] {
                let mut bytes = vec![0u8; K_PAGE_SIZE as usize];
                let _ = self.read_full(page_addr, &mut bytes);
                out.insert(page_addr, bytes.into());
            }
        }

        fn read_full(&self, addr: u64, buf: &mut [u8]) -> bool {
            let read = self.read_available(addr, buf);
            if read < buf.len() {
                buf[read..].fill(0);
            }
            read == buf.len()
        }

        fn read_available(&self, addr: u64, buf: &mut [u8]) -> usize {
            if buf.is_empty() || self.handle.is_null() {
                return 0;
            }
            unsafe {
                let mut read = 0usize;
                let _ = ReadProcessMemory(
                    self.handle,
                    addr as *const c_void,
                    buf.as_mut_ptr() as *mut c_void,
                    buf.len(),
                    &mut read,
                );
                read.min(buf.len())
            }
        }

        pub fn write(&self, addr: u64, data: &[u8]) -> bool {
            if data.is_empty() || self.handle.is_null() || !self.writable {
                return false;
            }
            unsafe {
                let mut written = 0usize;
                WriteProcessMemory(
                    self.handle,
                    addr as *mut c_void,
                    data.as_ptr() as *const c_void,
                    data.len(),
                    &mut written,
                ) != 0
                    && written == data.len()
            }
        }
        pub fn size(&self) -> i32 {
            if self.handle.is_null() {
                0
            } else {
                0x10000
            }
        }
        pub fn is_writable(&self) -> bool {
            self.writable
        }
        pub fn name(&self) -> String {
            self.name.clone()
        }
        pub fn pointer_size(&self) -> i32 {
            self.pointer_size
        }
        pub fn base(&self) -> u64 {
            self.base
        }
        pub fn enumerate_regions_with_lookup(
            &self,
            module_lookup: &ModuleLookup,
        ) -> Vec<MemoryRegion> {
            unsafe { enumerate_regions_for(self.handle, module_lookup) }
        }
        pub fn peb(&self) -> u64 {
            self.peb
        }
        pub fn tebs(&self) -> Vec<ThreadInfo> {
            if self.handle.is_null() || self.peb == 0 {
                return Vec::new();
            }
            unsafe { query_tebs(self.pid) }
        }
        pub fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            self.modules.clone()
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            unsafe {
                if !self.handle.is_null() {
                    CloseHandle(self.handle);
                    self.handle = null_mut();
                }
            }
        }
    }

    unsafe fn ntdll_proc(name: *const u8) -> Option<unsafe extern "system" fn() -> isize> {
        let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr() as *const u8);
        if ntdll.is_null() {
            return None;
        }
        GetProcAddress(ntdll, name)
    }

    unsafe fn query_peb(handle: HANDLE) -> u64 {
        let Some(proc_addr) = ntdll_proc(c"NtQueryInformationProcess".as_ptr() as *const u8) else {
            return 0;
        };
        let nt_query_information_process: NtQueryInformationProcessFn = transmute(proc_addr);
        let mut pbi: ProcessBasicInformation = zeroed();
        let mut ret_len = 0u32;
        let status = nt_query_information_process(
            handle,
            PROCESS_BASIC_INFORMATION_CLASS,
            &mut pbi as *mut _ as *mut c_void,
            size_of::<ProcessBasicInformation>() as u32,
            &mut ret_len,
        );
        if status >= 0 && !pbi.peb_base_address.is_null() {
            pbi.peb_base_address as u64
        } else {
            0
        }
    }

    unsafe fn query_tebs(pid: u32) -> Vec<ThreadInfo> {
        let Some(qsi_addr) = ntdll_proc(c"NtQuerySystemInformation".as_ptr() as *const u8) else {
            return Vec::new();
        };
        let Some(qit_addr) = ntdll_proc(c"NtQueryInformationThread".as_ptr() as *const u8) else {
            return Vec::new();
        };
        let nt_query_system_information: NtQuerySystemInformationFn = transmute(qsi_addr);
        let nt_query_information_thread: NtQueryInformationThreadFn = transmute(qit_addr);

        let mut ret_len = 0u32;
        let mut buf_size = 1usize << 20;
        let mut buf = vec![0u8; buf_size];
        let mut status = STATUS_INFO_LENGTH_MISMATCH;
        for _ in 0..8 {
            status = nt_query_system_information(
                SYSTEM_PROCESS_INFORMATION_CLASS,
                buf.as_mut_ptr() as *mut c_void,
                buf_size as u32,
                &mut ret_len,
            );
            if status != STATUS_INFO_LENGTH_MISMATCH {
                break;
            }
            buf_size = buf_size.saturating_mul(2).max(ret_len as usize);
            buf.resize(buf_size, 0);
        }
        if status < 0 {
            return Vec::new();
        }

        let mut result = Vec::new();
        let mut offset = 0usize;
        loop {
            if offset.saturating_add(size_of::<SystemProcessInformation>()) > buf.len() {
                break;
            }
            let proc = &*(buf.as_ptr().add(offset) as *const SystemProcessInformation);
            if proc.unique_process_id as usize == pid as usize {
                let threads = buf
                    .as_ptr()
                    .add(offset + size_of::<SystemProcessInformation>())
                    as *const SystemThreadInformation;
                for i in 0..proc.number_of_threads as usize {
                    let thread = &*threads.add(i);
                    let tid = thread.client_id.unique_thread as usize as u32;
                    if tid == 0 {
                        continue;
                    }
                    let h_thread = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, tid);
                    if h_thread.is_null() {
                        continue;
                    }
                    let mut tbi: ThreadBasicInformation = zeroed();
                    let mut tbi_len = 0u32;
                    let qit_status = nt_query_information_thread(
                        h_thread,
                        THREAD_BASIC_INFORMATION_CLASS,
                        &mut tbi as *mut _ as *mut c_void,
                        size_of::<ThreadBasicInformation>() as u32,
                        &mut tbi_len,
                    );
                    if qit_status >= 0 && !tbi.teb_base_address.is_null() {
                        result.push(ThreadInfo {
                            teb_address: tbi.teb_base_address as u64,
                            thread_id: tid,
                        });
                    }
                    CloseHandle(h_thread);
                }
                break;
            }
            if proc.next_entry_offset == 0 {
                break;
            }
            offset = offset.saturating_add(proc.next_entry_offset as usize);
        }
        result
    }

    pub fn enumerate_processes() -> Vec<ProcessInfo> {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return Vec::new();
            }
            let mut out = Vec::new();
            let mut entry: PROCESSENTRY32W = zeroed();
            entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snap, &mut entry) != 0 {
                loop {
                    let pid = entry.th32ProcessID;
                    let name = wide_buf_to_string(&entry.szExeFile);
                    let h_proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                    let (path, is_32bit) = if h_proc.is_null() {
                        (String::new(), false)
                    } else {
                        let path = query_process_path(h_proc).unwrap_or_default();
                        let is_32bit = detect_pointer_size(h_proc) == 4;
                        CloseHandle(h_proc);
                        (path, is_32bit)
                    };
                    out.push(ProcessInfo {
                        pid,
                        name,
                        path,
                        is_32bit,
                    });
                    if Process32NextW(snap, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
            out.sort_by(|a, b| b.pid.cmp(&a.pid));
            out
        }
    }

    unsafe fn query_process_path(handle: HANDLE) -> Option<String> {
        let mut buf = vec![0u16; 32768];
        let mut len = buf.len() as u32;
        if QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len) == 0 {
            return None;
        }
        buf.truncate(len as usize);
        Some(String::from_utf16_lossy(&buf))
    }

    unsafe fn detect_pointer_size(handle: HANDLE) -> i32 {
        let mut wow64 = 0;
        if IsWow64Process(handle, &mut wow64) != 0 && wow64 != 0 {
            4
        } else {
            8
        }
    }

    unsafe fn enumerate_modules_for(handle: HANDLE) -> Vec<ModuleEntry> {
        let mut mods: [HMODULE; 1024] = [null_mut(); 1024];
        let mut needed = 0u32;
        if EnumProcessModulesEx(
            handle,
            mods.as_mut_ptr(),
            (mods.len() * size_of::<HMODULE>()) as u32,
            &mut needed,
            LIST_MODULES_ALL,
        ) == 0
        {
            return Vec::new();
        }
        let count = ((needed as usize) / size_of::<HMODULE>()).min(mods.len());
        let mut out = Vec::with_capacity(count);
        for module in mods.iter().copied().take(count) {
            let mut mi: MODULEINFO = zeroed();
            if GetModuleInformation(handle, module, &mut mi, size_of::<MODULEINFO>() as u32) == 0 {
                continue;
            }
            let mut name_buf = [0u16; 260];
            let name_len =
                GetModuleBaseNameW(handle, module, name_buf.as_mut_ptr(), name_buf.len() as u32)
                    as usize;
            let mut path_buf = vec![0u16; 32768];
            let path_len =
                GetModuleFileNameExW(handle, module, path_buf.as_mut_ptr(), path_buf.len() as u32)
                    as usize;
            let name = if name_len == 0 {
                String::new()
            } else {
                String::from_utf16_lossy(&name_buf[..name_len])
            };
            let full_path = if path_len == 0 {
                String::new()
            } else {
                String::from_utf16_lossy(&path_buf[..path_len])
            };
            out.push(ModuleEntry {
                name,
                full_path,
                base: mi.lpBaseOfDll as u64,
                size: mi.SizeOfImage as u64,
            });
        }
        out
    }

    unsafe fn enumerate_regions_for(
        handle: HANDLE,
        module_lookup: &ModuleLookup,
    ) -> Vec<MemoryRegion> {
        let mut regions = Vec::new();
        let mut addr = 0usize;
        loop {
            let mut mbi: MEMORY_BASIC_INFORMATION = zeroed();
            let got = VirtualQueryEx(
                handle,
                addr as *const c_void,
                &mut mbi,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            );
            if got == 0 {
                break;
            }
            if mbi.State == MEM_COMMIT {
                let protect = mbi.Protect;
                let readable = (protect & (PAGE_NOACCESS | PAGE_GUARD)) == 0
                    && (protect
                        & (PAGE_READONLY
                            | PAGE_READWRITE
                            | PAGE_WRITECOPY
                            | PAGE_EXECUTE_READ
                            | PAGE_EXECUTE_READWRITE
                            | PAGE_EXECUTE_WRITECOPY))
                        != 0;
                if readable {
                    let writable = (protect
                        & (PAGE_READWRITE
                            | PAGE_WRITECOPY
                            | PAGE_EXECUTE_READWRITE
                            | PAGE_EXECUTE_WRITECOPY))
                        != 0;
                    let executable = (protect
                        & (PAGE_EXECUTE
                            | PAGE_EXECUTE_READ
                            | PAGE_EXECUTE_READWRITE
                            | PAGE_EXECUTE_WRITECOPY))
                        != 0;
                    let base = mbi.BaseAddress as u64;
                    let region_type = if mbi.Type == MEM_IMAGE {
                        RegionType::Image
                    } else if mbi.Type == MEM_MAPPED {
                        RegionType::Mapped
                    } else {
                        RegionType::Private
                    };
                    let module_name = module_lookup
                        .find_by_addr(base)
                        .map(|m| m.name.clone())
                        .unwrap_or_default();
                    regions.push(MemoryRegion {
                        base,
                        size: mbi.RegionSize as u64,
                        readable,
                        writable,
                        executable,
                        module_name,
                        region_type,
                    });
                }
            }
            let next = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
            if next <= addr {
                break;
            }
            addr = next;
        }
        regions
    }

    fn wide_buf_to_string(buf: &[u16]) -> String {
        let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use crate::plugin::contract::ProcessInfo;
    use crate::provider::{MemoryRegion, ModuleEntry, ModuleLookup, ThreadInfo};

    use super::ProcessTarget;

    pub struct Inner {
        target: ProcessTarget,
    }

    impl Inner {
        pub fn attach(target: ProcessTarget) -> Result<Self, String> {
            Err(format!(
                "Process Memory provider is not implemented on this platform for PID {}",
                target.pid
            ))
        }
        pub fn read(&self, _addr: u64, _buf: &mut [u8]) -> bool {
            false
        }
        pub fn write(&self, _addr: u64, _data: &[u8]) -> bool {
            false
        }
        pub fn size(&self) -> i32 {
            0
        }
        pub fn is_writable(&self) -> bool {
            false
        }
        pub fn name(&self) -> String {
            self.target.name.clone()
        }
        pub fn pointer_size(&self) -> i32 {
            8
        }
        pub fn base(&self) -> u64 {
            0
        }
        pub fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            Vec::new()
        }
        pub fn enumerate_regions_with_lookup(
            &self,
            _module_lookup: &ModuleLookup,
        ) -> Vec<MemoryRegion> {
            self.enumerate_regions()
        }
        pub fn peb(&self) -> u64 {
            0
        }
        pub fn tebs(&self) -> Vec<ThreadInfo> {
            Vec::new()
        }
        pub fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            Vec::new()
        }
    }

    pub fn enumerate_processes() -> Vec<ProcessInfo> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::K_PAGE_SIZE;

    #[test]
    fn parses_pid_targets() {
        assert_eq!(
            ProcessTarget::parse("1234:notepad.exe").unwrap(),
            ProcessTarget {
                pid: 1234,
                name: "notepad.exe".to_string()
            }
        );
        assert_eq!(ProcessTarget::parse("1234").unwrap().pid, 1234);
        assert!(ProcessTarget::parse("0").is_err());
        assert!(ProcessTarget::parse("abc").is_err());
    }

    #[test]
    fn parses_linux_maps_for_modules_and_regions() {
        let maps = parse_maps_text(
            "00400000-00452000 r-xp 00000000 08:02 173521 /usr/bin/foo\n\
             00651000-00652000 rw-p 00051000 08:02 173521 /usr/bin/foo\n\
             7fff0000-7fff1000 rw-p 00000000 00:00 0 [stack]\n",
        );
        let (mods, base) = module_entries_from_maps(&maps);
        assert_eq!(base, 0x0040_0000);
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].name, "foo");
        assert_eq!(mods[0].size, 0x252000);
        let regions = regions_from_maps(&maps);
        assert_eq!(regions.len(), 3);
        assert_eq!(regions[0].region_type, RegionType::Image);
        assert_eq!(regions[1].region_type, RegionType::Mapped);
        assert_eq!(regions[2].region_type, RegionType::Private);
    }

    #[test]
    fn readable_ranges_are_sorted_merged_and_bound_checked() {
        let regions = vec![
            MemoryRegion {
                base: 0x3000,
                size: 0x1000,
                readable: true,
                ..MemoryRegion::default()
            },
            MemoryRegion {
                base: 0x1000,
                size: 0x1000,
                readable: true,
                ..MemoryRegion::default()
            },
            MemoryRegion {
                base: 0x2000,
                size: 0x1000,
                readable: true,
                ..MemoryRegion::default()
            },
            MemoryRegion {
                base: 0x5000,
                size: 0x1000,
                readable: false,
                ..MemoryRegion::default()
            },
        ];

        let ranges = readable_ranges_from_regions(&regions);

        assert_eq!(ranges, vec![(0x1000, 0x4000)]);
        assert!(readable_ranges_contains(&ranges, 0x1000, 1));
        assert!(readable_ranges_contains(&ranges, 0x3FFF, 1));
        assert!(!readable_ranges_contains(&ranges, 0x4000, 1));
        assert!(!readable_ranges_contains(&ranges, 0x5000, 1));
        assert!(!readable_ranges_contains(&ranges, u64::MAX, 8));
    }

    #[test]
    fn readable_ranges_ordered_input_uses_same_canonical_ranges() {
        let regions = vec![
            MemoryRegion {
                base: 0x1000,
                size: 0x1000,
                readable: true,
                ..MemoryRegion::default()
            },
            MemoryRegion {
                base: 0x2000,
                size: 0x1000,
                readable: true,
                ..MemoryRegion::default()
            },
            MemoryRegion {
                base: 0x5000,
                size: 0x1000,
                readable: true,
                ..MemoryRegion::default()
            },
        ];

        assert_eq!(
            readable_ranges_from_regions(&regions),
            vec![(0x1000, 0x3000), (0x5000, 0x6000)]
        );
    }

    #[test]
    fn module_lookup_indexes_ranges_and_names() {
        let lookup = ModuleLookup::new(vec![
            ModuleEntry {
                name: "Second.dll".into(),
                base: 0x3000,
                size: 0x1000,
                ..ModuleEntry::default()
            },
            ModuleEntry {
                name: "First.dll".into(),
                base: 0x1000,
                size: 0x1000,
                ..ModuleEntry::default()
            },
            ModuleEntry {
                name: "First.dll".into(),
                base: 0x5000,
                size: 0x1000,
                ..ModuleEntry::default()
            },
        ]);

        assert_eq!(lookup.find_by_addr(0x1004).unwrap().name, "First.dll");
        assert_eq!(lookup.find_by_addr(0x3004).unwrap().name, "Second.dll");
        assert!(lookup.find_by_addr(0x2000).is_none());
        assert_eq!(lookup.symbol_to_address("first.DLL"), 0x1000);
        assert_eq!(lookup.symbol_to_address("missing.dll"), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_process_readability_is_address_bound() {
        use crate::provider::Provider;

        let provider = LocalProcessProvider::attach(&std::process::id().to_string())
            .expect("attach to current process");
        let region = provider
            .enumerate_regions()
            .into_iter()
            .find(|r| r.readable && r.size > 0)
            .expect("current process has a readable mapping");
        assert!(provider.is_readable(region.base, 1));
        assert!(!provider.is_readable(1, 1));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_process_bulk_read_pages_matches_direct_reads() {
        use crate::provider::Provider;

        let provider = LocalProcessProvider::attach(&std::process::id().to_string())
            .expect("attach to current process");
        let region = provider
            .enumerate_regions()
            .into_iter()
            .find(|r| r.readable && r.size >= K_PAGE_SIZE * 2)
            .expect("current process has a two-page readable mapping");
        let pages = [region.base, region.base + K_PAGE_SIZE];
        let page_map = provider.read_pages(&pages);

        assert_eq!(page_map.len(), pages.len());
        for page_addr in pages {
            let mut expected = vec![0u8; K_PAGE_SIZE as usize];
            assert!(provider.read(page_addr, &mut expected));
            assert_eq!(
                page_map.get(&page_addr).map(|bytes| bytes.as_slice()),
                Some(expected.as_slice())
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_process_cached_readability_zero_fills_unreadable_pages() {
        use crate::provider::Provider;

        let provider = LocalProcessProvider::attach(&std::process::id().to_string())
            .expect("attach to current process");
        let region = provider
            .enumerate_regions()
            .into_iter()
            .find(|r| r.readable && r.size >= K_PAGE_SIZE)
            .expect("current process has a readable mapping");

        let page_map = provider.read_pages(&[0, region.base]);

        assert_eq!(page_map.len(), 2);
        assert!(page_map
            .get(&0)
            .expect("unreadable page is represented")
            .iter()
            .all(|&byte| byte == 0));

        let mut expected = vec![0u8; K_PAGE_SIZE as usize];
        assert!(provider.read(region.base, &mut expected));
        assert_eq!(
            page_map.get(&region.base).map(|bytes| bytes.as_slice()),
            Some(expected.as_slice())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_process_sparse_uncached_read_pages_prefilters_unreadable_pages() {
        use crate::provider::Provider;

        let provider = LocalProcessProvider::attach(&std::process::id().to_string())
            .expect("attach to current process");
        assert!(provider.readable_ranges.load().is_none());

        let live_bytes = vec![0xA5u8; K_PAGE_SIZE as usize * 2];
        let readable_addr = live_bytes.as_ptr() as u64;
        let readable_page = readable_addr & !(K_PAGE_SIZE - 1);
        let readable_offset = (readable_addr - readable_page) as usize;
        let mut pages: Vec<u64> = (0..K_MAX_BULK_READ_PAGES as u64)
            .map(|i| i * K_PAGE_SIZE)
            .collect();
        pages.push(readable_page);

        let page_map = provider.read_pages(&pages);

        assert!(provider.readable_ranges.load().is_some());
        assert_eq!(page_map.len(), K_MAX_BULK_READ_PAGES + 1);
        assert!(page_map
            .get(&0)
            .expect("unreadable page is represented")
            .iter()
            .all(|&byte| byte == 0));

        let readable = page_map.get(&readable_page).expect("readable page");
        let stable_len = 64.min(readable.len().saturating_sub(readable_offset));
        assert!(stable_len > 0);
        assert!(readable[readable_offset..readable_offset + stable_len]
            .iter()
            .all(|&byte| byte == 0xA5));

        std::hint::black_box(live_bytes);
    }
}
