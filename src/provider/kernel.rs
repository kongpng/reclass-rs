//! Kernel Memory provider.
//!
//! Target strings mirror the C++ plugin:
//! - `"km:{pid}:{name}"` for process virtual memory through `rcxdrv.sys`
//! - `"phys:{hex_base}"` for physical memory through `rcxdrv.sys`
//! - `"msr:{...}"` is recognized by `can_handle` for C++ plugin parity, but
//!   provider creation returns an unsupported-target error.

use crate::plugin::contract::ProcessInfo;

use super::{
    read_pages_in_runs, MemoryRegion, ModuleEntry, PageMap, Provider, ThreadInfo, VtopResult,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KernelTarget {
    Process { pid: u32, name: String },
    Physical { base: u64 },
}

impl KernelTarget {
    pub fn parse(target: &str) -> Result<Self, String> {
        let trimmed = target.trim();
        if let Some(rest) = trimmed.strip_prefix("km:") {
            let mut parts = rest.split(':');
            let Some(pid_text) = parts.next() else {
                return Err("kernel process target is missing PID".to_string());
            };
            let pid = pid_text
                .parse::<u32>()
                .map_err(|_| format!("invalid kernel process PID: {pid_text}"))?;
            if pid == 0 {
                return Err("invalid kernel process PID: 0".to_string());
            }
            let name = parts.collect::<Vec<_>>().join(":");
            return Ok(KernelTarget::Process {
                pid,
                name: if name.is_empty() {
                    format!("PID {pid}")
                } else {
                    name
                },
            });
        }
        if let Some(rest) = trimmed.strip_prefix("phys:") {
            let hex = rest
                .trim()
                .trim_start_matches("0x")
                .trim_start_matches("0X");
            let base = if hex.is_empty() {
                0
            } else {
                u64::from_str_radix(hex, 16)
                    .map_err(|_| format!("invalid physical base address: {rest}"))?
            };
            return Ok(KernelTarget::Physical { base });
        }
        if trimmed.strip_prefix("msr:").is_some() {
            return Err(format!(
                "unsupported kernel MSR target: {target}; only km: and phys: are implemented"
            ));
        }
        Err(format!("unsupported kernel memory target: {target}"))
    }

    pub fn to_target(&self) -> String {
        match self {
            KernelTarget::Process { pid, name } => format!("km:{pid}:{name}"),
            KernelTarget::Physical { base } => format!("phys:{base:x}"),
        }
    }
}

pub struct KernelMemoryProvider {
    inner: platform::Inner,
}

impl KernelMemoryProvider {
    pub fn attach(target: &str) -> Result<Self, String> {
        let target = KernelTarget::parse(target)?;
        platform::Inner::attach(target).map(|inner| Self { inner })
    }

    pub fn can_handle(target: &str) -> bool {
        let trimmed = target.trim();
        trimmed.starts_with("km:") || trimmed.starts_with("phys:") || trimmed.starts_with("msr:")
    }

    pub fn enumerate_processes() -> Vec<ProcessInfo> {
        platform::enumerate_processes()
    }

    pub fn unload_driver_service() -> Result<(), String> {
        platform::unload_driver_service()
    }
}

impl Provider for KernelMemoryProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.inner.read(addr, buf)
    }

    fn read_pages(&self, pages: &[u64]) -> PageMap {
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
        self.inner.kind()
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

    fn is_readable(&self, _addr: u64, len: i32) -> bool {
        self.inner.size() > 0 && len >= 0
    }

    fn peb(&self) -> u64 {
        self.inner.peb()
    }

    fn tebs(&self) -> Vec<ThreadInfo> {
        self.inner.tebs()
    }

    fn enumerate_modules(&self) -> Vec<ModuleEntry> {
        self.inner.enumerate_modules()
    }

    fn has_kernel_paging(&self) -> bool {
        self.inner.has_kernel_paging()
    }

    fn get_cr3(&self) -> u64 {
        self.inner.get_cr3()
    }

    fn translate_address(&self, va: u64) -> VtopResult {
        self.inner.translate_address(va)
    }

    fn read_page_table(&self, phys_addr: u64, start_idx: i32, count: i32) -> Vec<u64> {
        self.inner.read_page_table(phys_addr, start_idx, count)
    }
}

#[cfg(windows)]
mod platform {
    use std::env;
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::path::PathBuf;
    use std::ptr::{copy_nonoverlapping, null, null_mut};
    use std::sync::Mutex;

    use crate::plugin::contract::ProcessInfo;
    use crate::provider::{
        MemoryRegion, ModuleEntry, ModuleLookup, RegionType, ThreadInfo, VtopResult,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_SERVICE_ALREADY_RUNNING, GENERIC_READ, GENERIC_WRITE,
        HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileA, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, ControlService, CreateServiceW, DeleteService, OpenSCManagerW,
        OpenServiceW, StartServiceW, SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS,
        SERVICE_CONTROL_STOP, SERVICE_DEMAND_START, SERVICE_ERROR_NORMAL, SERVICE_KERNEL_DRIVER,
        SERVICE_STATUS,
    };
    use windows_sys::Win32::System::Threading::{
        IsWow64Process, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    use super::KernelTarget;

    const RCX_DRV_USERMODE_PATH: &[u8] = b"\\\\.\\RcxDrv\0";
    const RCX_DRV_SERVICE_NAME: &str = "RcxDrv";
    const RCX_DRV_VERSION: u32 = 1;
    const RCX_DRV_MAX_VIRTUAL: usize = 1024 * 1024;
    const RCX_DRV_MAX_PHYSICAL: usize = 4096;
    const IOCTL_RCX_READ_MEMORY: u32 = 0x222000;
    const IOCTL_RCX_WRITE_MEMORY: u32 = 0x222004;
    const IOCTL_RCX_QUERY_REGIONS: u32 = 0x222008;
    const IOCTL_RCX_QUERY_PEB: u32 = 0x22200C;
    const IOCTL_RCX_QUERY_MODULES: u32 = 0x222010;
    const IOCTL_RCX_QUERY_TEBS: u32 = 0x222014;
    const IOCTL_RCX_PING: u32 = 0x222018;
    const IOCTL_RCX_READ_PHYS: u32 = 0x22201C;
    const IOCTL_RCX_WRITE_PHYS: u32 = 0x222020;
    const IOCTL_RCX_READ_CR3: u32 = 0x222044;
    const IOCTL_RCX_VTOP: u32 = 0x222048;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvReadRequest {
        pid: u32,
        _pad0: u32,
        address: u64,
        length: u32,
        _pad1: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvWriteRequest {
        pid: u32,
        _pad0: u32,
        address: u64,
        length: u32,
        _pad1: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvQueryRegionsRequest {
        pid: u32,
        _pad: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvRegionEntry {
        base: u64,
        size: u64,
        protect: u32,
        state: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvQueryPebRequest {
        pid: u32,
        _pad: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvQueryPebResponse {
        peb_address: u64,
        pointer_size: u32,
        _pad: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvQueryModulesRequest {
        pid: u32,
        _pad: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RcxDrvModuleEntry {
        base: u64,
        size: u64,
        name: [u16; 260],
    }

    impl Default for RcxDrvModuleEntry {
        fn default() -> Self {
            Self {
                base: 0,
                size: 0,
                name: [0; 260],
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvQueryTebsRequest {
        pid: u32,
        _pad: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvTebEntry {
        teb_address: u64,
        thread_id: u32,
        _pad: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvPingResponse {
        version: u32,
        driver_build: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvPhysReadRequest {
        phys_address: u64,
        length: u32,
        width: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvPhysWriteRequest {
        phys_address: u64,
        length: u32,
        width: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvReadCr3Request {
        pid: u32,
        _pad: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvReadCr3Response {
        cr3: u64,
        kernel_cr3: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvVtopRequest {
        pid: u32,
        _pad: u32,
        virtual_address: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RcxDrvVtopResponse {
        physical_address: u64,
        pml4e: u64,
        pdpte: u64,
        pde: u64,
        pte: u64,
        page_size: u8,
        valid: u8,
        _pad2: [u8; 6],
    }

    const _: () = assert!(size_of::<RcxDrvReadRequest>() == 24);
    const _: () = assert!(size_of::<RcxDrvWriteRequest>() == 24);
    const _: () = assert!(size_of::<RcxDrvRegionEntry>() == 24);
    const _: () = assert!(size_of::<RcxDrvModuleEntry>() == 536);
    const _: () = assert!(size_of::<RcxDrvTebEntry>() == 16);
    const _: () = assert!(size_of::<RcxDrvPingResponse>() == 8);
    const _: () = assert!(size_of::<RcxDrvReadCr3Response>() == 16);
    const _: () = assert!(size_of::<RcxDrvVtopRequest>() == 16);
    const _: () = assert!(size_of::<RcxDrvVtopResponse>() == 48);

    pub(super) enum Inner {
        Process(KernelProcessInner),
        Physical(KernelPhysInner),
    }

    impl Inner {
        pub fn attach(target: KernelTarget) -> Result<Self, String> {
            let driver = Driver::connect()?;
            match target {
                KernelTarget::Process { pid, name } => {
                    let mut inner = KernelProcessInner {
                        driver,
                        pid,
                        name,
                        base: 0,
                        pointer_size: 8,
                        peb: 0,
                        cr3_cache: Mutex::new(0),
                        modules: Vec::new(),
                        module_lookup: ModuleLookup::default(),
                    };
                    inner.query_peb();
                    inner.cache_modules();
                    if inner.size() == 0 {
                        return Err(format!(
                            "Failed to read process {} (PID: {}) through rcxdrv.sys",
                            inner.name, inner.pid
                        ));
                    }
                    Ok(Inner::Process(inner))
                }
                KernelTarget::Physical { base } => {
                    Ok(Inner::Physical(KernelPhysInner { driver, base }))
                }
            }
        }

        pub fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            match self {
                Inner::Process(p) => p.read(addr, buf),
                Inner::Physical(p) => p.read(addr, buf),
            }
        }

        pub fn write(&self, addr: u64, data: &[u8]) -> bool {
            match self {
                Inner::Process(p) => p.write(addr, data),
                Inner::Physical(p) => p.write(addr, data),
            }
        }

        pub fn size(&self) -> i32 {
            match self {
                Inner::Process(p) => p.size(),
                Inner::Physical(p) => p.size(),
            }
        }

        pub fn is_writable(&self) -> bool {
            self.size() > 0
        }

        pub fn name(&self) -> String {
            match self {
                Inner::Process(p) => p.name.clone(),
                Inner::Physical(_) => "Physical Memory".to_string(),
            }
        }

        pub fn kind(&self) -> String {
            match self {
                Inner::Process(_) => "KernelProcess".to_string(),
                Inner::Physical(_) => "Physical".to_string(),
            }
        }

        pub fn pointer_size(&self) -> i32 {
            match self {
                Inner::Process(p) => p.pointer_size,
                Inner::Physical(_) => 8,
            }
        }

        pub fn base(&self) -> u64 {
            match self {
                Inner::Process(p) => p.base,
                Inner::Physical(p) => p.base,
            }
        }

        pub fn get_symbol(&self, addr: u64) -> String {
            match self {
                Inner::Process(p) => p.get_symbol(addr),
                Inner::Physical(_) => String::new(),
            }
        }

        pub fn symbol_to_address(&self, name: &str) -> u64 {
            match self {
                Inner::Process(p) => p.symbol_to_address(name),
                Inner::Physical(_) => 0,
            }
        }

        pub fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            match self {
                Inner::Process(p) => p.enumerate_regions(),
                Inner::Physical(_) => Vec::new(),
            }
        }

        pub fn peb(&self) -> u64 {
            match self {
                Inner::Process(p) => p.peb,
                Inner::Physical(_) => 0,
            }
        }

        pub fn tebs(&self) -> Vec<ThreadInfo> {
            match self {
                Inner::Process(p) => p.tebs(),
                Inner::Physical(_) => Vec::new(),
            }
        }

        pub fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            match self {
                Inner::Process(p) => p.modules.clone(),
                Inner::Physical(_) => Vec::new(),
            }
        }

        pub fn has_kernel_paging(&self) -> bool {
            matches!(self, Inner::Process(_))
        }

        pub fn get_cr3(&self) -> u64 {
            match self {
                Inner::Process(p) => p.get_cr3(),
                Inner::Physical(_) => 0,
            }
        }

        pub fn translate_address(&self, va: u64) -> VtopResult {
            match self {
                Inner::Process(p) => p.translate_address(va),
                Inner::Physical(_) => VtopResult::default(),
            }
        }

        pub fn read_page_table(&self, phys_addr: u64, start_idx: i32, count: i32) -> Vec<u64> {
            match self {
                Inner::Process(p) => p.read_page_table(phys_addr, start_idx, count),
                Inner::Physical(p) => p.read_page_table(phys_addr, start_idx, count),
            }
        }
    }

    pub fn enumerate_processes() -> Vec<ProcessInfo> {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return Vec::new();
            }
            let mut entry: PROCESSENTRY32W = zeroed();
            entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            let mut out = Vec::new();
            if Process32FirstW(snap, &mut entry) != 0 {
                loop {
                    let pid = entry.th32ProcessID;
                    let name = wide_to_string(&entry.szExeFile);
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

    pub fn unload_driver_service() -> Result<(), String> {
        unsafe {
            let scm = OpenSCManagerW(null(), null(), SC_MANAGER_ALL_ACCESS);
            if scm.is_null() {
                return Err("Failed to open Service Control Manager".to_string());
            }
            let name = wide_z(RCX_DRV_SERVICE_NAME);
            let svc = OpenServiceW(scm, name.as_ptr(), SERVICE_ALL_ACCESS);
            if svc.is_null() {
                CloseServiceHandle(scm);
                return Err("RcxDrv service is not installed".to_string());
            }
            let mut status: SERVICE_STATUS = zeroed();
            ControlService(svc, SERVICE_CONTROL_STOP, &mut status);
            DeleteService(svc);
            CloseServiceHandle(svc);
            CloseServiceHandle(scm);
            Ok(())
        }
    }

    struct Driver {
        handle: HANDLE,
    }

    unsafe impl Send for Driver {}
    unsafe impl Sync for Driver {}

    impl Driver {
        fn connect() -> Result<Self, String> {
            unsafe {
                let mut handle = open_device();
                if handle != INVALID_HANDLE_VALUE && ping(handle) {
                    return Ok(Self { handle });
                }
                if handle != INVALID_HANDLE_VALUE {
                    CloseHandle(handle);
                }

                ensure_service_started()?;
                handle = open_device();
                if handle == INVALID_HANDLE_VALUE {
                    return Err("Driver started but \\\\.\\RcxDrv could not be opened".to_string());
                }
                if !ping(handle) {
                    CloseHandle(handle);
                    return Err("Driver opened but IOCTL_RCX_PING failed".to_string());
                }
                Ok(Self { handle })
            }
        }
    }

    impl Drop for Driver {
        fn drop(&mut self) {
            unsafe {
                if self.handle != INVALID_HANDLE_VALUE {
                    CloseHandle(self.handle);
                    self.handle = INVALID_HANDLE_VALUE;
                }
            }
        }
    }

    pub(super) struct KernelProcessInner {
        driver: Driver,
        pid: u32,
        name: String,
        base: u64,
        pointer_size: i32,
        peb: u64,
        cr3_cache: Mutex<u64>,
        modules: Vec<ModuleEntry>,
        module_lookup: ModuleLookup,
    }

    unsafe impl Send for KernelProcessInner {}
    unsafe impl Sync for KernelProcessInner {}

    impl KernelProcessInner {
        fn size(&self) -> i32 {
            if self.driver.handle == INVALID_HANDLE_VALUE {
                0
            } else {
                0x10000
            }
        }

        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            if buf.is_empty() || self.driver.handle == INVALID_HANDLE_VALUE {
                return false;
            }
            let mut any = false;
            let mut offset = 0usize;
            while offset < buf.len() {
                let chunk = (buf.len() - offset).min(RCX_DRV_MAX_VIRTUAL);
                let req = RcxDrvReadRequest {
                    pid: self.pid,
                    address: addr.saturating_add(offset as u64),
                    length: chunk as u32,
                    ..RcxDrvReadRequest::default()
                };
                let out = &mut buf[offset..offset + chunk];
                let br = unsafe {
                    ioctl_struct_out(self.driver.handle, IOCTL_RCX_READ_MEMORY, &req, out)
                };
                match br {
                    Some(n) if n > 0 => {
                        any = true;
                        if (n as usize) < chunk {
                            out[n as usize..].fill(0);
                        }
                    }
                    _ => {
                        out.fill(0);
                        if !any {
                            return false;
                        }
                    }
                }
                offset += chunk;
            }
            any
        }

        fn write(&self, addr: u64, data: &[u8]) -> bool {
            if data.is_empty() || self.driver.handle == INVALID_HANDLE_VALUE {
                return false;
            }
            let mut offset = 0usize;
            while offset < data.len() {
                let chunk = (data.len() - offset).min(RCX_DRV_MAX_VIRTUAL);
                let mut packet = vec![0u8; size_of::<RcxDrvWriteRequest>() + chunk];
                let req = packet.as_mut_ptr() as *mut RcxDrvWriteRequest;
                unsafe {
                    (*req).pid = self.pid;
                    (*req).address = addr.saturating_add(offset as u64);
                    (*req).length = chunk as u32;
                    copy_nonoverlapping(
                        data[offset..offset + chunk].as_ptr(),
                        packet.as_mut_ptr().add(size_of::<RcxDrvWriteRequest>()),
                        chunk,
                    );
                }
                if unsafe { ioctl_bytes(self.driver.handle, IOCTL_RCX_WRITE_MEMORY, &packet) }
                    .is_none()
                {
                    return false;
                }
                offset += chunk;
            }
            true
        }

        fn get_symbol(&self, addr: u64) -> String {
            self.module_lookup.symbol_for_addr_lower(addr)
        }

        fn symbol_to_address(&self, name: &str) -> u64 {
            self.module_lookup.symbol_to_address(name)
        }

        fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            let req = RcxDrvQueryRegionsRequest {
                pid: self.pid,
                _pad: 0,
            };
            let mut entries = vec![RcxDrvRegionEntry::default(); 8192];
            let out = unsafe { as_mut_bytes(entries.as_mut_slice()) };
            let Some(br) = (unsafe {
                ioctl_struct_out(self.driver.handle, IOCTL_RCX_QUERY_REGIONS, &req, out)
            }) else {
                return Vec::new();
            };
            let count = (br as usize / size_of::<RcxDrvRegionEntry>()).min(entries.len());
            let mut regions = Vec::with_capacity(count);
            for entry in entries.into_iter().take(count) {
                if (entry.state & 0x1000) == 0 {
                    continue;
                }
                let protect = entry.protect;
                if (protect & 0x01) != 0 || (protect & 0x100) != 0 {
                    continue;
                }
                let module_name = self
                    .modules
                    .iter()
                    .find(|m| entry.base >= m.base && entry.base < m.base.saturating_add(m.size))
                    .map(|m| m.name.clone())
                    .unwrap_or_default();
                regions.push(MemoryRegion {
                    base: entry.base,
                    size: entry.size,
                    readable: true,
                    writable: (protect & 0x04) != 0
                        || (protect & 0x08) != 0
                        || (protect & 0x40) != 0
                        || (protect & 0x80) != 0,
                    executable: (protect & 0x10) != 0
                        || (protect & 0x20) != 0
                        || (protect & 0x40) != 0
                        || (protect & 0x80) != 0,
                    module_name,
                    region_type: RegionType::Private,
                });
            }
            regions
        }

        fn query_peb(&mut self) {
            let req = RcxDrvQueryPebRequest {
                pid: self.pid,
                _pad: 0,
            };
            let mut resp = RcxDrvQueryPebResponse::default();
            if unsafe {
                ioctl_struct_struct(self.driver.handle, IOCTL_RCX_QUERY_PEB, &req, &mut resp)
            }
            .is_some()
            {
                self.peb = resp.peb_address;
                if resp.pointer_size == 4 {
                    self.pointer_size = 4;
                }
            }
        }

        fn tebs(&self) -> Vec<ThreadInfo> {
            let req = RcxDrvQueryTebsRequest {
                pid: self.pid,
                _pad: 0,
            };
            let mut entries = vec![RcxDrvTebEntry::default(); 4096];
            let out = unsafe { as_mut_bytes(entries.as_mut_slice()) };
            let Some(br) =
                (unsafe { ioctl_struct_out(self.driver.handle, IOCTL_RCX_QUERY_TEBS, &req, out) })
            else {
                return Vec::new();
            };
            let count = (br as usize / size_of::<RcxDrvTebEntry>()).min(entries.len());
            entries
                .into_iter()
                .take(count)
                .map(|e| ThreadInfo {
                    teb_address: e.teb_address,
                    thread_id: e.thread_id,
                })
                .collect()
        }

        fn cache_modules(&mut self) {
            let req = RcxDrvQueryModulesRequest {
                pid: self.pid,
                _pad: 0,
            };
            let mut entries = vec![RcxDrvModuleEntry::default(); 1024];
            let out = unsafe { as_mut_bytes(entries.as_mut_slice()) };
            let Some(br) = (unsafe {
                ioctl_struct_out(self.driver.handle, IOCTL_RCX_QUERY_MODULES, &req, out)
            }) else {
                return;
            };
            let count = (br as usize / size_of::<RcxDrvModuleEntry>()).min(entries.len());
            let mut modules = Vec::with_capacity(count);
            for (i, entry) in entries.into_iter().take(count).enumerate() {
                if i == 0 {
                    self.base = entry.base;
                }
                modules.push(ModuleEntry {
                    name: wide_to_string(&entry.name),
                    full_path: String::new(),
                    base: entry.base,
                    size: entry.size,
                });
            }
            self.module_lookup = ModuleLookup::new(modules.clone());
            self.modules = modules;
        }

        fn get_cr3(&self) -> u64 {
            if let Ok(cache) = self.cr3_cache.lock() {
                if *cache != 0 {
                    return *cache;
                }
            }
            let req = RcxDrvReadCr3Request {
                pid: self.pid,
                _pad: 0,
            };
            let mut resp = RcxDrvReadCr3Response::default();
            if unsafe {
                ioctl_struct_struct(self.driver.handle, IOCTL_RCX_READ_CR3, &req, &mut resp)
            }
            .is_some()
            {
                if let Ok(mut cache) = self.cr3_cache.lock() {
                    *cache = resp.cr3;
                }
                return resp.cr3;
            }
            0
        }

        fn translate_address(&self, va: u64) -> VtopResult {
            let req = RcxDrvVtopRequest {
                pid: self.pid,
                virtual_address: va,
                ..RcxDrvVtopRequest::default()
            };
            let mut resp = RcxDrvVtopResponse::default();
            if unsafe { ioctl_struct_struct(self.driver.handle, IOCTL_RCX_VTOP, &req, &mut resp) }
                .is_none()
            {
                return VtopResult::default();
            }
            VtopResult {
                physical: resp.physical_address,
                pml4e: resp.pml4e,
                pdpte: resp.pdpte,
                pde: resp.pde,
                pte: resp.pte,
                page_size: resp.page_size,
                valid: resp.valid != 0,
            }
        }

        fn read_page_table(&self, phys_addr: u64, start_idx: i32, count: i32) -> Vec<u64> {
            read_page_table_with_driver(&self.driver, phys_addr, start_idx, count)
        }
    }

    pub(super) struct KernelPhysInner {
        driver: Driver,
        base: u64,
    }

    unsafe impl Send for KernelPhysInner {}
    unsafe impl Sync for KernelPhysInner {}

    impl KernelPhysInner {
        fn size(&self) -> i32 {
            if self.driver.handle == INVALID_HANDLE_VALUE {
                0
            } else {
                0x10000
            }
        }

        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            read_physical(&self.driver, addr, buf)
        }

        fn write(&self, addr: u64, data: &[u8]) -> bool {
            write_physical(&self.driver, addr, data)
        }

        fn read_page_table(&self, phys_addr: u64, start_idx: i32, count: i32) -> Vec<u64> {
            read_page_table_with_driver(&self.driver, phys_addr, start_idx, count)
        }
    }

    fn read_physical(driver: &Driver, addr: u64, buf: &mut [u8]) -> bool {
        if buf.is_empty() || driver.handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut any = false;
        let mut offset = 0usize;
        while offset < buf.len() {
            let chunk = (buf.len() - offset).min(RCX_DRV_MAX_PHYSICAL);
            let req = RcxDrvPhysReadRequest {
                phys_address: addr.saturating_add(offset as u64),
                length: chunk as u32,
                width: 0,
            };
            let out = &mut buf[offset..offset + chunk];
            let br = unsafe { ioctl_struct_out(driver.handle, IOCTL_RCX_READ_PHYS, &req, out) };
            match br {
                Some(n) if n > 0 => {
                    any = true;
                    if (n as usize) < chunk {
                        out[n as usize..].fill(0);
                    }
                }
                _ => {
                    out.fill(0);
                    return any;
                }
            }
            offset += chunk;
        }
        any
    }

    fn write_physical(driver: &Driver, addr: u64, data: &[u8]) -> bool {
        if data.is_empty() || driver.handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut offset = 0usize;
        while offset < data.len() {
            let chunk = (data.len() - offset).min(RCX_DRV_MAX_PHYSICAL);
            let mut packet = vec![0u8; size_of::<RcxDrvPhysWriteRequest>() + chunk];
            let req = packet.as_mut_ptr() as *mut RcxDrvPhysWriteRequest;
            unsafe {
                (*req).phys_address = addr.saturating_add(offset as u64);
                (*req).length = chunk as u32;
                (*req).width = 0;
                copy_nonoverlapping(
                    data[offset..offset + chunk].as_ptr(),
                    packet.as_mut_ptr().add(size_of::<RcxDrvPhysWriteRequest>()),
                    chunk,
                );
            }
            if unsafe { ioctl_bytes(driver.handle, IOCTL_RCX_WRITE_PHYS, &packet) }.is_none() {
                return false;
            }
            offset += chunk;
        }
        true
    }

    fn read_page_table_with_driver(
        driver: &Driver,
        phys_addr: u64,
        start_idx: i32,
        count: i32,
    ) -> Vec<u64> {
        if start_idx < 0 || start_idx >= 512 || count <= 0 {
            return Vec::new();
        }
        let count = count.min(512 - start_idx);
        let mut bytes = vec![0u8; count as usize * size_of::<u64>()];
        let addr = phys_addr.saturating_add(start_idx as u64 * size_of::<u64>() as u64);
        if !read_physical(driver, addr, &mut bytes) {
            return Vec::new();
        }
        bytes
            .chunks_exact(size_of::<u64>())
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
            .collect()
    }

    unsafe fn open_device() -> HANDLE {
        CreateFileA(
            RCX_DRV_USERMODE_PATH.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    }

    unsafe fn ping(handle: HANDLE) -> bool {
        let mut ping = RcxDrvPingResponse::default();
        ioctl_no_in_struct(handle, IOCTL_RCX_PING, &mut ping).is_some()
            && ping.version == RCX_DRV_VERSION
    }

    unsafe fn ensure_service_started() -> Result<(), String> {
        let driver_path = driver_path()?;
        if !driver_path.is_file() {
            return Err(format!(
                "Driver not found: {}. Place rcxdrv.sys beside the executable in Plugins/ or set RECLASS_RCX_DRIVER_PATH.",
                driver_path.display()
            ));
        }

        let scm = OpenSCManagerW(null(), null(), SC_MANAGER_ALL_ACCESS);
        if scm.is_null() {
            return Err(
                "Failed to open Service Control Manager. Run Reclass as Administrator.".to_string(),
            );
        }

        let service_name = wide_z(RCX_DRV_SERVICE_NAME);
        let mut svc = OpenServiceW(scm, service_name.as_ptr(), SERVICE_ALL_ACCESS);
        if svc.is_null() {
            let path = wide_path(&driver_path);
            svc = CreateServiceW(
                scm,
                service_name.as_ptr(),
                service_name.as_ptr(),
                SERVICE_ALL_ACCESS,
                SERVICE_KERNEL_DRIVER,
                SERVICE_DEMAND_START,
                SERVICE_ERROR_NORMAL,
                path.as_ptr(),
                null(),
                null_mut(),
                null(),
                null(),
                null(),
            );
            if svc.is_null() {
                let err = GetLastError();
                CloseServiceHandle(scm);
                return Err(format!(
                    "Failed to create RcxDrv service (error {err}). Ensure test signing is enabled."
                ));
            }
        }

        if StartServiceW(svc, 0, null()) == 0 {
            let err = GetLastError();
            if err != ERROR_SERVICE_ALREADY_RUNNING {
                CloseServiceHandle(svc);
                CloseServiceHandle(scm);
                return Err(format!(
                    "Failed to start RcxDrv service (error {err}). Ensure the driver is signed."
                ));
            }
        }

        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
        Ok(())
    }

    fn driver_path() -> Result<PathBuf, String> {
        if let Ok(path) = env::var("RECLASS_RCX_DRIVER_PATH") {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Ok(path);
            }
        }
        let exe = env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
        let mut candidates = Vec::new();
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("Plugins").join("rcxdrv.sys"));
            candidates.push(dir.join("plugins").join("rcxdrv.sys"));
            candidates.push(dir.join("rcxdrv.sys"));
            candidates.push(dir.join("deps").join("rcxdrv.sys"));
        }
        if let Some(dir) = exe.parent().and_then(|p| p.parent()) {
            candidates.push(dir.join("deps").join("rcxdrv.sys"));
        }
        Ok(candidates
            .into_iter()
            .find(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("rcxdrv.sys")))
    }

    unsafe fn ioctl_no_in_struct<T>(handle: HANDLE, code: u32, output: &mut T) -> Option<u32> {
        let out = std::slice::from_raw_parts_mut(output as *mut T as *mut u8, size_of::<T>());
        ioctl_raw(handle, code, None, Some(out))
    }

    unsafe fn ioctl_struct_struct<I, O>(
        handle: HANDLE,
        code: u32,
        input: &I,
        output: &mut O,
    ) -> Option<u32> {
        let input = std::slice::from_raw_parts(input as *const I as *const u8, size_of::<I>());
        let output = std::slice::from_raw_parts_mut(output as *mut O as *mut u8, size_of::<O>());
        ioctl_raw(handle, code, Some(input), Some(output))
    }

    unsafe fn ioctl_struct_out<I>(
        handle: HANDLE,
        code: u32,
        input: &I,
        output: &mut [u8],
    ) -> Option<u32> {
        let input = std::slice::from_raw_parts(input as *const I as *const u8, size_of::<I>());
        ioctl_raw(handle, code, Some(input), Some(output))
    }

    unsafe fn ioctl_bytes(handle: HANDLE, code: u32, input: &[u8]) -> Option<u32> {
        ioctl_raw(handle, code, Some(input), None)
    }

    unsafe fn ioctl_raw(
        handle: HANDLE,
        code: u32,
        input: Option<&[u8]>,
        output: Option<&mut [u8]>,
    ) -> Option<u32> {
        let (in_ptr, in_len) = input
            .map(|s| (s.as_ptr() as *const c_void, s.len() as u32))
            .unwrap_or((null(), 0));
        let (out_ptr, out_len) = output
            .map(|s| (s.as_mut_ptr() as *mut c_void, s.len() as u32))
            .unwrap_or((null_mut(), 0));
        let mut bytes = 0u32;
        if DeviceIoControl(
            handle,
            code,
            in_ptr,
            in_len,
            out_ptr,
            out_len,
            &mut bytes,
            null_mut(),
        ) == 0
        {
            None
        } else {
            Some(bytes)
        }
    }

    unsafe fn as_mut_bytes<T>(slice: &mut [T]) -> &mut [u8] {
        std::slice::from_raw_parts_mut(slice.as_mut_ptr() as *mut u8, std::mem::size_of_val(slice))
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

    fn wide_to_string(buf: &[u16]) -> String {
        let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    fn wide_z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn wide_path(path: &std::path::Path) -> Vec<u16> {
        path.to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }
}

#[cfg(not(windows))]
mod platform {
    use crate::plugin::contract::ProcessInfo;
    use crate::provider::{MemoryRegion, ModuleEntry, ThreadInfo, VtopResult};

    use super::KernelTarget;

    pub struct Inner;

    impl Inner {
        pub fn attach(_target: KernelTarget) -> Result<Self, String> {
            Err(
                "Kernel Memory is implemented only on Windows because it requires rcxdrv.sys"
                    .to_string(),
            )
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
            String::new()
        }
        pub fn kind(&self) -> String {
            "Kernel".to_string()
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
        pub fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            Vec::new()
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
        pub fn has_kernel_paging(&self) -> bool {
            false
        }
        pub fn get_cr3(&self) -> u64 {
            0
        }
        pub fn translate_address(&self, _va: u64) -> VtopResult {
            VtopResult::default()
        }
        pub fn read_page_table(&self, _phys_addr: u64, _start_idx: i32, _count: i32) -> Vec<u64> {
            Vec::new()
        }
    }

    pub fn enumerate_processes() -> Vec<ProcessInfo> {
        Vec::new()
    }

    pub fn unload_driver_service() -> Result<(), String> {
        Err(
            "Kernel Memory is implemented only on Windows because it requires rcxdrv.sys"
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kernel_targets() {
        assert_eq!(
            KernelTarget::parse("km:4321:game.exe").unwrap(),
            KernelTarget::Process {
                pid: 4321,
                name: "game.exe".to_string()
            }
        );
        assert_eq!(
            KernelTarget::parse("phys:1000").unwrap(),
            KernelTarget::Physical { base: 0x1000 }
        );
        assert!(KernelMemoryProvider::can_handle("msr:10"));
        assert!(KernelTarget::parse("km:0:x").is_err());
        assert!(KernelTarget::parse("msr:10").is_err());
    }
}
