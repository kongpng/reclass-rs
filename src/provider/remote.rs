//! Remote Process Memory provider.
//!
//! Target strings mirror the C++ plugin: `"rpm:{pid}:{name}"`.

use std::mem::size_of;

use bytemuck::pod_read_unaligned;
use bytes::Bytes;
use rcx_rpc::RcxRpcReadEntry;

use super::{MemoryRegion, ModuleEntry, PageMap, Provider, K_PAGE_SIZE};

const REMOTE_BULK_PAGE_SLICE_THRESHOLD: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteProcessTarget {
    pub pid: u32,
    pub name: String,
}

impl RemoteProcessTarget {
    pub fn parse(target: &str) -> Result<Self, String> {
        let mut parts = target.split(':');
        let Some(prefix) = parts.next() else {
            return Err("remote process target is empty".to_string());
        };
        if prefix != "rpm" {
            return Err(format!("invalid remote process target prefix: {prefix}"));
        }
        let Some(pid_text) = parts.next() else {
            return Err("remote process target is missing PID".to_string());
        };
        let pid = pid_text
            .parse::<u32>()
            .map_err(|_| format!("invalid remote process target PID: {pid_text}"))?;
        if pid == 0 {
            return Err("invalid remote process target PID: 0".to_string());
        }
        let name = parts.collect::<Vec<_>>().join(":");
        if name.is_empty() {
            return Err("remote process target is missing process name".to_string());
        }
        Ok(Self { pid, name })
    }

    pub fn to_target(&self) -> String {
        format!("rpm:{}:{}", self.pid, self.name)
    }
}

fn read_batch_entry(data: &[u8], index: usize) -> Option<RcxRpcReadEntry> {
    let entry_size = size_of::<RcxRpcReadEntry>();
    let start = index.checked_mul(entry_size)?;
    let end = start.checked_add(entry_size)?;
    data.get(start..end)
        .map(pod_read_unaligned::<RcxRpcReadEntry>)
}

fn insert_zero_pages(pages: &[u64], out: &mut PageMap) {
    for &page_addr in pages {
        out.insert(page_addr, vec![0u8; K_PAGE_SIZE as usize].into());
    }
}

fn insert_read_batch_pages_from_payload(
    pages: &[u64],
    data: &[u8],
    response_count: usize,
    out: &mut PageMap,
) {
    let responses = response_count.min(pages.len());
    if responses <= REMOTE_BULK_PAGE_SLICE_THRESHOLD {
        insert_read_batch_pages_allocating_from_payload(pages, data, responses, out);
        return;
    }

    let page_len = K_PAGE_SIZE as usize;
    let mut copied = vec![0u8; responses * page_len];
    let entry_size = size_of::<RcxRpcReadEntry>();
    let table_len = pages.len() * entry_size;
    let contiguous_end = table_len.saturating_add(copied.len());
    if let Some(src) = data.get(table_len..contiguous_end) {
        copied.copy_from_slice(src);
    } else {
        for i in 0..responses {
            let Some(entry) = read_batch_entry(data, i) else {
                continue;
            };
            let src_start = entry.data_offset as usize;
            let src_end = src_start.saturating_add(page_len);
            let dst_start = i * page_len;
            let dst_end = dst_start + page_len;
            if let Some(src) = data.get(src_start..src_end) {
                copied[dst_start..dst_end].copy_from_slice(src);
            }
        }
    }

    let copied = Bytes::from(copied);
    for (i, &page_addr) in pages.iter().take(responses).enumerate() {
        let start = i * page_len;
        let end = start + page_len;
        out.insert(page_addr, copied.slice(start..end).into());
    }
    if responses < pages.len() {
        insert_zero_pages(&pages[responses..], out);
    }
}

fn insert_read_batch_pages_allocating_from_payload(
    pages: &[u64],
    data: &[u8],
    responses: usize,
    out: &mut PageMap,
) {
    let page_len = K_PAGE_SIZE as usize;
    for (i, &page_addr) in pages.iter().enumerate() {
        let mut bytes = vec![0u8; page_len];
        if i < responses {
            if let Some(entry) = read_batch_entry(data, i) {
                let src_start = entry.data_offset as usize;
                let src_end = src_start.saturating_add(page_len);
                if let Some(src) = data.get(src_start..src_end) {
                    bytes.copy_from_slice(src);
                }
            }
        }
        out.insert(page_addr, bytes.into());
    }
}

#[doc(hidden)]
pub fn bench_remote_read_batch_insert_allocating(pages: &[u64], data: &[u8]) -> usize {
    let mut out = PageMap::new();
    out.reserve(pages.len());
    insert_read_batch_pages_allocating_from_payload(pages, data, pages.len(), &mut out);
    bench_page_map_score(&out)
}

#[doc(hidden)]
pub fn bench_remote_read_batch_insert_adaptive(pages: &[u64], data: &[u8]) -> usize {
    let mut out = PageMap::new();
    out.reserve(pages.len());
    insert_read_batch_pages_from_payload(pages, data, pages.len(), &mut out);
    bench_page_map_score(&out)
}

fn bench_page_map_score(map: &PageMap) -> usize {
    map.values().fold(map.len(), |score, page| {
        score
            .wrapping_add(page.len())
            .wrapping_add(page.first().copied().unwrap_or_default() as usize)
    })
}

pub struct RemoteProcessProvider {
    inner: platform::Inner,
}

impl RemoteProcessProvider {
    pub fn attach(target: &str) -> Result<Self, String> {
        let parsed = RemoteProcessTarget::parse(target)?;
        platform::Inner::attach(parsed).map(|inner| Self { inner })
    }

    pub fn can_handle(target: &str) -> bool {
        RemoteProcessTarget::parse(target).is_ok()
    }

    pub fn inject_payload(pid: u32) -> Result<(), String> {
        platform::inject_payload(pid)
    }
}

impl Provider for RemoteProcessProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.inner.read(addr, buf)
    }

    fn read_pages(&self, pages: &[u64]) -> PageMap {
        self.inner.read_pages(pages)
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
        "RemoteProcess".to_string()
    }

    fn pointer_size(&self) -> i32 {
        self.inner.pointer_size()
    }

    fn base(&self) -> u64 {
        self.inner.base()
    }

    fn get_symbol(&self, addr: u64) -> String {
        self.inner.get_symbol(addr)
    }

    fn symbol_to_address(&self, name: &str) -> u64 {
        self.inner.symbol_to_address(name)
    }

    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        self.inner.enumerate_regions()
    }

    fn trusts_enumerated_region_readability(&self) -> bool {
        true
    }

    fn is_readable(&self, _addr: u64, len: i32) -> bool {
        self.inner.size() > 0 && len >= 0
    }

    fn enumerate_modules(&self) -> Vec<ModuleEntry> {
        self.inner.enumerate_modules()
    }
}

#[cfg(windows)]
mod platform {
    use std::env;
    use std::ffi::c_void;
    use std::mem::{size_of, transmute};
    use std::path::PathBuf;
    use std::ptr::{copy_nonoverlapping, null, null_mut};
    use std::sync::Mutex;

    use bytemuck::from_bytes;
    use rcx_rpc::{
        req_name, rsp_name, shm_name, RcxRpcHeader, RcxRpcModuleEntry, RcxRpcReadEntry,
        RcxRpcRegionEntry, RCX_RPC_DATA_OFFSET, RCX_RPC_DATA_SIZE, RCX_RPC_MAX_BATCH,
        RCX_RPC_REGION_EXECUTABLE, RCX_RPC_REGION_IMAGE, RCX_RPC_REGION_MAPPED,
        RCX_RPC_REGION_READABLE, RCX_RPC_REGION_WRITABLE, RCX_RPC_SHM_SIZE, RCX_RPC_STATUS_OK,
        RPC_CMD_ENUM_MODULES, RPC_CMD_ENUM_REGIONS, RPC_CMD_PING, RPC_CMD_READ_BATCH,
        RPC_CMD_WRITE,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    use windows_sys::Win32::System::Memory::{
        MapViewOfFile, OpenFileMappingA, UnmapViewOfFile, VirtualAllocEx, VirtualFreeEx,
        FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
        PAGE_EXECUTE_READWRITE, PAGE_READWRITE,
    };
    use windows_sys::Win32::System::Threading::{
        CreateRemoteThread, GetExitCodeThread, OpenEventA, OpenProcess, SetEvent, Sleep,
        WaitForSingleObject, EVENT_ALL_ACCESS, PROCESS_ALL_ACCESS,
    };

    use super::RemoteProcessTarget;
    use crate::provider::{
        normalize_page_list, MemoryRegion, ModuleEntry, ModuleLookup, PageMap, RegionType,
        K_PAGE_SIZE,
    };

    pub struct Inner {
        target: RemoteProcessTarget,
        ipc: IpcClient,
        module_lookup: ModuleLookup,
        base: u64,
        pointer_size: i32,
    }

    impl Inner {
        pub fn attach(target: RemoteProcessTarget) -> Result<Self, String> {
            let ipc = IpcClient::connect(target.pid)?;
            let modules = ipc.enumerate_modules();
            let header = ipc.header_snapshot();
            let base = if header.image_base != 0 {
                header.image_base
            } else {
                modules.first().map(|m| m.base).unwrap_or(0)
            };
            let pointer_size = if matches!(header.pointer_size, 4 | 8) {
                header.pointer_size as i32
            } else {
                8
            };
            Ok(Self {
                target,
                ipc,
                module_lookup: ModuleLookup::new(modules),
                base,
                pointer_size,
            })
        }
        pub fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            self.ipc.read_single(addr, buf)
        }
        pub fn read_pages(&self, pages: &[u64]) -> PageMap {
            self.ipc.read_pages(pages)
        }
        pub fn write(&self, addr: u64, data: &[u8]) -> bool {
            self.ipc.write_single(addr, data)
        }
        pub fn size(&self) -> i32 {
            0x10000
        }
        pub fn is_writable(&self) -> bool {
            self.ipc.is_connected()
        }
        pub fn name(&self) -> String {
            self.target.name.clone()
        }
        pub fn pointer_size(&self) -> i32 {
            self.pointer_size
        }
        pub fn base(&self) -> u64 {
            self.base
        }
        pub fn get_symbol(&self, addr: u64) -> String {
            self.module_lookup.symbol_for_addr_lower(addr)
        }
        pub fn symbol_to_address(&self, name: &str) -> u64 {
            self.module_lookup.symbol_to_address(name)
        }
        pub fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            self.module_lookup.clone_modules()
        }
        pub fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            self.ipc.enumerate_regions()
        }
    }

    pub fn inject_payload(pid: u32) -> Result<(), String> {
        unsafe { inject_payload_impl(pid) }
    }

    struct IpcClient {
        h_shm: HANDLE,
        h_req: HANDLE,
        h_rsp: HANDLE,
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        lock: Mutex<()>,
    }

    unsafe impl Send for IpcClient {}
    unsafe impl Sync for IpcClient {}

    impl IpcClient {
        fn connect(pid: u32) -> Result<Self, String> {
            unsafe {
                let shm = c_string(shm_name(pid));
                let req = c_string(req_name(pid));
                let rsp = c_string(rsp_name(pid));
                let mut h_shm = null_mut();
                for _ in 0..100 {
                    h_shm = OpenFileMappingA(FILE_MAP_ALL_ACCESS, 0, shm.as_ptr());
                    if !h_shm.is_null() {
                        break;
                    }
                    Sleep(50);
                }
                if h_shm.is_null() {
                    return Err(format!("OpenFileMappingA failed for PID {pid}"));
                }
                let view = MapViewOfFile(h_shm, FILE_MAP_ALL_ACCESS, 0, 0, RCX_RPC_SHM_SIZE);
                if view.Value.is_null() {
                    CloseHandle(h_shm);
                    return Err(format!("MapViewOfFile failed for PID {pid}"));
                }
                let h_req = OpenEventA(EVENT_ALL_ACCESS, 0, req.as_ptr());
                let h_rsp = OpenEventA(EVENT_ALL_ACCESS, 0, rsp.as_ptr());
                if h_req.is_null() || h_rsp.is_null() {
                    if !h_req.is_null() {
                        CloseHandle(h_req);
                    }
                    if !h_rsp.is_null() {
                        CloseHandle(h_rsp);
                    }
                    UnmapViewOfFile(view);
                    CloseHandle(h_shm);
                    return Err(format!("OpenEventA failed for PID {pid}"));
                }
                let client = Self {
                    h_shm,
                    h_req,
                    h_rsp,
                    view,
                    lock: Mutex::new(()),
                };
                let hdr = client.header_snapshot();
                if hdr.payload_ready == 0 {
                    return Err(format!("rcx_payload for PID {pid} is not ready"));
                }
                if !client.ping() {
                    return Err(format!("rcx_payload ping failed for PID {pid}"));
                }
                Ok(client)
            }
        }

        fn header_snapshot(&self) -> RcxRpcHeader {
            unsafe { *(self.view.Value as *const RcxRpcHeader) }
        }

        fn is_connected(&self) -> bool {
            !self.view.Value.is_null()
                && !self.h_shm.is_null()
                && !self.h_req.is_null()
                && !self.h_rsp.is_null()
        }

        fn data_ptr(&self) -> *mut u8 {
            unsafe { (self.view.Value as *mut u8).add(RCX_RPC_DATA_OFFSET) }
        }

        fn header_ptr(&self) -> *mut RcxRpcHeader {
            self.view.Value as *mut RcxRpcHeader
        }

        fn signal_and_wait(&self, timeout_ms: u32) -> bool {
            unsafe {
                SetEvent(self.h_req) != 0
                    && WaitForSingleObject(self.h_rsp, timeout_ms) == WAIT_OBJECT_0
            }
        }

        fn ping(&self) -> bool {
            let Ok(_guard) = self.lock.lock() else {
                return false;
            };
            unsafe {
                let hdr = &mut *self.header_ptr();
                hdr.command = RPC_CMD_PING;
                hdr.status = RCX_RPC_STATUS_OK;
            }
            self.signal_and_wait(1000)
        }

        fn read_single(&self, addr: u64, buf: &mut [u8]) -> bool {
            if buf.is_empty() || buf.len() > RCX_RPC_DATA_SIZE - size_of::<RcxRpcReadEntry>() {
                return false;
            }
            let Ok(_guard) = self.lock.lock() else {
                buf.fill(0);
                return false;
            };
            unsafe {
                let hdr = &mut *self.header_ptr();
                let data = self.data_ptr();
                hdr.command = RPC_CMD_READ_BATCH;
                hdr.request_count = 1;
                hdr.status = RCX_RPC_STATUS_OK;
                let entry = data as *mut RcxRpcReadEntry;
                (*entry).address = addr;
                (*entry).length = buf.len() as u32;
                (*entry).data_offset = size_of::<RcxRpcReadEntry>() as u32;
                if !self.signal_and_wait(2000) {
                    buf.fill(0);
                    return false;
                }
                copy_nonoverlapping(
                    data.add((*entry).data_offset as usize),
                    buf.as_mut_ptr(),
                    buf.len(),
                );
                true
            }
        }

        fn read_pages(&self, pages: &[u64]) -> PageMap {
            let mut out = PageMap::new();
            if pages.is_empty() {
                return out;
            }

            let normalized_pages = normalize_page_list(pages);
            let pages = normalized_pages.as_slice();
            out.reserve(pages.len());

            let max_entries = rpc_read_page_capacity();
            for chunk in pages.chunks(max_entries) {
                self.read_page_batch(chunk, &mut out);
            }
            out
        }

        fn read_page_batch(&self, pages: &[u64], out: &mut PageMap) {
            let Ok(_guard) = self.lock.lock() else {
                super::insert_zero_pages(pages, out);
                return;
            };
            unsafe {
                let hdr = &mut *self.header_ptr();
                let data = self.data_ptr();
                let entry_size = size_of::<RcxRpcReadEntry>();
                let table_len = pages.len() * entry_size;
                hdr.command = RPC_CMD_READ_BATCH;
                hdr.request_count = pages.len() as u32;
                hdr.status = RCX_RPC_STATUS_OK;
                hdr.response_count = 0;
                hdr.total_data_used = 0;
                for (i, &page_addr) in pages.iter().enumerate() {
                    let entry = data.add(i * entry_size) as *mut RcxRpcReadEntry;
                    (*entry).address = page_addr;
                    (*entry).length = K_PAGE_SIZE as u32;
                    (*entry).data_offset = (table_len + i * K_PAGE_SIZE as usize) as u32;
                }

                if !self.signal_and_wait(2000) {
                    super::insert_zero_pages(pages, out);
                    return;
                }

                let payload = std::slice::from_raw_parts(data, RCX_RPC_DATA_SIZE);
                super::insert_read_batch_pages_from_payload(
                    pages,
                    payload,
                    hdr.response_count as usize,
                    out,
                );
            }
        }

        fn write_single(&self, addr: u64, data_in: &[u8]) -> bool {
            if data_in.is_empty() || data_in.len() > RCX_RPC_DATA_SIZE {
                return false;
            }
            let Ok(_guard) = self.lock.lock() else {
                return false;
            };
            unsafe {
                let hdr = &mut *self.header_ptr();
                let data = self.data_ptr();
                hdr.command = RPC_CMD_WRITE;
                hdr.write_address = addr;
                hdr.write_length = data_in.len() as u32;
                hdr.status = RCX_RPC_STATUS_OK;
                copy_nonoverlapping(data_in.as_ptr(), data, data_in.len());
                self.signal_and_wait(2000) && hdr.status == RCX_RPC_STATUS_OK
            }
        }

        fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            let Ok(_guard) = self.lock.lock() else {
                return Vec::new();
            };
            unsafe {
                let hdr = &mut *self.header_ptr();
                hdr.command = RPC_CMD_ENUM_MODULES;
                hdr.status = RCX_RPC_STATUS_OK;
                if !self.signal_and_wait(3000) || hdr.status != RCX_RPC_STATUS_OK {
                    return Vec::new();
                }
                let data = self.data_ptr();
                let count = hdr.response_count as usize;
                let max_entries = (RCX_RPC_DATA_SIZE / size_of::<RcxRpcModuleEntry>()).min(count);
                let mut out = Vec::with_capacity(max_entries);
                for i in 0..max_entries {
                    let off = i * size_of::<RcxRpcModuleEntry>();
                    let entry = from_bytes::<RcxRpcModuleEntry>(std::slice::from_raw_parts(
                        data.add(off),
                        size_of::<RcxRpcModuleEntry>(),
                    ));
                    let name_off = entry.name_offset as usize;
                    let name_len = entry.name_length as usize;
                    if name_off.saturating_add(name_len) > RCX_RPC_DATA_SIZE {
                        continue;
                    }
                    let raw = std::slice::from_raw_parts(data.add(name_off), name_len);
                    let name = decode_utf16_bytes(raw);
                    out.push(ModuleEntry {
                        name,
                        full_path: String::new(),
                        base: entry.base,
                        size: entry.size,
                    });
                }
                out
            }
        }

        fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            let Ok(_guard) = self.lock.lock() else {
                return Vec::new();
            };
            unsafe {
                let hdr = &mut *self.header_ptr();
                hdr.command = RPC_CMD_ENUM_REGIONS;
                hdr.status = RCX_RPC_STATUS_OK;
                if !self.signal_and_wait(3000) || hdr.status != RCX_RPC_STATUS_OK {
                    return Vec::new();
                }
                let data = self.data_ptr();
                let count = hdr.response_count as usize;
                let max_entries = (RCX_RPC_DATA_SIZE / size_of::<RcxRpcRegionEntry>()).min(count);
                let mut out = Vec::with_capacity(max_entries);
                for i in 0..max_entries {
                    let off = i * size_of::<RcxRpcRegionEntry>();
                    let entry = from_bytes::<RcxRpcRegionEntry>(std::slice::from_raw_parts(
                        data.add(off),
                        size_of::<RcxRpcRegionEntry>(),
                    ));
                    let name_off = entry.name_offset as usize;
                    let name_len = entry.name_length as usize;
                    if name_off.saturating_add(name_len) > RCX_RPC_DATA_SIZE {
                        continue;
                    }
                    let raw = std::slice::from_raw_parts(data.add(name_off), name_len);
                    out.push(MemoryRegion {
                        base: entry.base,
                        size: entry.size,
                        readable: (entry.flags & RCX_RPC_REGION_READABLE) != 0,
                        writable: (entry.flags & RCX_RPC_REGION_WRITABLE) != 0,
                        executable: (entry.flags & RCX_RPC_REGION_EXECUTABLE) != 0,
                        module_name: decode_utf16_bytes(raw),
                        region_type: rpc_region_type(entry.region_type),
                    });
                }
                out
            }
        }
    }

    fn rpc_region_type(kind: u32) -> RegionType {
        match kind {
            RCX_RPC_REGION_IMAGE => RegionType::Image,
            RCX_RPC_REGION_MAPPED => RegionType::Mapped,
            _ => RegionType::Private,
        }
    }

    fn rpc_read_page_capacity() -> usize {
        (RCX_RPC_DATA_SIZE / (size_of::<RcxRpcReadEntry>() + K_PAGE_SIZE as usize))
            .min(RCX_RPC_MAX_BATCH)
            .max(1)
    }

    impl Drop for IpcClient {
        fn drop(&mut self) {
            unsafe {
                if !self.view.Value.is_null() {
                    UnmapViewOfFile(self.view);
                    self.view = MEMORY_MAPPED_VIEW_ADDRESS { Value: null_mut() };
                }
                if !self.h_shm.is_null() {
                    CloseHandle(self.h_shm);
                    self.h_shm = null_mut();
                }
                if !self.h_req.is_null() {
                    CloseHandle(self.h_req);
                    self.h_req = null_mut();
                }
                if !self.h_rsp.is_null() {
                    CloseHandle(self.h_rsp);
                    self.h_rsp = null_mut();
                }
            }
        }
    }

    unsafe fn inject_payload_impl(pid: u32) -> Result<(), String> {
        let path = payload_path()?;
        let path_bytes = c_string(path.to_string_lossy().replace('/', "\\"));
        let h_proc = OpenProcess(PROCESS_ALL_ACCESS, 0, pid);
        if h_proc.is_null() {
            return Err(format!(
                "OpenProcess failed for PID {pid} (error {}). Try running as Administrator.",
                GetLastError()
            ));
        }

        let remote_path = VirtualAllocEx(
            h_proc,
            null(),
            path_bytes.len(),
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        );
        if remote_path.is_null() {
            CloseHandle(h_proc);
            return Err("VirtualAllocEx for payload path failed".to_string());
        }
        let mut written = 0usize;
        if WriteProcessMemory(
            h_proc,
            remote_path,
            path_bytes.as_ptr() as *const c_void,
            path_bytes.len(),
            &mut written,
        ) == 0
            || written != path_bytes.len()
        {
            VirtualFreeEx(h_proc, remote_path, 0, MEM_RELEASE);
            CloseHandle(h_proc);
            return Err("WriteProcessMemory for payload path failed".to_string());
        }

        let kernel32 = GetModuleHandleA(c"kernel32.dll".as_ptr() as *const u8);
        let load_library = GetProcAddress(kernel32, c"LoadLibraryA".as_ptr() as *const u8);
        let Some(load_library) = load_library else {
            VirtualFreeEx(h_proc, remote_path, 0, MEM_RELEASE);
            CloseHandle(h_proc);
            return Err("GetProcAddress(LoadLibraryA) failed".to_string());
        };
        let load_thread = CreateRemoteThread(
            h_proc,
            null(),
            0,
            Some(transmute(load_library)),
            remote_path,
            0,
            null_mut(),
        );
        if load_thread.is_null() {
            VirtualFreeEx(h_proc, remote_path, 0, MEM_RELEASE);
            CloseHandle(h_proc);
            return Err(format!(
                "CreateRemoteThread(LoadLibraryA) failed (error {})",
                GetLastError()
            ));
        }
        WaitForSingleObject(load_thread, 10_000);
        let mut module_exit = 0u32;
        GetExitCodeThread(load_thread, &mut module_exit);
        CloseHandle(load_thread);
        VirtualFreeEx(h_proc, remote_path, 0, MEM_RELEASE);
        if module_exit == 0 {
            CloseHandle(h_proc);
            return Err(format!(
                "LoadLibraryA returned NULL; ensure payload is at {}",
                path.display()
            ));
        }

        let init_name = c"RcxPayloadInit";
        let remote_init_name = VirtualAllocEx(
            h_proc,
            null(),
            init_name.to_bytes_with_nul().len(),
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        );
        if remote_init_name.is_null() {
            CloseHandle(h_proc);
            return Err("VirtualAllocEx for RcxPayloadInit name failed".to_string());
        }
        WriteProcessMemory(
            h_proc,
            remote_init_name,
            init_name.as_ptr() as *const c_void,
            init_name.to_bytes_with_nul().len(),
            null_mut(),
        );

        let get_proc_address = GetProcAddress(kernel32, c"GetProcAddress".as_ptr() as *const u8)
            .ok_or_else(|| "GetProcAddress(GetProcAddress) failed".to_string())?;
        let shellcode = init_shellcode(
            module_exit as u64,
            remote_init_name as u64,
            get_proc_address as usize as u64,
        );
        let remote_code = VirtualAllocEx(
            h_proc,
            null(),
            shellcode.len(),
            MEM_COMMIT | MEM_RESERVE,
            PAGE_EXECUTE_READWRITE,
        );
        if remote_code.is_null() {
            VirtualFreeEx(h_proc, remote_init_name, 0, MEM_RELEASE);
            CloseHandle(h_proc);
            return Err("VirtualAllocEx for init shellcode failed".to_string());
        }
        WriteProcessMemory(
            h_proc,
            remote_code,
            shellcode.as_ptr() as *const c_void,
            shellcode.len(),
            null_mut(),
        );
        let init_thread = CreateRemoteThread(
            h_proc,
            null(),
            0,
            Some(transmute(remote_code)),
            null(),
            0,
            null_mut(),
        );
        if init_thread.is_null() {
            VirtualFreeEx(h_proc, remote_code, 0, MEM_RELEASE);
            VirtualFreeEx(h_proc, remote_init_name, 0, MEM_RELEASE);
            CloseHandle(h_proc);
            return Err(format!(
                "CreateRemoteThread(RcxPayloadInit thunk) failed (error {})",
                GetLastError()
            ));
        }
        WaitForSingleObject(init_thread, 10_000);
        CloseHandle(init_thread);
        VirtualFreeEx(h_proc, remote_code, 0, MEM_RELEASE);
        VirtualFreeEx(h_proc, remote_init_name, 0, MEM_RELEASE);
        CloseHandle(h_proc);
        Ok(())
    }

    fn payload_path() -> Result<PathBuf, String> {
        if let Ok(path) = env::var("RECLASS_RCX_PAYLOAD_PATH") {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Ok(path);
            }
        }
        let exe = env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
        let mut candidates = Vec::new();
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("Plugins").join("rcx_payload.dll"));
            candidates.push(dir.join("plugins").join("rcx_payload.dll"));
            candidates.push(dir.join("rcx_payload.dll"));
            candidates.push(dir.join("deps").join("rcx_payload.dll"));
        }
        if let Some(dir) = exe.parent().and_then(|p| p.parent()) {
            candidates.push(dir.join("deps").join("rcx_payload.dll"));
        }
        for candidate in candidates {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err("rcx_payload.dll was not found beside the executable or in target deps; set RECLASS_RCX_PAYLOAD_PATH".to_string())
    }

    fn init_shellcode(h_module: u64, remote_init_name: u64, get_proc_address: u64) -> Vec<u8> {
        let mut sc = Vec::with_capacity(64);
        sc.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]);
        sc.extend_from_slice(&[0x48, 0xB9]);
        sc.extend_from_slice(&h_module.to_le_bytes());
        sc.extend_from_slice(&[0x48, 0xBA]);
        sc.extend_from_slice(&remote_init_name.to_le_bytes());
        sc.extend_from_slice(&[0x48, 0xB8]);
        sc.extend_from_slice(&get_proc_address.to_le_bytes());
        sc.extend_from_slice(&[0xFF, 0xD0]);
        sc.extend_from_slice(&[0x48, 0x85, 0xC0]);
        sc.extend_from_slice(&[0x74, 0x02]);
        sc.extend_from_slice(&[0xFF, 0xD0]);
        sc.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]);
        sc.push(0xC3);
        sc
    }

    fn c_string(mut s: String) -> Vec<u8> {
        s.push('\0');
        s.into_bytes()
    }

    fn decode_utf16_bytes(bytes: &[u8]) -> String {
        let words = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&words)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::env;
    use std::ffi::{c_void, CString};
    use std::fs::File;
    use std::mem::{size_of, zeroed};
    use std::os::fd::RawFd;
    use std::os::unix::fs::FileExt;
    use std::path::PathBuf;
    use std::ptr::{copy_nonoverlapping, null_mut};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    };
    use std::thread;
    use std::time::{Duration, Instant};

    use bytemuck::from_bytes;
    #[cfg(target_arch = "x86_64")]
    use nix::sys::ptrace;
    use nix::sys::signal::Signal;
    use nix::sys::wait::{waitpid, WaitStatus};
    use nix::unistd::Pid;
    use procfs::process::{MMPermissions, MMapPath, Process};
    use rcx_rpc::{
        req_name, rsp_name, shm_name, RcxRpcHeader, RcxRpcModuleEntry, RcxRpcReadEntry,
        RcxRpcRegionEntry, RCX_RPC_DATA_OFFSET, RCX_RPC_DATA_SIZE, RCX_RPC_MAX_BATCH,
        RCX_RPC_REGION_EXECUTABLE, RCX_RPC_REGION_IMAGE, RCX_RPC_REGION_MAPPED,
        RCX_RPC_REGION_READABLE, RCX_RPC_REGION_WRITABLE, RCX_RPC_SHM_SIZE, RCX_RPC_STATUS_OK,
        RPC_CMD_ENUM_MODULES, RPC_CMD_ENUM_REGIONS, RPC_CMD_PING, RPC_CMD_READ_BATCH,
        RPC_CMD_WRITE,
    };

    use super::RemoteProcessTarget;
    use crate::provider::{
        normalize_page_list, MemoryRegion, ModuleEntry, ModuleLookup, PageMap, RegionType,
        K_PAGE_SIZE,
    };

    pub struct Inner {
        target: RemoteProcessTarget,
        ipc: IpcClient,
        module_lookup: ModuleLookup,
        base: u64,
        pointer_size: i32,
    }

    impl Inner {
        pub fn attach(target: RemoteProcessTarget) -> Result<Self, String> {
            let ipc = IpcClient::connect(target.pid)?;
            let modules = ipc.enumerate_modules();
            let header = ipc.header_snapshot();
            let base = if header.image_base != 0 {
                header.image_base
            } else {
                modules.first().map(|m| m.base).unwrap_or(0)
            };
            let pointer_size = if matches!(header.pointer_size, 4 | 8) {
                header.pointer_size as i32
            } else {
                8
            };
            Ok(Self {
                target,
                ipc,
                module_lookup: ModuleLookup::new(modules),
                base,
                pointer_size,
            })
        }
        pub fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            self.ipc.read_single(addr, buf)
        }
        pub fn read_pages(&self, pages: &[u64]) -> PageMap {
            self.ipc.read_pages(pages)
        }
        pub fn write(&self, addr: u64, data: &[u8]) -> bool {
            self.ipc.write_single(addr, data)
        }
        pub fn size(&self) -> i32 {
            if self.ipc.is_connected() {
                0x10000
            } else {
                0
            }
        }
        pub fn is_writable(&self) -> bool {
            self.ipc.is_connected()
        }
        pub fn name(&self) -> String {
            self.target.name.clone()
        }
        pub fn pointer_size(&self) -> i32 {
            self.pointer_size
        }
        pub fn base(&self) -> u64 {
            self.base
        }
        pub fn get_symbol(&self, addr: u64) -> String {
            self.module_lookup.symbol_for_addr_lower(addr)
        }
        pub fn symbol_to_address(&self, name: &str) -> u64 {
            self.module_lookup.symbol_to_address(name)
        }
        pub fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            self.module_lookup.clone_modules()
        }
        pub fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            self.ipc.enumerate_regions()
        }
    }

    pub fn inject_payload(pid: u32) -> Result<(), String> {
        #[cfg(target_arch = "x86_64")]
        {
            return unsafe { inject_payload_impl(pid) };
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            Err(format!(
                "Remote Process Memory payload injection is currently implemented for Linux x86_64 only; PID {pid} was not modified"
            ))
        }
    }

    struct IpcClient {
        shm_fd: RawFd,
        req_sem: *mut libc::sem_t,
        rsp_sem: *mut libc::sem_t,
        view: *mut c_void,
        connected: AtomicBool,
        lock: Mutex<()>,
    }

    unsafe impl Send for IpcClient {}
    unsafe impl Sync for IpcClient {}

    impl IpcClient {
        fn connect(pid: u32) -> Result<Self, String> {
            unsafe {
                let shm = CString::new(shm_name(pid)).map_err(|err| err.to_string())?;
                let req = CString::new(req_name(pid)).map_err(|err| err.to_string())?;
                let rsp = CString::new(rsp_name(pid)).map_err(|err| err.to_string())?;
                let deadline = Instant::now() + Duration::from_millis(5000);
                let mut shm_fd = -1;
                while Instant::now() < deadline {
                    shm_fd = libc::shm_open(shm.as_ptr(), libc::O_RDWR, 0);
                    if shm_fd >= 0 {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                if shm_fd < 0 {
                    return Err(format!("shm_open failed for PID {pid}"));
                }

                let view = libc::mmap(
                    null_mut(),
                    RCX_RPC_SHM_SIZE,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    shm_fd,
                    0,
                );
                if view == libc::MAP_FAILED {
                    libc::close(shm_fd);
                    return Err(format!("mmap failed for PID {pid}"));
                }

                let req_sem = libc::sem_open(req.as_ptr(), 0);
                let rsp_sem = libc::sem_open(rsp.as_ptr(), 0);
                if sem_failed(req_sem) || sem_failed(rsp_sem) {
                    if !sem_failed(req_sem) {
                        libc::sem_close(req_sem);
                    }
                    if !sem_failed(rsp_sem) {
                        libc::sem_close(rsp_sem);
                    }
                    libc::munmap(view, RCX_RPC_SHM_SIZE);
                    libc::close(shm_fd);
                    return Err(format!("sem_open failed for PID {pid}"));
                }

                let client = Self {
                    shm_fd,
                    req_sem,
                    rsp_sem,
                    view,
                    connected: AtomicBool::new(true),
                    lock: Mutex::new(()),
                };

                while client.payload_ready() == 0 {
                    if Instant::now() >= deadline {
                        return Err(format!("rcx_payload for PID {pid} is not ready"));
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                if !client.ping() {
                    return Err(format!("rcx_payload ping failed for PID {pid}"));
                }
                Ok(client)
            }
        }

        fn payload_ready(&self) -> u32 {
            unsafe { std::ptr::read_volatile(&(*(self.view as *const RcxRpcHeader)).payload_ready) }
        }

        fn header_snapshot(&self) -> RcxRpcHeader {
            unsafe { *(self.view as *const RcxRpcHeader) }
        }

        fn is_connected(&self) -> bool {
            self.connected.load(Ordering::Acquire)
                && !self.view.is_null()
                && self.shm_fd >= 0
                && !sem_failed(self.req_sem)
                && !sem_failed(self.rsp_sem)
        }

        fn data_ptr(&self) -> *mut u8 {
            unsafe { (self.view as *mut u8).add(RCX_RPC_DATA_OFFSET) }
        }

        fn header_ptr(&self) -> *mut RcxRpcHeader {
            self.view as *mut RcxRpcHeader
        }

        fn signal_and_wait(&self, timeout_ms: u32) -> bool {
            if !self.is_connected() {
                return false;
            }
            unsafe {
                if libc::sem_post(self.req_sem) != 0 {
                    self.connected.store(false, Ordering::Release);
                    return false;
                }
                let mut ts = timespec_after(timeout_ms);
                loop {
                    if libc::sem_timedwait(self.rsp_sem, &mut ts) == 0 {
                        return true;
                    }
                    if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                    self.connected.store(false, Ordering::Release);
                    return false;
                }
            }
        }

        fn ping(&self) -> bool {
            let Ok(_guard) = self.lock.lock() else {
                return false;
            };
            unsafe {
                let hdr = &mut *self.header_ptr();
                hdr.command = RPC_CMD_PING;
                hdr.status = RCX_RPC_STATUS_OK;
            }
            self.signal_and_wait(1000)
        }

        fn read_single(&self, addr: u64, buf: &mut [u8]) -> bool {
            if buf.is_empty() || buf.len() > RCX_RPC_DATA_SIZE - size_of::<RcxRpcReadEntry>() {
                return false;
            }
            let Ok(_guard) = self.lock.lock() else {
                buf.fill(0);
                return false;
            };
            unsafe {
                let hdr = &mut *self.header_ptr();
                let data = self.data_ptr();
                hdr.command = RPC_CMD_READ_BATCH;
                hdr.request_count = 1;
                hdr.status = RCX_RPC_STATUS_OK;
                let entry = data as *mut RcxRpcReadEntry;
                (*entry).address = addr;
                (*entry).length = buf.len() as u32;
                (*entry).data_offset = size_of::<RcxRpcReadEntry>() as u32;
                if !self.signal_and_wait(2000) {
                    buf.fill(0);
                    return false;
                }
                copy_nonoverlapping(
                    data.add((*entry).data_offset as usize),
                    buf.as_mut_ptr(),
                    buf.len(),
                );
                true
            }
        }

        fn read_pages(&self, pages: &[u64]) -> PageMap {
            let mut out = PageMap::new();
            if pages.is_empty() {
                return out;
            }

            let normalized_pages = normalize_page_list(pages);
            let pages = normalized_pages.as_slice();
            out.reserve(pages.len());

            let max_entries = rpc_read_page_capacity();
            for chunk in pages.chunks(max_entries) {
                self.read_page_batch(chunk, &mut out);
            }
            out
        }

        fn read_page_batch(&self, pages: &[u64], out: &mut PageMap) {
            let Ok(_guard) = self.lock.lock() else {
                super::insert_zero_pages(pages, out);
                return;
            };
            unsafe {
                let hdr = &mut *self.header_ptr();
                let data = self.data_ptr();
                let entry_size = size_of::<RcxRpcReadEntry>();
                let table_len = pages.len() * entry_size;
                hdr.command = RPC_CMD_READ_BATCH;
                hdr.request_count = pages.len() as u32;
                hdr.status = RCX_RPC_STATUS_OK;
                hdr.response_count = 0;
                hdr.total_data_used = 0;
                for (i, &page_addr) in pages.iter().enumerate() {
                    let entry = data.add(i * entry_size) as *mut RcxRpcReadEntry;
                    (*entry).address = page_addr;
                    (*entry).length = K_PAGE_SIZE as u32;
                    (*entry).data_offset = (table_len + i * K_PAGE_SIZE as usize) as u32;
                }

                if !self.signal_and_wait(2000) {
                    super::insert_zero_pages(pages, out);
                    return;
                }

                let payload = std::slice::from_raw_parts(data, RCX_RPC_DATA_SIZE);
                super::insert_read_batch_pages_from_payload(
                    pages,
                    payload,
                    hdr.response_count as usize,
                    out,
                );
            }
        }

        fn write_single(&self, addr: u64, data_in: &[u8]) -> bool {
            if data_in.is_empty() || data_in.len() > RCX_RPC_DATA_SIZE {
                return false;
            }
            let Ok(_guard) = self.lock.lock() else {
                return false;
            };
            unsafe {
                let hdr = &mut *self.header_ptr();
                let data = self.data_ptr();
                hdr.command = RPC_CMD_WRITE;
                hdr.write_address = addr;
                hdr.write_length = data_in.len() as u32;
                hdr.status = RCX_RPC_STATUS_OK;
                copy_nonoverlapping(data_in.as_ptr(), data, data_in.len());
                self.signal_and_wait(2000) && hdr.status == RCX_RPC_STATUS_OK
            }
        }

        fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            let Ok(_guard) = self.lock.lock() else {
                return Vec::new();
            };
            unsafe {
                let hdr = &mut *self.header_ptr();
                hdr.command = RPC_CMD_ENUM_MODULES;
                hdr.status = RCX_RPC_STATUS_OK;
                if !self.signal_and_wait(3000) || hdr.status != RCX_RPC_STATUS_OK {
                    return Vec::new();
                }
                let data = self.data_ptr();
                let count = hdr.response_count as usize;
                let max_entries = (RCX_RPC_DATA_SIZE / size_of::<RcxRpcModuleEntry>()).min(count);
                let mut out = Vec::with_capacity(max_entries);
                for i in 0..max_entries {
                    let off = i * size_of::<RcxRpcModuleEntry>();
                    let entry = from_bytes::<RcxRpcModuleEntry>(std::slice::from_raw_parts(
                        data.add(off),
                        size_of::<RcxRpcModuleEntry>(),
                    ));
                    let name_off = entry.name_offset as usize;
                    let name_len = entry.name_length as usize;
                    if name_off.saturating_add(name_len) > RCX_RPC_DATA_SIZE {
                        continue;
                    }
                    let raw = std::slice::from_raw_parts(data.add(name_off), name_len);
                    let name = String::from_utf8_lossy(raw).into_owned();
                    out.push(ModuleEntry {
                        name,
                        full_path: String::new(),
                        base: entry.base,
                        size: entry.size,
                    });
                }
                out
            }
        }

        fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            let Ok(_guard) = self.lock.lock() else {
                return Vec::new();
            };
            unsafe {
                let hdr = &mut *self.header_ptr();
                hdr.command = RPC_CMD_ENUM_REGIONS;
                hdr.status = RCX_RPC_STATUS_OK;
                if !self.signal_and_wait(3000) || hdr.status != RCX_RPC_STATUS_OK {
                    return Vec::new();
                }
                let data = self.data_ptr();
                let count = hdr.response_count as usize;
                let max_entries = (RCX_RPC_DATA_SIZE / size_of::<RcxRpcRegionEntry>()).min(count);
                let mut out = Vec::with_capacity(max_entries);
                for i in 0..max_entries {
                    let off = i * size_of::<RcxRpcRegionEntry>();
                    let entry = from_bytes::<RcxRpcRegionEntry>(std::slice::from_raw_parts(
                        data.add(off),
                        size_of::<RcxRpcRegionEntry>(),
                    ));
                    let name_off = entry.name_offset as usize;
                    let name_len = entry.name_length as usize;
                    if name_off.saturating_add(name_len) > RCX_RPC_DATA_SIZE {
                        continue;
                    }
                    let raw = std::slice::from_raw_parts(data.add(name_off), name_len);
                    out.push(MemoryRegion {
                        base: entry.base,
                        size: entry.size,
                        readable: (entry.flags & RCX_RPC_REGION_READABLE) != 0,
                        writable: (entry.flags & RCX_RPC_REGION_WRITABLE) != 0,
                        executable: (entry.flags & RCX_RPC_REGION_EXECUTABLE) != 0,
                        module_name: String::from_utf8_lossy(raw).into_owned(),
                        region_type: rpc_region_type(entry.region_type),
                    });
                }
                out
            }
        }
    }

    fn rpc_region_type(kind: u32) -> RegionType {
        match kind {
            RCX_RPC_REGION_IMAGE => RegionType::Image,
            RCX_RPC_REGION_MAPPED => RegionType::Mapped,
            _ => RegionType::Private,
        }
    }

    fn rpc_read_page_capacity() -> usize {
        (RCX_RPC_DATA_SIZE / (size_of::<RcxRpcReadEntry>() + K_PAGE_SIZE as usize))
            .min(RCX_RPC_MAX_BATCH)
            .max(1)
    }

    impl Drop for IpcClient {
        fn drop(&mut self) {
            unsafe {
                self.connected.store(false, Ordering::Release);
                if !self.view.is_null() {
                    libc::munmap(self.view, RCX_RPC_SHM_SIZE);
                    self.view = null_mut();
                }
                if self.shm_fd >= 0 {
                    libc::close(self.shm_fd);
                    self.shm_fd = -1;
                }
                if !sem_failed(self.req_sem) {
                    libc::sem_close(self.req_sem);
                    self.req_sem = libc::SEM_FAILED;
                }
                if !sem_failed(self.rsp_sem) {
                    libc::sem_close(self.rsp_sem);
                    self.rsp_sem = libc::SEM_FAILED;
                }
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    unsafe fn inject_payload_impl(pid: u32) -> Result<(), String> {
        let path = payload_path()?;
        let path_c = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| format!("payload path contains a NUL byte: {}", path.display()))?;
        let path_bytes = path_c.as_bytes_with_nul();
        if path_bytes.len() + 64 > 4096 {
            return Err(format!("payload path is too long: {}", path.display()));
        }

        let traced = Pid::from_raw(pid as i32);
        ptrace::attach(traced).map_err(|err| {
            format!(
                "ptrace attach failed for PID {pid}: {err}. Check /proc/sys/kernel/yama/ptrace_scope or run as root."
            )
        })?;
        let mut guard = TraceGuard::new(traced);
        wait_for_trace_stop(traced)?;
        let saved =
            ptrace::getregs(traced).map_err(|err| format!("PTRACE_GETREGS failed: {err}"))?;
        guard.saved = Some(saved);

        let syscall_addr = find_syscall_insn(traced)?;
        let dlopen = CString::new("dlopen").expect("literal contains no NUL");
        let our_dlopen = libc::dlsym(libc::RTLD_DEFAULT, dlopen.as_ptr()) as u64;
        let our_libc_base = find_lib_base(Pid::from_raw(std::process::id() as i32), "libc")?;
        let target_libc_base = find_lib_base(traced, "libc")?;
        if our_dlopen == 0 || our_libc_base == 0 || target_libc_base == 0 {
            return Err("Could not resolve dlopen address".to_string());
        }
        let dlopen_offset = our_dlopen
            .checked_sub(our_libc_base)
            .ok_or_else(|| "Resolved dlopen is below local libc base".to_string())?;
        let target_dlopen = target_libc_base.saturating_add(dlopen_offset);

        let mut regs = saved;
        regs.rax = libc::SYS_mmap as u64;
        regs.rdi = 0;
        regs.rsi = 4096;
        regs.rdx = (libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC) as u64;
        regs.r10 = (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64;
        regs.r8 = u64::MAX;
        regs.r9 = 0;
        regs.rip = syscall_addr;
        ptrace::setregs(traced, regs)
            .map_err(|err| format!("PTRACE_SETREGS(mmap) failed: {err}"))?;
        ptrace::step(traced, None::<Signal>)
            .map_err(|err| format!("PTRACE_SINGLESTEP(mmap) failed: {err}"))?;
        wait_for_trace_stop(traced)?;
        regs =
            ptrace::getregs(traced).map_err(|err| format!("PTRACE_GETREGS(mmap) failed: {err}"))?;
        let mmap_page = regs.rax;
        if mmap_page == 0 || (mmap_page as i64) < 0 {
            return Err("mmap in target failed".to_string());
        }

        write_target_mem(traced, mmap_page, path_bytes)?;
        let code_addr = mmap_page + (((path_bytes.len() as u64) + 15) & !15);
        let shellcode = dlopen_shellcode(mmap_page, target_dlopen);
        write_target_mem(traced, code_addr, &shellcode)?;

        regs = saved;
        regs.rip = code_addr;
        regs.rsp = (mmap_page + 4096) & !0xf;
        ptrace::setregs(traced, regs)
            .map_err(|err| format!("PTRACE_SETREGS(dlopen) failed: {err}"))?;
        ptrace::cont(traced, None::<Signal>)
            .map_err(|err| format!("PTRACE_CONT(dlopen) failed: {err}"))?;
        let stopped =
            waitpid(traced, None).map_err(|err| format!("waitpid(dlopen) failed: {err}"))?;
        let ok = matches!(stopped, WaitStatus::Stopped(_, Signal::SIGTRAP))
            && ptrace::getregs(traced)
                .map(|after| after.rax != 0)
                .unwrap_or(false);

        let mut clean_regs = saved;
        clean_regs.rax = libc::SYS_munmap as u64;
        clean_regs.rdi = mmap_page;
        clean_regs.rsi = 4096;
        clean_regs.rip = syscall_addr;
        let _ = ptrace::setregs(traced, clean_regs);
        let _ = ptrace::step(traced, None::<Signal>);
        let _ = waitpid(traced, None);

        guard.restore_detach();
        if ok {
            Ok(())
        } else {
            Err(format!(
                "dlopen failed in target. Ensure payload is at {}",
                path.display()
            ))
        }
    }

    fn payload_path() -> Result<PathBuf, String> {
        if let Ok(path) = env::var("RECLASS_RCX_PAYLOAD_PATH") {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Ok(path);
            }
        }
        let exe = env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
        let mut candidates = Vec::new();
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("Plugins").join("librcx_payload.so"));
            candidates.push(dir.join("Plugins").join("rcx_payload.so"));
            candidates.push(dir.join("plugins").join("librcx_payload.so"));
            candidates.push(dir.join("plugins").join("rcx_payload.so"));
            candidates.push(dir.join("librcx_payload.so"));
            candidates.push(dir.join("rcx_payload.so"));
            candidates.push(dir.join("deps").join("librcx_payload.so"));
            candidates.push(dir.join("deps").join("rcx_payload.so"));
        }
        if let Some(dir) = exe.parent().and_then(|p| p.parent()) {
            candidates.push(dir.join("deps").join("librcx_payload.so"));
            candidates.push(dir.join("deps").join("rcx_payload.so"));
        }
        for candidate in candidates {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err("librcx_payload.so was not found beside the executable or in target deps; set RECLASS_RCX_PAYLOAD_PATH".to_string())
    }

    #[cfg(target_arch = "x86_64")]
    struct TraceGuard {
        pid: Pid,
        saved: Option<libc::user_regs_struct>,
        detached: bool,
    }

    #[cfg(target_arch = "x86_64")]
    impl TraceGuard {
        fn new(pid: Pid) -> Self {
            Self {
                pid,
                saved: None,
                detached: false,
            }
        }

        fn restore_detach(&mut self) {
            if self.detached {
                return;
            }
            if let Some(regs) = self.saved {
                let _ = ptrace::setregs(self.pid, regs);
            }
            let _ = ptrace::detach(self.pid, None::<Signal>);
            self.detached = true;
        }
    }

    #[cfg(target_arch = "x86_64")]
    impl Drop for TraceGuard {
        fn drop(&mut self) {
            self.restore_detach();
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn wait_for_trace_stop(pid: Pid) -> Result<(), String> {
        match waitpid(pid, None).map_err(|err| format!("waitpid failed: {err}"))? {
            WaitStatus::Stopped(_, _) => Ok(()),
            status => Err(format!("target did not stop under ptrace: {status:?}")),
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn find_lib_base(pid: Pid, lib_name: &str) -> Result<u64, String> {
        let proc = Process::new(pid.as_raw())
            .map_err(|err| format!("open /proc/{}: {err}", pid.as_raw()))?;
        let maps = proc
            .maps()
            .map_err(|err| format!("read /proc/{}/maps: {err}", pid.as_raw()))?;
        maps.0
            .iter()
            .find_map(|map| {
                let path = map_path(&map.pathname)?;
                path.contains(lib_name).then_some(map.address.0)
            })
            .ok_or_else(|| format!("Could not find {lib_name} base in PID {}", pid.as_raw()))
    }

    #[cfg(target_arch = "x86_64")]
    fn find_syscall_insn(pid: Pid) -> Result<u64, String> {
        let proc = Process::new(pid.as_raw())
            .map_err(|err| format!("open /proc/{}: {err}", pid.as_raw()))?;
        let maps = proc
            .maps()
            .map_err(|err| format!("read /proc/{}/maps: {err}", pid.as_raw()))?;
        let libc_exec = maps
            .0
            .iter()
            .find(|map| {
                map.perms.contains(MMPermissions::EXECUTE)
                    && map_path(&map.pathname)
                        .map(|path| path.contains("libc"))
                        .unwrap_or(false)
            })
            .ok_or_else(|| {
                format!(
                    "Could not find executable libc mapping in PID {}",
                    pid.as_raw()
                )
            })?;
        let mem = File::open(format!("/proc/{}/mem", pid.as_raw()))
            .map_err(|err| format!("open /proc/{}/mem: {err}", pid.as_raw()))?;
        let mut buf = [0u8; 4096];
        let mut prev = None;
        let mut off = libc_exec.address.0;
        while off < libc_exec.address.1 {
            let want = (libc_exec.address.1 - off).min(buf.len() as u64) as usize;
            let n = match mem.read_at(&mut buf[..want], off) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            if prev == Some(0x0f) && buf[0] == 0x05 {
                return Ok(off - 1);
            }
            for i in 0..n.saturating_sub(1) {
                if buf[i] == 0x0f && buf[i + 1] == 0x05 {
                    return Ok(off + i as u64);
                }
            }
            prev = Some(buf[n - 1]);
            off += n as u64;
        }
        Err(format!(
            "Could not find syscall instruction in PID {}",
            pid.as_raw()
        ))
    }

    #[cfg(target_arch = "x86_64")]
    fn write_target_mem(pid: Pid, addr: u64, src: &[u8]) -> Result<(), String> {
        let word_size = size_of::<libc::c_long>();
        for i in (0..src.len()).step_by(word_size) {
            let chunk = (src.len() - i).min(word_size);
            let mut word = if chunk < word_size {
                ptrace::read(pid, (addr + i as u64) as *mut c_void).map_err(|err| {
                    format!("PTRACE_PEEKDATA failed at 0x{:x}: {err}", addr + i as u64)
                })?
            } else {
                0
            };
            unsafe {
                copy_nonoverlapping(
                    src.as_ptr().add(i),
                    (&mut word as *mut libc::c_long).cast::<u8>(),
                    chunk,
                );
            }
            ptrace::write(pid, (addr + i as u64) as *mut c_void, word).map_err(|err| {
                format!("PTRACE_POKEDATA failed at 0x{:x}: {err}", addr + i as u64)
            })?;
        }
        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn dlopen_shellcode(path_addr: u64, dlopen_addr: u64) -> Vec<u8> {
        let mut sc = Vec::with_capacity(64);
        sc.extend_from_slice(&[0x48, 0xBF]);
        sc.extend_from_slice(&path_addr.to_le_bytes());
        sc.extend_from_slice(&[0x48, 0xBE]);
        sc.extend_from_slice(&2u64.to_le_bytes());
        sc.extend_from_slice(&[0x48, 0xB8]);
        sc.extend_from_slice(&dlopen_addr.to_le_bytes());
        sc.extend_from_slice(&[0xFF, 0xD0]);
        sc.push(0xCC);
        sc
    }

    fn map_path(path: &MMapPath) -> Option<String> {
        match path {
            MMapPath::Path(path) => path.to_str().map(ToString::to_string),
            MMapPath::Other(path) => Some(path.clone()),
            _ => None,
        }
    }

    unsafe fn timespec_after(timeout_ms: u32) -> libc::timespec {
        let mut ts: libc::timespec = zeroed();
        libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts);
        ts.tv_sec += (timeout_ms / 1000) as libc::time_t;
        ts.tv_nsec += ((timeout_ms % 1000) * 1_000_000) as libc::c_long;
        if ts.tv_nsec >= 1_000_000_000 {
            ts.tv_sec += 1;
            ts.tv_nsec -= 1_000_000_000;
        }
        ts
    }

    fn sem_failed(sem: *mut libc::sem_t) -> bool {
        sem.is_null() || sem == libc::SEM_FAILED
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use super::RemoteProcessTarget;
    use crate::provider::{MemoryRegion, ModuleEntry, PageMap};

    pub struct Inner {
        target: RemoteProcessTarget,
    }

    impl Inner {
        pub fn attach(target: RemoteProcessTarget) -> Result<Self, String> {
            Err(format!(
                "Remote Process Memory is implemented on Windows and Linux; PID {} ({}) is not attachable on this platform yet",
                target.pid, target.name
            ))
        }
        pub fn read(&self, _addr: u64, _buf: &mut [u8]) -> bool {
            false
        }
        pub fn read_pages(&self, _pages: &[u64]) -> PageMap {
            PageMap::new()
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
        pub fn get_symbol(&self, _addr: u64) -> String {
            String::new()
        }
        pub fn symbol_to_address(&self, _name: &str) -> u64 {
            0
        }
        pub fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            Vec::new()
        }
        pub fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            Vec::new()
        }
    }

    pub fn inject_payload(pid: u32) -> Result<(), String> {
        Err(format!(
            "Remote Process Memory payload injection is implemented on Windows and Linux; PID {pid} was not modified"
        ))
    }
}

#[cfg(test)]
mod tests {
    use rcx_rpc::RCX_RPC_DATA_SIZE;

    use super::*;

    #[test]
    fn batch_payload_insert_copies_responses_and_zero_fills_missing_pages() {
        let pages = [0x1000_u64, 0x2000, 0x3000];
        let entry_size = std::mem::size_of::<RcxRpcReadEntry>();
        let page_len = K_PAGE_SIZE as usize;
        let mut data = vec![0u8; RCX_RPC_DATA_SIZE];
        let table_len = pages.len() * entry_size;

        for (idx, &page_addr) in pages.iter().take(2).enumerate() {
            let data_offset = table_len + idx * page_len;
            let entry = RcxRpcReadEntry {
                address: page_addr,
                length: K_PAGE_SIZE as u32,
                data_offset: data_offset as u32,
            };
            let entry_start = idx * entry_size;
            data[entry_start..entry_start + entry_size].copy_from_slice(bytemuck::bytes_of(&entry));
            data[data_offset..data_offset + page_len].fill(0xA0 + idx as u8);
        }

        let mut out = PageMap::new();
        insert_read_batch_pages_from_payload(&pages, &data, 2, &mut out);

        assert_eq!(out.len(), pages.len());
        assert_eq!(
            out.get(&pages[0]).and_then(|page| page.first()),
            Some(&0xA0)
        );
        assert_eq!(
            out.get(&pages[1]).and_then(|page| page.first()),
            Some(&0xA1)
        );
        assert!(out
            .get(&pages[2])
            .is_some_and(|page| page.iter().all(|byte| *byte == 0)));
    }

    #[test]
    fn parses_remote_targets() {
        assert_eq!(
            RemoteProcessTarget::parse("rpm:4321:game.exe").unwrap(),
            RemoteProcessTarget {
                pid: 4321,
                name: "game.exe".to_string()
            }
        );
        assert_eq!(
            RemoteProcessTarget::parse("rpm:4321:C:\\x\\game.exe")
                .unwrap()
                .name,
            "C:\\x\\game.exe"
        );
        assert!(RemoteProcessTarget::parse("4321:game.exe").is_err());
        assert!(RemoteProcessTarget::parse("rpm:0:game.exe").is_err());
        assert!(RemoteProcessTarget::parse("rpm:1").is_err());
    }
}
