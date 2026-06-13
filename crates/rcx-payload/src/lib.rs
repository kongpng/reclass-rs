#![cfg_attr(not(windows), allow(non_snake_case))]

#[cfg(windows)]
mod windows_payload {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::{addr_of_mut, copy_nonoverlapping, null, null_mut};
    use std::sync::atomic::{AtomicBool, Ordering};

    use rcx_rpc::{
        req_name, rsp_name, shm_name, RcxRpcHeader, RcxRpcModuleEntry, RcxRpcReadEntry,
        RcxRpcRegionEntry, RCX_RPC_DATA_OFFSET, RCX_RPC_DATA_SIZE, RCX_RPC_HEADER_SIZE,
        RCX_RPC_REGION_EXECUTABLE, RCX_RPC_REGION_IMAGE, RCX_RPC_REGION_MAPPED,
        RCX_RPC_REGION_PRIVATE, RCX_RPC_REGION_READABLE, RCX_RPC_REGION_WRITABLE, RCX_RPC_SHM_SIZE,
        RCX_RPC_STATUS_ERROR, RCX_RPC_STATUS_OK, RCX_RPC_STATUS_PARTIAL, RCX_RPC_VERSION,
        RPC_CMD_ENUM_MODULES, RPC_CMD_ENUM_REGIONS, RPC_CMD_PING, RPC_CMD_READ_BATCH,
        RPC_CMD_SHUTDOWN, RPC_CMD_WRITE,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, HINSTANCE, HMODULE, INVALID_HANDLE_VALUE, TRUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingA, MapViewOfFile, UnmapViewOfFile, VirtualQuery, FILE_MAP_ALL_ACCESS,
        MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS, MEM_COMMIT, MEM_IMAGE, MEM_MAPPED,
        PAGE_EXECUTE, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY,
        PAGE_GUARD, PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        EnumProcessModules, GetModuleBaseNameW, GetModuleInformation, MODULEINFO,
    };
    use windows_sys::Win32::System::SystemServices::DLL_PROCESS_DETACH;
    use windows_sys::Win32::System::Threading::{
        CreateEventA, CreateTimerQueue, CreateTimerQueueTimer, DeleteTimerQueueEx,
        GetCurrentProcess, GetCurrentProcessId, SetEvent, WaitForSingleObject, WT_EXECUTEDEFAULT,
    };

    static INITIALIZED: AtomicBool = AtomicBool::new(false);
    static mut H_SHM: HANDLE = null_mut();
    static mut MAPPED_VIEW: MEMORY_MAPPED_VIEW_ADDRESS =
        MEMORY_MAPPED_VIEW_ADDRESS { Value: null_mut() };
    static mut H_REQ_EVENT: HANDLE = null_mut();
    static mut H_RSP_EVENT: HANDLE = null_mut();
    static mut H_TIMER_QUEUE: HANDLE = null_mut();
    static mut H_POLL_TIMER: HANDLE = null_mut();

    fn c_string_bytes(mut s: String) -> Vec<u8> {
        s.push('\0');
        s.into_bytes()
    }

    fn is_readable_protect(p: u32) -> bool {
        if (p & (PAGE_NOACCESS | PAGE_GUARD)) != 0 {
            return false;
        }
        let readable = PAGE_READONLY
            | PAGE_READWRITE
            | PAGE_WRITECOPY
            | PAGE_EXECUTE_READ
            | PAGE_EXECUTE_READWRITE
            | PAGE_EXECUTE_WRITECOPY;
        (p & readable) != 0
    }

    fn is_writable_protect(p: u32) -> bool {
        if (p & (PAGE_NOACCESS | PAGE_GUARD)) != 0 {
            return false;
        }
        let writable =
            PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY;
        (p & writable) != 0
    }

    unsafe fn is_range_readable(addr: usize, len: u32) -> bool {
        let Some(end) = addr.checked_add(len as usize) else {
            return false;
        };
        let mut cur = addr;
        while cur < end {
            let mut mbi: MEMORY_BASIC_INFORMATION = zeroed();
            if VirtualQuery(
                cur as *const c_void,
                &mut mbi,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            ) == 0
            {
                return false;
            }
            if mbi.State != MEM_COMMIT || !is_readable_protect(mbi.Protect) {
                return false;
            }
            let region_end = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
            if region_end <= cur {
                return false;
            }
            cur = region_end;
        }
        true
    }

    unsafe fn is_range_writable(addr: usize, len: u32) -> bool {
        let Some(end) = addr.checked_add(len as usize) else {
            return false;
        };
        let mut cur = addr;
        while cur < end {
            let mut mbi: MEMORY_BASIC_INFORMATION = zeroed();
            if VirtualQuery(
                cur as *const c_void,
                &mut mbi,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            ) == 0
            {
                return false;
            }
            if mbi.State != MEM_COMMIT || !is_writable_protect(mbi.Protect) {
                return false;
            }
            let region_end = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
            if region_end <= cur {
                return false;
            }
            cur = region_end;
        }
        true
    }

    unsafe fn handle_read_batch(hdr: &mut RcxRpcHeader, data: *mut u8) {
        let count = hdr.request_count.min(rcx_rpc::RCX_RPC_MAX_BATCH as u32);
        let entries = data as *const RcxRpcReadEntry;
        for i in 0..count {
            let entry = *entries.add(i as usize);
            let Some(end) = (entry.data_offset as usize).checked_add(entry.length as usize) else {
                hdr.status = RCX_RPC_STATUS_PARTIAL;
                continue;
            };
            if end > RCX_RPC_DATA_SIZE {
                hdr.status = RCX_RPC_STATUS_PARTIAL;
                continue;
            }
            let dest = data.add(entry.data_offset as usize);
            if is_range_readable(entry.address as usize, entry.length) {
                copy_nonoverlapping(entry.address as *const u8, dest, entry.length as usize);
            } else {
                std::ptr::write_bytes(dest, 0, entry.length as usize);
                hdr.status = RCX_RPC_STATUS_PARTIAL;
            }
        }
        hdr.response_count = count;
    }

    unsafe fn handle_write(hdr: &mut RcxRpcHeader, data: *const u8) {
        if hdr.write_length as usize > RCX_RPC_DATA_SIZE {
            hdr.status = RCX_RPC_STATUS_ERROR;
            return;
        }
        if is_range_writable(hdr.write_address as usize, hdr.write_length) {
            copy_nonoverlapping(
                data,
                hdr.write_address as *mut u8,
                hdr.write_length as usize,
            );
        } else {
            hdr.status = RCX_RPC_STATUS_ERROR;
        }
    }

    unsafe fn handle_enum_modules(hdr: &mut RcxRpcHeader, data: *mut u8) {
        let h_proc = GetCurrentProcess();
        let mut mods: [HMODULE; 1024] = [null_mut(); 1024];
        let mut needed = 0u32;
        if EnumProcessModules(
            h_proc,
            mods.as_mut_ptr(),
            (mods.len() * size_of::<HMODULE>()) as u32,
            &mut needed,
        ) == 0
        {
            hdr.status = RCX_RPC_STATUS_ERROR;
            hdr.response_count = 0;
            return;
        }

        let count = ((needed as usize) / size_of::<HMODULE>()).min(mods.len());
        let entry_bytes = count * size_of::<RcxRpcModuleEntry>();
        if entry_bytes > RCX_RPC_DATA_SIZE {
            hdr.status = RCX_RPC_STATUS_ERROR;
            hdr.response_count = 0;
            return;
        }

        let mut name_data_off = entry_bytes as u32;
        for (i, module) in mods.iter().copied().take(count).enumerate() {
            let mut mi: MODULEINFO = zeroed();
            let mut mod_name = [0u16; 260];
            let _ = GetModuleInformation(h_proc, module, &mut mi, size_of::<MODULEINFO>() as u32);
            let name_len =
                GetModuleBaseNameW(h_proc, module, mod_name.as_mut_ptr(), mod_name.len() as u32)
                    as usize;
            let name_bytes = (name_len * size_of::<u16>()) as u32;
            let entry = data.add(i * size_of::<RcxRpcModuleEntry>()) as *mut RcxRpcModuleEntry;
            (*entry).base = mi.lpBaseOfDll as u64;
            (*entry).size = mi.SizeOfImage as u64;
            (*entry).name_offset = name_data_off;
            (*entry).name_length = name_bytes;
            if (name_data_off as usize).saturating_add(name_bytes as usize) <= RCX_RPC_DATA_SIZE {
                copy_nonoverlapping(
                    mod_name.as_ptr() as *const u8,
                    data.add(name_data_off as usize),
                    name_bytes as usize,
                );
                name_data_off += name_bytes;
            }
        }

        hdr.response_count = count as u32;
        hdr.total_data_used = name_data_off;
        hdr.status = RCX_RPC_STATUS_OK;
    }

    struct EncodedRegion {
        base: u64,
        size: u64,
        name: Vec<u8>,
        flags: u32,
        region_type: u32,
    }

    unsafe fn handle_enum_regions(hdr: &mut RcxRpcHeader, data: *mut u8) {
        let h_proc = GetCurrentProcess();
        let mut regions = Vec::new();
        let mut addr = 0usize;

        loop {
            let mut mbi: MEMORY_BASIC_INFORMATION = zeroed();
            if VirtualQuery(
                addr as *const c_void,
                &mut mbi,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            ) == 0
            {
                break;
            }

            if mbi.State == MEM_COMMIT {
                let readable = is_readable_protect(mbi.Protect);
                if readable {
                    let writable = is_writable_protect(mbi.Protect);
                    let executable = (mbi.Protect
                        & (PAGE_EXECUTE
                            | PAGE_EXECUTE_READ
                            | PAGE_EXECUTE_READWRITE
                            | PAGE_EXECUTE_WRITECOPY))
                        != 0;
                    let region_type = if mbi.Type == MEM_IMAGE {
                        RCX_RPC_REGION_IMAGE
                    } else if mbi.Type == MEM_MAPPED {
                        RCX_RPC_REGION_MAPPED
                    } else {
                        RCX_RPC_REGION_PRIVATE
                    };
                    let mut flags = RCX_RPC_REGION_READABLE;
                    if writable {
                        flags |= RCX_RPC_REGION_WRITABLE;
                    }
                    if executable {
                        flags |= RCX_RPC_REGION_EXECUTABLE;
                    }
                    let mut module_name = [0u16; 260];
                    let name_len = if mbi.Type == MEM_IMAGE {
                        GetModuleBaseNameW(
                            h_proc,
                            mbi.AllocationBase as HMODULE,
                            module_name.as_mut_ptr(),
                            module_name.len() as u32,
                        ) as usize
                    } else {
                        0
                    };
                    let name = std::slice::from_raw_parts(
                        module_name.as_ptr() as *const u8,
                        name_len * size_of::<u16>(),
                    )
                    .to_vec();
                    regions.push(EncodedRegion {
                        base: mbi.BaseAddress as u64,
                        size: mbi.RegionSize as u64,
                        name,
                        flags,
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

        write_region_entries(hdr, data, regions);
    }

    unsafe fn write_region_entries(
        hdr: &mut RcxRpcHeader,
        data: *mut u8,
        regions: Vec<EncodedRegion>,
    ) {
        let max_entries = RCX_RPC_DATA_SIZE / size_of::<RcxRpcRegionEntry>();
        let count = regions.len().min(max_entries);
        let entry_bytes = count * size_of::<RcxRpcRegionEntry>();
        let mut name_data_off = entry_bytes as u32;
        let mut written = 0usize;

        for (i, region) in regions.into_iter().take(count).enumerate() {
            let name_len = region
                .name
                .len()
                .min(RCX_RPC_DATA_SIZE.saturating_sub(name_data_off as usize));
            let entry = data.add(i * size_of::<RcxRpcRegionEntry>()) as *mut RcxRpcRegionEntry;
            (*entry).base = region.base;
            (*entry).size = region.size;
            (*entry).name_offset = name_data_off;
            (*entry).name_length = name_len as u32;
            (*entry).flags = region.flags;
            (*entry).region_type = region.region_type;
            if name_len > 0 {
                copy_nonoverlapping(
                    region.name.as_ptr(),
                    data.add(name_data_off as usize),
                    name_len,
                );
                name_data_off += name_len as u32;
            }
            written += 1;
        }

        hdr.response_count = written as u32;
        hdr.total_data_used = name_data_off;
        hdr.status = RCX_RPC_STATUS_OK;
    }

    unsafe extern "system" fn poll_timer_callback(_: *mut c_void, _: bool) {
        if MAPPED_VIEW.Value.is_null() || H_REQ_EVENT.is_null() || H_RSP_EVENT.is_null() {
            return;
        }
        if WaitForSingleObject(H_REQ_EVENT, 0) != WAIT_OBJECT_0 {
            return;
        }

        let hdr = &mut *(MAPPED_VIEW.Value as *mut RcxRpcHeader);
        let data = (MAPPED_VIEW.Value as *mut u8).add(RCX_RPC_DATA_OFFSET);
        hdr.status = RCX_RPC_STATUS_OK;

        match hdr.command {
            RPC_CMD_READ_BATCH => handle_read_batch(hdr, data),
            RPC_CMD_WRITE => handle_write(hdr, data),
            RPC_CMD_ENUM_MODULES => handle_enum_modules(hdr, data),
            RPC_CMD_ENUM_REGIONS => handle_enum_regions(hdr, data),
            RPC_CMD_PING => {}
            RPC_CMD_SHUTDOWN => {
                payload_cleanup();
                return;
            }
            _ => hdr.status = RCX_RPC_STATUS_ERROR,
        }

        SetEvent(H_RSP_EVENT);
    }

    unsafe fn payload_cleanup() {
        if !INITIALIZED.load(Ordering::Acquire) {
            return;
        }

        if !H_TIMER_QUEUE.is_null() {
            DeleteTimerQueueEx(H_TIMER_QUEUE, INVALID_HANDLE_VALUE);
            H_TIMER_QUEUE = null_mut();
            H_POLL_TIMER = null_mut();
        }

        if !MAPPED_VIEW.Value.is_null() {
            (*(MAPPED_VIEW.Value as *mut RcxRpcHeader)).payload_ready = 0;
        }

        if !MAPPED_VIEW.Value.is_null() {
            UnmapViewOfFile(MAPPED_VIEW);
            MAPPED_VIEW = MEMORY_MAPPED_VIEW_ADDRESS { Value: null_mut() };
        }
        if !H_SHM.is_null() {
            CloseHandle(H_SHM);
            H_SHM = null_mut();
        }
        if !H_REQ_EVENT.is_null() {
            CloseHandle(H_REQ_EVENT);
            H_REQ_EVENT = null_mut();
        }
        if !H_RSP_EVENT.is_null() {
            CloseHandle(H_RSP_EVENT);
            H_RSP_EVENT = null_mut();
        }

        INITIALIZED.store(false, Ordering::Release);
    }

    unsafe fn init_impl() -> bool {
        if INITIALIZED.swap(true, Ordering::AcqRel) {
            return true;
        }

        let pid = GetCurrentProcessId();
        let shm_name = c_string_bytes(shm_name(pid));
        let req_name = c_string_bytes(req_name(pid));
        let rsp_name = c_string_bytes(rsp_name(pid));

        H_SHM = CreateFileMappingA(
            INVALID_HANDLE_VALUE,
            null(),
            PAGE_READWRITE,
            0,
            RCX_RPC_SHM_SIZE as u32,
            shm_name.as_ptr(),
        );
        if H_SHM.is_null() {
            INITIALIZED.store(false, Ordering::Release);
            return false;
        }

        MAPPED_VIEW = MapViewOfFile(H_SHM, FILE_MAP_ALL_ACCESS, 0, 0, RCX_RPC_SHM_SIZE);
        if MAPPED_VIEW.Value.is_null() {
            CloseHandle(H_SHM);
            H_SHM = null_mut();
            INITIALIZED.store(false, Ordering::Release);
            return false;
        }

        std::ptr::write_bytes(MAPPED_VIEW.Value, 0, RCX_RPC_HEADER_SIZE);
        let hdr = &mut *(MAPPED_VIEW.Value as *mut RcxRpcHeader);
        hdr.version = RCX_RPC_VERSION;
        hdr.image_base = GetModuleHandleW(null()) as u64;
        hdr.pointer_size = size_of::<usize>() as u32;

        H_REQ_EVENT = CreateEventA(null(), 0, 0, req_name.as_ptr());
        H_RSP_EVENT = CreateEventA(null(), 0, 0, rsp_name.as_ptr());
        if H_REQ_EVENT.is_null() || H_RSP_EVENT.is_null() {
            payload_cleanup();
            return false;
        }

        H_TIMER_QUEUE = CreateTimerQueue();
        if H_TIMER_QUEUE.is_null() {
            payload_cleanup();
            return false;
        }

        if CreateTimerQueueTimer(
            addr_of_mut!(H_POLL_TIMER),
            H_TIMER_QUEUE,
            Some(poll_timer_callback),
            null(),
            0,
            10,
            WT_EXECUTEDEFAULT,
        ) == 0
        {
            payload_cleanup();
            return false;
        }

        hdr.payload_ready = 1;
        true
    }

    #[no_mangle]
    pub unsafe extern "system" fn RcxPayloadInit() -> bool {
        init_impl()
    }

    #[no_mangle]
    pub unsafe extern "system" fn DllMain(_: HINSTANCE, reason: u32, _: *mut c_void) -> i32 {
        if reason == DLL_PROCESS_DETACH {
            payload_cleanup();
        }
        TRUE
    }
}

#[cfg(target_os = "linux")]
mod linux_payload {
    use std::ffi::CString;
    use std::mem::{size_of, zeroed};
    use std::path::Path;
    use std::ptr::{copy_nonoverlapping, null_mut};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    use rcx_rpc::{
        req_name, rsp_name, shm_name, RcxRpcHeader, RcxRpcModuleEntry, RcxRpcReadEntry,
        RcxRpcRegionEntry, RCX_RPC_DATA_OFFSET, RCX_RPC_DATA_SIZE, RCX_RPC_HEADER_SIZE,
        RCX_RPC_MAX_BATCH, RCX_RPC_REGION_EXECUTABLE, RCX_RPC_REGION_IMAGE, RCX_RPC_REGION_MAPPED,
        RCX_RPC_REGION_PRIVATE, RCX_RPC_REGION_READABLE, RCX_RPC_REGION_WRITABLE, RCX_RPC_SHM_SIZE,
        RCX_RPC_STATUS_ERROR, RCX_RPC_STATUS_OK, RCX_RPC_STATUS_PARTIAL, RCX_RPC_VERSION,
        RPC_CMD_ENUM_MODULES, RPC_CMD_ENUM_REGIONS, RPC_CMD_PING, RPC_CMD_READ_BATCH,
        RPC_CMD_SHUTDOWN, RPC_CMD_WRITE,
    };

    static INITIALIZED: AtomicBool = AtomicBool::new(false);
    static SHUTDOWN: AtomicBool = AtomicBool::new(false);
    static THREAD_RUNNING: AtomicBool = AtomicBool::new(false);
    static THREAD_CREATED: AtomicBool = AtomicBool::new(false);

    const NAME_CAP: usize = 128;

    static mut SHM_FD: libc::c_int = -1;
    static mut MEM_FD: libc::c_int = -1;
    static mut MAPPED_VIEW: *mut libc::c_void = null_mut();
    static mut REQ_SEM: *mut libc::sem_t = null_mut();
    static mut RSP_SEM: *mut libc::sem_t = null_mut();
    static mut THREAD: libc::pthread_t = 0;
    static mut SHM_NAME: [libc::c_char; NAME_CAP] = [0; NAME_CAP];
    static mut REQ_NAME: [libc::c_char; NAME_CAP] = [0; NAME_CAP];
    static mut RSP_NAME: [libc::c_char; NAME_CAP] = [0; NAME_CAP];

    #[used]
    #[link_section = ".init_array"]
    static INIT_ARRAY: extern "C" fn() = payload_ctor;

    #[used]
    #[link_section = ".fini_array"]
    static FINI_ARRAY: extern "C" fn() = payload_dtor;

    extern "C" fn payload_ctor() {
        unsafe {
            let _ = init_impl();
        }
    }

    extern "C" fn payload_dtor() {
        unsafe {
            payload_cleanup();
        }
    }

    unsafe fn safe_read(addr: u64, dest: *mut u8, len: u32, status: &mut u32) {
        if len == 0 {
            return;
        }
        let n = libc::pread(MEM_FD, dest.cast(), len as usize, addr as libc::off_t);
        if n < len as isize {
            if n > 0 {
                std::ptr::write_bytes(dest.add(n as usize), 0, len as usize - n as usize);
            } else {
                std::ptr::write_bytes(dest, 0, len as usize);
            }
            *status = RCX_RPC_STATUS_PARTIAL;
        }
    }

    unsafe fn safe_write(addr: u64, src: *const u8, len: u32, status: &mut u32) {
        if len == 0 {
            return;
        }
        let n = libc::pwrite(MEM_FD, src.cast(), len as usize, addr as libc::off_t);
        if n < len as isize {
            *status = RCX_RPC_STATUS_ERROR;
        }
    }

    unsafe fn handle_read_batch(hdr: &mut RcxRpcHeader, data: *mut u8) {
        let count = hdr.request_count.min(RCX_RPC_MAX_BATCH as u32);
        let entries = data as *const RcxRpcReadEntry;
        for i in 0..count {
            let entry = *entries.add(i as usize);
            let Some(end) = (entry.data_offset as usize).checked_add(entry.length as usize) else {
                hdr.status = RCX_RPC_STATUS_PARTIAL;
                continue;
            };
            if end > RCX_RPC_DATA_SIZE {
                hdr.status = RCX_RPC_STATUS_PARTIAL;
                continue;
            }
            safe_read(
                entry.address,
                data.add(entry.data_offset as usize),
                entry.length,
                &mut hdr.status,
            );
        }
        hdr.response_count = count;
    }

    unsafe fn handle_write(hdr: &mut RcxRpcHeader, data: *const u8) {
        if hdr.write_length as usize > RCX_RPC_DATA_SIZE {
            hdr.status = RCX_RPC_STATUS_ERROR;
            return;
        }
        safe_write(hdr.write_address, data, hdr.write_length, &mut hdr.status);
    }

    unsafe fn handle_enum_modules(hdr: &mut RcxRpcHeader, data: *mut u8) {
        let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
            hdr.status = RCX_RPC_STATUS_ERROR;
            hdr.response_count = 0;
            return;
        };

        let mut ranges: Vec<(String, u64, u64)> = Vec::new();
        for line in maps.lines() {
            let Some((start, end, path)) = parse_file_map_line(line) else {
                continue;
            };
            if let Some((_, base, mapped_end)) = ranges.iter_mut().find(|(p, _, _)| p == &path) {
                *base = (*base).min(start);
                *mapped_end = (*mapped_end).max(end);
            } else {
                ranges.push((path, start, end));
            }
            if ranges.len() >= 512 {
                break;
            }
        }

        let max_entries = RCX_RPC_DATA_SIZE / size_of::<RcxRpcModuleEntry>();
        let count = ranges.len().min(max_entries);
        let entry_bytes = count * size_of::<RcxRpcModuleEntry>();
        let mut name_data_off = entry_bytes as u32;

        for (i, (path, base, end)) in ranges.into_iter().take(count).enumerate() {
            let basename = Path::new(&path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&path);
            let name = basename.as_bytes();
            let entry = data.add(i * size_of::<RcxRpcModuleEntry>()) as *mut RcxRpcModuleEntry;
            (*entry).base = base;
            (*entry).size = end.saturating_sub(base);
            (*entry).name_offset = name_data_off;
            (*entry).name_length = name.len() as u32;
            if (name_data_off as usize).saturating_add(name.len()) <= RCX_RPC_DATA_SIZE {
                copy_nonoverlapping(name.as_ptr(), data.add(name_data_off as usize), name.len());
                name_data_off += name.len() as u32;
            }
        }

        hdr.response_count = count as u32;
        hdr.total_data_used = name_data_off;
        hdr.status = RCX_RPC_STATUS_OK;
    }

    struct MapRegion {
        start: u64,
        end: u64,
        readable: bool,
        writable: bool,
        executable: bool,
        name: Vec<u8>,
        region_type: u32,
    }

    unsafe fn handle_enum_regions(hdr: &mut RcxRpcHeader, data: *mut u8) {
        let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
            hdr.status = RCX_RPC_STATUS_ERROR;
            hdr.response_count = 0;
            return;
        };

        let regions = maps
            .lines()
            .filter_map(parse_region_map_line)
            .collect::<Vec<_>>();
        write_region_entries(hdr, data, regions);
    }

    unsafe fn write_region_entries(hdr: &mut RcxRpcHeader, data: *mut u8, regions: Vec<MapRegion>) {
        let max_entries = RCX_RPC_DATA_SIZE / size_of::<RcxRpcRegionEntry>();
        let count = regions.len().min(max_entries);
        let entry_bytes = count * size_of::<RcxRpcRegionEntry>();
        let mut name_data_off = entry_bytes as u32;
        let mut written = 0usize;

        for (i, region) in regions.into_iter().take(count).enumerate() {
            if region.end <= region.start || !region.readable {
                continue;
            }
            let mut flags = RCX_RPC_REGION_READABLE;
            if region.writable {
                flags |= RCX_RPC_REGION_WRITABLE;
            }
            if region.executable {
                flags |= RCX_RPC_REGION_EXECUTABLE;
            }
            let name_len = region
                .name
                .len()
                .min(RCX_RPC_DATA_SIZE.saturating_sub(name_data_off as usize));
            let entry = data.add(i * size_of::<RcxRpcRegionEntry>()) as *mut RcxRpcRegionEntry;
            (*entry).base = region.start;
            (*entry).size = region.end - region.start;
            (*entry).name_offset = name_data_off;
            (*entry).name_length = name_len as u32;
            (*entry).flags = flags;
            (*entry).region_type = region.region_type;
            if name_len > 0 {
                copy_nonoverlapping(
                    region.name.as_ptr(),
                    data.add(name_data_off as usize),
                    name_len,
                );
                name_data_off += name_len as u32;
            }
            written += 1;
        }

        hdr.response_count = written as u32;
        hdr.total_data_used = name_data_off;
        hdr.status = RCX_RPC_STATUS_OK;
    }

    extern "C" fn server_thread_func(_: *mut libc::c_void) -> *mut libc::c_void {
        unsafe {
            THREAD_RUNNING.store(true, Ordering::Release);
            if MAPPED_VIEW.is_null() {
                THREAD_RUNNING.store(false, Ordering::Release);
                return null_mut();
            }
            let hdr = &mut *(MAPPED_VIEW as *mut RcxRpcHeader);
            let data = (MAPPED_VIEW as *mut u8).add(RCX_RPC_DATA_OFFSET);
            std::ptr::write_volatile(&mut hdr.payload_ready, 1);

            while !SHUTDOWN.load(Ordering::Acquire) {
                let mut ts = timespec_after(250);
                let rc = libc::sem_timedwait(REQ_SEM, &mut ts);
                if rc != 0 {
                    let errno = std::io::Error::last_os_error().raw_os_error();
                    if matches!(errno, Some(libc::ETIMEDOUT | libc::EINTR)) {
                        continue;
                    }
                    break;
                }

                hdr.status = RCX_RPC_STATUS_OK;
                match hdr.command {
                    RPC_CMD_READ_BATCH => handle_read_batch(hdr, data),
                    RPC_CMD_WRITE => handle_write(hdr, data),
                    RPC_CMD_ENUM_MODULES => handle_enum_modules(hdr, data),
                    RPC_CMD_ENUM_REGIONS => handle_enum_regions(hdr, data),
                    RPC_CMD_PING => {}
                    RPC_CMD_SHUTDOWN => SHUTDOWN.store(true, Ordering::Release),
                    _ => hdr.status = RCX_RPC_STATUS_ERROR,
                }
                libc::sem_post(RSP_SEM);
                if hdr.command == RPC_CMD_SHUTDOWN {
                    break;
                }
            }

            if !MAPPED_VIEW.is_null() {
                let hdr = &mut *(MAPPED_VIEW as *mut RcxRpcHeader);
                std::ptr::write_volatile(&mut hdr.payload_ready, 0);
            }
            THREAD_RUNNING.store(false, Ordering::Release);
            null_mut()
        }
    }

    unsafe fn init_impl() -> bool {
        if INITIALIZED.swap(true, Ordering::AcqRel) {
            return true;
        }
        SHUTDOWN.store(false, Ordering::Release);
        THREAD_RUNNING.store(false, Ordering::Release);
        THREAD_CREATED.store(false, Ordering::Release);

        MEM_FD = libc::open(c"/proc/self/mem".as_ptr(), libc::O_RDWR);
        if MEM_FD < 0 {
            INITIALIZED.store(false, Ordering::Release);
            return false;
        }

        let pid = libc::getpid() as u32;
        let shm = CString::new(shm_name(pid)).expect("rpc name contains no NUL");
        let req = CString::new(req_name(pid)).expect("rpc name contains no NUL");
        let rsp = CString::new(rsp_name(pid)).expect("rpc name contains no NUL");
        store_name(shm_name_ptr(), NAME_CAP, &shm);
        store_name(req_name_ptr(), NAME_CAP, &req);
        store_name(rsp_name_ptr(), NAME_CAP, &rsp);

        SHM_FD = libc::shm_open(
            shm_name_ptr().cast_const(),
            libc::O_CREAT | libc::O_RDWR,
            0o600,
        );
        if SHM_FD < 0 {
            payload_cleanup();
            return false;
        }
        if libc::ftruncate(SHM_FD, RCX_RPC_SHM_SIZE as libc::off_t) != 0 {
            payload_cleanup();
            return false;
        }

        MAPPED_VIEW = libc::mmap(
            null_mut(),
            RCX_RPC_SHM_SIZE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            SHM_FD,
            0,
        );
        if MAPPED_VIEW == libc::MAP_FAILED {
            MAPPED_VIEW = null_mut();
            payload_cleanup();
            return false;
        }

        std::ptr::write_bytes(MAPPED_VIEW, 0, RCX_RPC_HEADER_SIZE);
        let hdr = &mut *(MAPPED_VIEW as *mut RcxRpcHeader);
        hdr.version = RCX_RPC_VERSION;
        hdr.image_base = first_executable_mapping();
        hdr.pointer_size = size_of::<usize>() as u32;

        REQ_SEM = libc::sem_open(req_name_ptr().cast_const(), libc::O_CREAT, 0o600, 0);
        RSP_SEM = libc::sem_open(rsp_name_ptr().cast_const(), libc::O_CREAT, 0o600, 0);
        if sem_failed(REQ_SEM) || sem_failed(RSP_SEM) {
            payload_cleanup();
            return false;
        }

        let mut thread: libc::pthread_t = zeroed();
        if libc::pthread_create(&mut thread, null_mut(), server_thread_func, null_mut()) != 0 {
            payload_cleanup();
            return false;
        }
        THREAD = thread;
        THREAD_CREATED.store(true, Ordering::Release);
        true
    }

    unsafe fn payload_cleanup() {
        SHUTDOWN.store(true, Ordering::Release);
        if !sem_failed(REQ_SEM) {
            libc::sem_post(REQ_SEM);
        }

        if THREAD_CREATED.swap(false, Ordering::AcqRel) {
            let mut ts = timespec_after(2000);
            let rc = libc::pthread_timedjoin_np(THREAD, null_mut(), &mut ts);
            if rc != 0 {
                libc::pthread_detach(THREAD);
                for _ in 0..200 {
                    if !THREAD_RUNNING.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }

        if !MAPPED_VIEW.is_null() {
            (*(MAPPED_VIEW as *mut RcxRpcHeader)).payload_ready = 0;
            libc::munmap(MAPPED_VIEW, RCX_RPC_SHM_SIZE);
            MAPPED_VIEW = null_mut();
        }
        if SHM_FD >= 0 {
            libc::close(SHM_FD);
            SHM_FD = -1;
        }
        if !sem_failed(REQ_SEM) {
            libc::sem_close(REQ_SEM);
            REQ_SEM = null_mut();
        }
        if !sem_failed(RSP_SEM) {
            libc::sem_close(RSP_SEM);
            RSP_SEM = null_mut();
        }

        if name_nonempty(shm_name_ptr().cast_const()) {
            libc::shm_unlink(shm_name_ptr().cast_const());
            std::ptr::write_bytes(shm_name_ptr(), 0, NAME_CAP);
        }
        if name_nonempty(req_name_ptr().cast_const()) {
            libc::sem_unlink(req_name_ptr().cast_const());
            std::ptr::write_bytes(req_name_ptr(), 0, NAME_CAP);
        }
        if name_nonempty(rsp_name_ptr().cast_const()) {
            libc::sem_unlink(rsp_name_ptr().cast_const());
            std::ptr::write_bytes(rsp_name_ptr(), 0, NAME_CAP);
        }
        if MEM_FD >= 0 {
            libc::close(MEM_FD);
            MEM_FD = -1;
        }
        INITIALIZED.store(false, Ordering::Release);
    }

    fn parse_file_map_line(line: &str) -> Option<(u64, u64, String)> {
        let mut parts = line.split_whitespace();
        let range = parts.next()?;
        let _perms = parts.next()?;
        let _offset = parts.next()?;
        let _dev = parts.next()?;
        let _inode = parts.next()?;
        let path = parts.collect::<Vec<_>>().join(" ");
        let path = path.trim();
        if path.is_empty()
            || !path.starts_with('/')
            || path.starts_with("/dev/")
            || path.starts_with("/memfd:")
        {
            return None;
        }
        let (start, end) = range.split_once('-')?;
        Some((
            u64::from_str_radix(start, 16).ok()?,
            u64::from_str_radix(end, 16).ok()?,
            path.to_string(),
        ))
    }

    fn parse_region_map_line(line: &str) -> Option<MapRegion> {
        let mut parts = line.split_whitespace();
        let range = parts.next()?;
        let perms = parts.next()?;
        let _offset = parts.next()?;
        let _dev = parts.next()?;
        let _inode = parts.next()?;
        if perms.len() < 3 {
            return None;
        }
        let path = parts.collect::<Vec<_>>().join(" ");
        let path = path.trim();
        let readable = perms.as_bytes().first() == Some(&b'r');
        if !readable {
            return None;
        }
        let writable = perms.as_bytes().get(1) == Some(&b'w');
        let executable = perms.as_bytes().get(2) == Some(&b'x');
        let (start, end) = range.split_once('-')?;
        let mut name = Vec::new();
        let mut region_type = RCX_RPC_REGION_PRIVATE;
        if path.starts_with('/') && !path.starts_with("/dev/") && !path.starts_with("/memfd:") {
            let basename = Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(path);
            name.extend_from_slice(basename.as_bytes());
            region_type = if executable {
                RCX_RPC_REGION_IMAGE
            } else {
                RCX_RPC_REGION_MAPPED
            };
        }
        Some(MapRegion {
            start: u64::from_str_radix(start, 16).ok()?,
            end: u64::from_str_radix(end, 16).ok()?,
            readable,
            writable,
            executable,
            name,
            region_type,
        })
    }

    fn first_executable_mapping() -> u64 {
        let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
            return 0;
        };
        for line in maps.lines() {
            let mut parts = line.split_whitespace();
            let Some(range) = parts.next() else {
                continue;
            };
            let Some(perms) = parts.next() else {
                continue;
            };
            if perms.as_bytes().get(2) != Some(&b'x') {
                continue;
            }
            let Some((start, _)) = range.split_once('-') else {
                continue;
            };
            if let Ok(base) = u64::from_str_radix(start, 16) {
                return base;
            }
        }
        0
    }

    unsafe fn store_name(dst: *mut libc::c_char, cap: usize, name: &CString) {
        std::ptr::write_bytes(dst, 0, cap);
        let bytes = name.as_bytes_with_nul();
        let count = bytes.len().min(cap);
        copy_nonoverlapping(bytes.as_ptr().cast::<libc::c_char>(), dst, count);
        if count == cap {
            *dst.add(cap - 1) = 0;
        }
    }

    unsafe fn name_nonempty(name: *const libc::c_char) -> bool {
        !name.is_null() && *name != 0
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

    fn shm_name_ptr() -> *mut libc::c_char {
        std::ptr::addr_of_mut!(SHM_NAME).cast::<libc::c_char>()
    }

    fn req_name_ptr() -> *mut libc::c_char {
        std::ptr::addr_of_mut!(REQ_NAME).cast::<libc::c_char>()
    }

    fn rsp_name_ptr() -> *mut libc::c_char {
        std::ptr::addr_of_mut!(RSP_NAME).cast::<libc::c_char>()
    }

    #[no_mangle]
    pub unsafe extern "C" fn RcxPayloadInit() -> bool {
        init_impl()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parse_region_map_line_classifies_readable_file_and_heap_regions() {
            let image = parse_region_map_line("7f00-8000 r-xp 00000000 00:00 1 /tmp/game/bin/game")
                .unwrap();
            assert_eq!(image.start, 0x7f00);
            assert_eq!(image.end, 0x8000);
            assert!(image.readable);
            assert!(image.executable);
            assert!(!image.writable);
            assert_eq!(image.region_type, RCX_RPC_REGION_IMAGE);
            assert_eq!(image.name, b"game");

            let heap = parse_region_map_line("9000-a000 rw-p 00000000 00:00 0 [heap]").unwrap();
            assert_eq!(heap.region_type, RCX_RPC_REGION_PRIVATE);
            assert!(heap.name.is_empty());
            assert!(heap.writable);

            assert!(parse_region_map_line("a000-b000 ---p 00000000 00:00 0").is_none());
        }
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
#[no_mangle]
pub extern "C" fn RcxPayloadInit() -> bool {
    false
}

/// A tiny rlib symbol so the app can depend on this crate and make Cargo build
/// the `cdylib` artifact during normal `cargo run --release`.
pub fn artifact_dependency_marker() {}
