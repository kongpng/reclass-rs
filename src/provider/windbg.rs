//! WinDbg / DbgEng memory provider.
//!
//! Target strings mirror the C++ plugin:
//! - `"tcp:Port=5055,Server=localhost"`
//! - `"npipe:Pipe=name,Server=localhost"`
//! - `"pid:1234"`
//! - `"dump:C:/path/to/file.dmp"`

use super::{read_pages_in_runs, MemoryRegion, ModuleEntry, PageMap, Provider};

pub struct WinDbgMemoryProvider {
    inner: platform::Inner,
}

impl WinDbgMemoryProvider {
    pub fn attach(target: &str) -> Result<Self, String> {
        platform::Inner::attach(target).map(|inner| Self { inner })
    }

    pub fn can_handle(target: &str) -> bool {
        let lower = target.trim().to_ascii_lowercase();
        lower.starts_with("tcp:")
            || lower.starts_with("npipe:")
            || lower.starts_with("pid:")
            || lower.starts_with("dump:")
    }
}

impl Provider for WinDbgMemoryProvider {
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
        self.inner.is_live()
    }

    fn prefers_coalesced_rescan_reads(&self) -> bool {
        self.inner.is_live()
    }

    fn kind(&self) -> String {
        "WinDbg".to_string()
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

    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        self.inner.enumerate_regions()
    }

    fn enumerate_regions_with_modules(&self, modules: &[ModuleEntry]) -> Vec<MemoryRegion> {
        self.inner.enumerate_regions_with_modules(modules)
    }

    fn enumerate_modules(&self) -> Vec<ModuleEntry> {
        self.inner.enumerate_modules()
    }

    fn enumerate_modules_and_regions(&self) -> (Vec<ModuleEntry>, Vec<MemoryRegion>) {
        self.inner.enumerate_modules_and_regions()
    }

    fn is_readable(&self, _addr: u64, len: i32) -> bool {
        self.inner.size() > 0 && len >= 0
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::Mutex;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use crate::provider::{MemoryRegion, ModuleEntry, ModuleRangeLookup, RegionType};
    use windows::core::{Interface, PCSTR};
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    use windows::Win32::System::Diagnostics::Debug::Extensions::{
        DebugConnect, DebugCreate, IDebugClient, IDebugControl, IDebugDataSpaces,
        IDebugDataSpaces2, IDebugSymbols, DEBUG_ATTACH_NONINVASIVE,
        DEBUG_ATTACH_NONINVASIVE_NO_SUSPEND, DEBUG_DUMP_SMALL, DEBUG_END_DISCONNECT,
        DEBUG_MODULE_PARAMETERS,
    };
    use windows::Win32::System::LibraryLoader::{LoadLibraryA, SetDllDirectoryA};
    use windows::Win32::System::Memory::{
        MEMORY_BASIC_INFORMATION64, MEM_COMMIT, MEM_IMAGE, MEM_MAPPED, PAGE_EXECUTE,
        PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD,
        PAGE_NOACCESS, PAGE_READWRITE, PAGE_WRITECOPY,
    };

    #[derive(Clone, Debug)]
    struct SessionInfo {
        valid: bool,
        name: String,
        base: u64,
        is_live: bool,
        writable: bool,
        pointer_size: i32,
    }

    pub struct Inner {
        tx: Sender<Request>,
        join: Mutex<Option<JoinHandle<()>>>,
        info: SessionInfo,
    }

    unsafe impl Send for Inner {}
    unsafe impl Sync for Inner {}

    impl Inner {
        pub fn attach(target: &str) -> Result<Self, String> {
            let target = target.trim().to_string();
            if target.is_empty() {
                return Err("WinDbg target is empty".to_string());
            }
            let (tx, rx) = mpsc::channel::<Request>();
            let (init_tx, init_rx) = mpsc::channel::<Result<SessionInfo, String>>();
            let worker_target = target.clone();
            let join = thread::Builder::new()
                .name("DbgEngThread".to_string())
                .spawn(move || worker(worker_target, rx, init_tx))
                .map_err(|err| format!("failed to start DbgEng thread: {err}"))?;

            let info = match init_rx.recv_timeout(Duration::from_secs(20)) {
                Ok(Ok(info)) => info,
                Ok(Err(err)) => {
                    let _ = join.join();
                    return Err(err);
                }
                Err(err) => {
                    let _ = tx.send(Request::Shutdown);
                    let _ = join.join();
                    return Err(format!("DbgEng initialization timed out or failed: {err}"));
                }
            };

            Ok(Self {
                tx,
                join: Mutex::new(Some(join)),
                info,
            })
        }

        pub fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            if buf.is_empty() || buf.len() > u32::MAX as usize {
                return false;
            }
            let (reply_tx, reply_rx) = mpsc::channel();
            if self
                .tx
                .send(Request::Read {
                    addr,
                    len: buf.len(),
                    reply: reply_tx,
                })
                .is_err()
            {
                buf.fill(0);
                return false;
            }
            match reply_rx.recv_timeout(Duration::from_secs(30)) {
                Ok((ok, data)) => {
                    let n = buf.len().min(data.len());
                    buf[..n].copy_from_slice(&data[..n]);
                    if n < buf.len() {
                        buf[n..].fill(0);
                    }
                    ok
                }
                Err(_) => {
                    buf.fill(0);
                    false
                }
            }
        }

        pub fn write(&self, addr: u64, data: &[u8]) -> bool {
            if data.is_empty() || data.len() > u32::MAX as usize {
                return false;
            }
            let (reply_tx, reply_rx) = mpsc::channel();
            if self
                .tx
                .send(Request::Write {
                    addr,
                    data: data.to_vec(),
                    reply: reply_tx,
                })
                .is_err()
            {
                return false;
            }
            reply_rx
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or(false)
        }

        pub fn size(&self) -> i32 {
            if self.info.valid {
                0x10000
            } else {
                0
            }
        }

        pub fn is_writable(&self) -> bool {
            self.info.writable
        }

        pub fn name(&self) -> String {
            self.info.name.clone()
        }

        pub fn is_live(&self) -> bool {
            self.info.is_live
        }

        pub fn pointer_size(&self) -> i32 {
            self.info.pointer_size
        }

        pub fn base(&self) -> u64 {
            self.info.base
        }

        pub fn get_symbol(&self, addr: u64) -> String {
            let (reply_tx, reply_rx) = mpsc::channel();
            if self
                .tx
                .send(Request::Symbol {
                    addr,
                    reply: reply_tx,
                })
                .is_err()
            {
                return String::new();
            }
            reply_rx
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_default()
        }

        pub fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            let (reply_tx, reply_rx) = mpsc::channel();
            if self.tx.send(Request::Regions { reply: reply_tx }).is_err() {
                return Vec::new();
            }
            reply_rx
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_default()
        }

        pub fn enumerate_regions_with_modules(&self, modules: &[ModuleEntry]) -> Vec<MemoryRegion> {
            let (reply_tx, reply_rx) = mpsc::channel();
            if self
                .tx
                .send(Request::RegionsWithModules {
                    modules: modules.to_vec(),
                    reply: reply_tx,
                })
                .is_err()
            {
                return Vec::new();
            }
            reply_rx
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_default()
        }

        pub fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            let (reply_tx, reply_rx) = mpsc::channel();
            if self.tx.send(Request::Modules { reply: reply_tx }).is_err() {
                return Vec::new();
            }
            reply_rx
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_default()
        }

        pub fn enumerate_modules_and_regions(&self) -> (Vec<ModuleEntry>, Vec<MemoryRegion>) {
            let (reply_tx, reply_rx) = mpsc::channel();
            if self
                .tx
                .send(Request::ModulesAndRegions { reply: reply_tx })
                .is_err()
            {
                return Default::default();
            }
            reply_rx
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_default()
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            let _ = self.tx.send(Request::Shutdown);
            if let Ok(mut join) = self.join.lock() {
                if let Some(handle) = join.take() {
                    let _ = handle.join();
                }
            }
        }
    }

    enum Request {
        Read {
            addr: u64,
            len: usize,
            reply: Sender<(bool, Vec<u8>)>,
        },
        Write {
            addr: u64,
            data: Vec<u8>,
            reply: Sender<bool>,
        },
        Symbol {
            addr: u64,
            reply: Sender<String>,
        },
        Regions {
            reply: Sender<Vec<MemoryRegion>>,
        },
        RegionsWithModules {
            modules: Vec<ModuleEntry>,
            reply: Sender<Vec<MemoryRegion>>,
        },
        Modules {
            reply: Sender<Vec<ModuleEntry>>,
        },
        ModulesAndRegions {
            reply: Sender<(Vec<ModuleEntry>, Vec<MemoryRegion>)>,
        },
        Shutdown,
    }

    fn worker(target: String, rx: Receiver<Request>, init_tx: Sender<Result<SessionInfo, String>>) {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_err() {
                let _ = init_tx.send(Err(format!("CoInitializeEx failed: 0x{:08x}", hr.0 as u32)));
                return;
            }
        }

        let mut session = match unsafe { DbgSession::open(&target) } {
            Ok(session) => {
                let _ = init_tx.send(Ok(session.info.clone()));
                session
            }
            Err(err) => {
                let _ = init_tx.send(Err(err));
                unsafe { CoUninitialize() };
                return;
            }
        };

        while let Ok(req) = rx.recv() {
            match req {
                Request::Read { addr, len, reply } => {
                    let _ = reply.send(session.read(addr, len));
                }
                Request::Write { addr, data, reply } => {
                    let _ = reply.send(session.write(addr, &data));
                }
                Request::Symbol { addr, reply } => {
                    let _ = reply.send(session.get_symbol(addr));
                }
                Request::Regions { reply } => {
                    let _ = reply.send(session.enumerate_regions());
                }
                Request::RegionsWithModules { modules, reply } => {
                    let _ = reply.send(session.enumerate_regions_with_modules(&modules));
                }
                Request::Modules { reply } => {
                    let _ = reply.send(session.enumerate_modules());
                }
                Request::ModulesAndRegions { reply } => {
                    let _ = reply.send(session.enumerate_modules_and_regions());
                }
                Request::Shutdown => break,
            }
        }

        drop(session);
        unsafe { CoUninitialize() };
    }

    struct DbgSession {
        client: IDebugClient,
        data_spaces: IDebugDataSpaces,
        data_spaces2: Option<IDebugDataSpaces2>,
        symbols: Option<IDebugSymbols>,
        is_remote: bool,
        info: SessionInfo,
    }

    impl DbgSession {
        unsafe fn open(target: &str) -> Result<Self, String> {
            preload_dbgeng_tools();
            let lower = target.to_ascii_lowercase();
            let mut is_remote = false;
            let client = if lower.starts_with("tcp:") || lower.starts_with("npipe:") {
                is_remote = true;
                let bytes = c_string(target);
                let mut raw: *mut c_void = null_mut();
                DebugConnect(PCSTR(bytes.as_ptr()), &IDebugClient::IID, &mut raw)
                    .map_err(|err| format!("DebugConnect failed for {target}: {err}"))?;
                IDebugClient::from_raw(raw)
            } else {
                let client = DebugCreate::<IDebugClient>()
                    .map_err(|err| format!("DebugCreate failed: {err}"))?;
                if let Some(pid_text) = lower.strip_prefix("pid:") {
                    let pid = pid_text
                        .trim()
                        .parse::<u32>()
                        .map_err(|_| format!("invalid WinDbg PID target: {target}"))?;
                    client
                        .AttachProcess(
                            0,
                            pid,
                            DEBUG_ATTACH_NONINVASIVE | DEBUG_ATTACH_NONINVASIVE_NO_SUSPEND,
                        )
                        .map_err(|err| format!("AttachProcess failed for PID {pid}: {err}"))?;
                } else if lower.starts_with("dump:") {
                    let path = target[5..].trim();
                    let bytes = c_string(path);
                    client
                        .OpenDumpFile(PCSTR(bytes.as_ptr()))
                        .map_err(|err| format!("OpenDumpFile failed for {path}: {err}"))?;
                } else {
                    return Err(format!("unsupported WinDbg target: {target}"));
                }
                client
            };

            let data_spaces = client
                .cast::<IDebugDataSpaces>()
                .map_err(|err| format!("IDebugDataSpaces unavailable: {err}"))?;
            let data_spaces2 = client.cast::<IDebugDataSpaces2>().ok();
            let control = client.cast::<IDebugControl>().ok();
            let symbols = client.cast::<IDebugSymbols>().ok();

            if let Some(control) = &control {
                if !is_remote {
                    let _ = control.WaitForEvent(0, 10_000);
                }
            }

            let mut info = SessionInfo {
                valid: true,
                name: "WinDbg (Dump)".to_string(),
                base: 0,
                is_live: false,
                writable: false,
                pointer_size: 8,
            };
            if let Some(control) = &control {
                let mut debug_class = 0u32;
                let mut debug_qualifier = 0u32;
                if control
                    .GetDebuggeeType(&mut debug_class, &mut debug_qualifier)
                    .is_ok()
                {
                    info.is_live = debug_qualifier < DEBUG_DUMP_SMALL;
                    info.writable = info.is_live;
                    info.name = if info.is_live {
                        "WinDbg (Live)".to_string()
                    } else {
                        "WinDbg (Dump)".to_string()
                    };
                }
                if control.GetEffectiveProcessorType().unwrap_or(0) == 0x014c {
                    info.pointer_size = 4;
                }
            }

            Ok(Self {
                client,
                data_spaces,
                data_spaces2,
                symbols,
                is_remote,
                info,
            })
        }

        fn read(&mut self, addr: u64, len: usize) -> (bool, Vec<u8>) {
            let mut buf = vec![0u8; len];
            let mut bytes_read = 0u32;
            let ok = unsafe {
                self.data_spaces
                    .ReadVirtual(
                        addr,
                        buf.as_mut_ptr() as *mut c_void,
                        len as u32,
                        Some(&mut bytes_read),
                    )
                    .is_ok()
            };
            let got = bytes_read as usize;
            if got < buf.len() {
                buf[got..].fill(0);
            }
            (ok && got >= len || got > 0, buf)
        }

        fn write(&mut self, addr: u64, data: &[u8]) -> bool {
            if !self.info.writable || data.is_empty() {
                return false;
            }
            let mut written = 0u32;
            unsafe {
                self.data_spaces
                    .WriteVirtual(
                        addr,
                        data.as_ptr() as *const c_void,
                        data.len() as u32,
                        Some(&mut written),
                    )
                    .is_ok()
                    && written as usize == data.len()
            }
        }

        fn get_symbol(&mut self, addr: u64) -> String {
            let Some(symbols) = &self.symbols else {
                return String::new();
            };
            let mut name = [0u8; 512];
            let mut name_size = 0u32;
            let mut displacement = 0u64;
            if unsafe {
                symbols
                    .GetNameByOffset(
                        addr,
                        Some(&mut name),
                        Some(&mut name_size),
                        Some(&mut displacement),
                    )
                    .is_err()
            } {
                return String::new();
            }
            let mut out = nul_terminated(&name).to_string();
            if !out.is_empty() && displacement > 0 {
                out.push_str(&format!("+0x{displacement:x}"));
            }
            out
        }

        fn enumerate_modules(&mut self) -> Vec<ModuleEntry> {
            let Some(symbols) = &self.symbols else {
                return Vec::new();
            };
            let mut loaded = 0u32;
            let mut unloaded = 0u32;
            if unsafe {
                symbols
                    .GetNumberModules(&mut loaded, &mut unloaded)
                    .is_err()
            } {
                return Vec::new();
            }
            let mut modules = Vec::with_capacity(loaded as usize);
            for i in 0..loaded {
                let Ok(base) = (unsafe { symbols.GetModuleByIndex(i) }) else {
                    continue;
                };
                let mut params = DEBUG_MODULE_PARAMETERS::default();
                if unsafe {
                    symbols
                        .GetModuleParameters(1, Some(&base), 0, &mut params)
                        .is_err()
                } {
                    continue;
                }
                let mut image = [0u8; 512];
                let mut module = [0u8; 256];
                let mut image_size = 0u32;
                let mut module_size = 0u32;
                let _ = unsafe {
                    symbols.GetModuleNames(
                        i,
                        0,
                        Some(&mut image),
                        Some(&mut image_size),
                        Some(&mut module),
                        Some(&mut module_size),
                        None,
                        None,
                    )
                };
                modules.push(ModuleEntry {
                    name: nul_terminated(&module).to_string(),
                    full_path: nul_terminated(&image).to_string(),
                    base,
                    size: params.Size as u64,
                });
            }
            modules
        }

        fn enumerate_modules_and_regions(&mut self) -> (Vec<ModuleEntry>, Vec<MemoryRegion>) {
            let modules = self.enumerate_modules();
            let regions = self.enumerate_regions_with_modules(&modules);
            (modules, regions)
        }

        fn enumerate_regions(&mut self) -> Vec<MemoryRegion> {
            let modules = self.enumerate_modules();
            self.enumerate_regions_with_modules(&modules)
        }

        fn enumerate_regions_with_modules(&mut self, modules: &[ModuleEntry]) -> Vec<MemoryRegion> {
            let module_lookup = ModuleRangeLookup::new(modules);
            let mut regions = Vec::new();
            if let Some(data_spaces2) = &self.data_spaces2 {
                let mut addr = 0u64;
                for _ in 0..500_000 {
                    let mut mbi = MEMORY_BASIC_INFORMATION64::default();
                    if unsafe { data_spaces2.QueryVirtual(addr, &mut mbi).is_err() } {
                        break;
                    }
                    if mbi.State == MEM_COMMIT
                        && (mbi.Protect.0 & PAGE_NOACCESS.0) == 0
                        && (mbi.Protect.0 & PAGE_GUARD.0) == 0
                    {
                        let module_name = module_lookup
                            .find_by_addr(mbi.BaseAddress)
                            .map(|m| m.name.clone())
                            .unwrap_or_default();
                        regions.push(MemoryRegion {
                            base: mbi.BaseAddress,
                            size: mbi.RegionSize,
                            readable: true,
                            writable: (mbi.Protect.0 & PAGE_READWRITE.0) != 0
                                || (mbi.Protect.0 & PAGE_WRITECOPY.0) != 0
                                || (mbi.Protect.0 & PAGE_EXECUTE_READWRITE.0) != 0
                                || (mbi.Protect.0 & PAGE_EXECUTE_WRITECOPY.0) != 0,
                            executable: (mbi.Protect.0 & PAGE_EXECUTE.0) != 0
                                || (mbi.Protect.0 & PAGE_EXECUTE_READ.0) != 0
                                || (mbi.Protect.0 & PAGE_EXECUTE_READWRITE.0) != 0
                                || (mbi.Protect.0 & PAGE_EXECUTE_WRITECOPY.0) != 0,
                            module_name,
                            region_type: if mbi.Type == MEM_IMAGE {
                                RegionType::Image
                            } else if mbi.Type == MEM_MAPPED {
                                RegionType::Mapped
                            } else {
                                RegionType::Private
                            },
                        });
                    }
                    let next = mbi.BaseAddress.saturating_add(mbi.RegionSize);
                    if next <= addr {
                        break;
                    }
                    addr = next;
                }
            }

            if regions.is_empty() {
                regions.extend(modules.iter().filter(|m| m.size > 0).map(|m| MemoryRegion {
                    base: m.base,
                    size: m.size,
                    readable: true,
                    writable: false,
                    executable: true,
                    module_name: m.name.clone(),
                    region_type: RegionType::Image,
                }));
            }

            regions
        }
    }

    impl Drop for DbgSession {
        fn drop(&mut self) {
            unsafe {
                if self.is_remote {
                    let _ = self.client.EndSession(DEBUG_END_DISCONNECT);
                } else {
                    let _ = self.client.DetachProcesses();
                }
            }
        }
    }

    unsafe fn preload_dbgeng_tools() {
        let mut dirs = Vec::new();
        if let Ok(dir) = std::env::var("RECLASS_DBGTOOLS_DIR") {
            dirs.push(dir);
        }
        dirs.push("C:\\Program Files (x86)\\Windows Kits\\10\\Debuggers\\x64".to_string());
        dirs.push("C:\\Program Files\\Windows Kits\\10\\Debuggers\\x64".to_string());

        for dir in dirs {
            let dir_z = c_string(&dir);
            if SetDllDirectoryA(PCSTR(dir_z.as_ptr())).is_err() {
                continue;
            }
            for dep in ["dbghelp.dll", "dbgcore.dll", "symsrv.dll", "dbgeng.dll"] {
                let path = format!("{dir}\\{dep}");
                let path_z = c_string(&path);
                let _ = LoadLibraryA(PCSTR(path_z.as_ptr()));
            }
            return;
        }
    }

    fn c_string(s: &str) -> Vec<u8> {
        let mut out = s.as_bytes().to_vec();
        out.retain(|b| *b != 0);
        out.push(0);
        out
    }

    fn nul_terminated(buf: &[u8]) -> &str {
        let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        std::str::from_utf8(&buf[..len]).unwrap_or("")
    }
}

#[cfg(not(windows))]
mod platform {
    use crate::provider::{MemoryRegion, ModuleEntry};

    pub struct Inner;

    impl Inner {
        pub fn attach(_target: &str) -> Result<Self, String> {
            Err(
                "WinDbg Memory is implemented only on Windows because it requires DbgEng"
                    .to_string(),
            )
        }
        pub fn read(&self, _addr: u64, _buf: &mut [u8]) -> bool {
            false
        }
        pub fn size(&self) -> i32 {
            0
        }
        pub fn write(&self, _addr: u64, _data: &[u8]) -> bool {
            false
        }
        pub fn is_writable(&self) -> bool {
            false
        }
        pub fn name(&self) -> String {
            String::new()
        }
        pub fn is_live(&self) -> bool {
            false
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
        pub fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            Vec::new()
        }
        pub fn enumerate_regions_with_modules(
            &self,
            _modules: &[ModuleEntry],
        ) -> Vec<MemoryRegion> {
            Vec::new()
        }
        pub fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            Vec::new()
        }
        pub fn enumerate_modules_and_regions(&self) -> (Vec<ModuleEntry>, Vec<MemoryRegion>) {
            Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_cpp_target_shapes() {
        assert!(WinDbgMemoryProvider::can_handle(
            "tcp:Port=5055,Server=localhost"
        ));
        assert!(WinDbgMemoryProvider::can_handle(
            "npipe:Pipe=reclass,Server=localhost"
        ));
        assert!(WinDbgMemoryProvider::can_handle("pid:1234"));
        assert!(WinDbgMemoryProvider::can_handle("dump:C:/x.dmp"));
        assert!(!WinDbgMemoryProvider::can_handle("km:1234:x"));
    }
}
