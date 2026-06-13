//! Wire contract shared by the Remote Process Memory provider and `rcx_payload`.
//!
//! This mirrors `plugins/RemoteProcessMemory/rcx_rpc_protocol.h` from the C++
//! tree. Keep these structs `repr(C)` and naturally aligned.

use bytemuck::{Pod, Zeroable};

pub const RCX_RPC_VERSION: u32 = 1;
pub const RCX_RPC_MAX_BATCH: usize = 256;
pub const RCX_RPC_SHM_SIZE: usize = 1024 * 1024;
pub const RCX_RPC_HEADER_SIZE: usize = 4096;
pub const RCX_RPC_DATA_OFFSET: usize = RCX_RPC_HEADER_SIZE;
pub const RCX_RPC_DATA_SIZE: usize = RCX_RPC_SHM_SIZE - RCX_RPC_DATA_OFFSET;

pub const RCX_RPC_STATUS_OK: u32 = 0;
pub const RCX_RPC_STATUS_ERROR: u32 = 1;
pub const RCX_RPC_STATUS_PARTIAL: u32 = 2;

pub const RPC_CMD_NONE: u32 = 0;
pub const RPC_CMD_READ_BATCH: u32 = 1;
pub const RPC_CMD_WRITE: u32 = 2;
pub const RPC_CMD_ENUM_MODULES: u32 = 3;
pub const RPC_CMD_PING: u32 = 4;
pub const RPC_CMD_SHUTDOWN: u32 = 5;
pub const RPC_CMD_ENUM_REGIONS: u32 = 6;

pub const RCX_RPC_REGION_READABLE: u32 = 1 << 0;
pub const RCX_RPC_REGION_WRITABLE: u32 = 1 << 1;
pub const RCX_RPC_REGION_EXECUTABLE: u32 = 1 << 2;

pub const RCX_RPC_REGION_IMAGE: u32 = 0;
pub const RCX_RPC_REGION_MAPPED: u32 = 1;
pub const RCX_RPC_REGION_PRIVATE: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct RcxRpcReadEntry {
    pub address: u64,
    pub length: u32,
    pub data_offset: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct RcxRpcModuleEntry {
    pub base: u64,
    pub size: u64,
    pub name_offset: u32,
    pub name_length: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct RcxRpcRegionEntry {
    pub base: u64,
    pub size: u64,
    pub name_offset: u32,
    pub name_length: u32,
    pub flags: u32,
    pub region_type: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RcxRpcHeader {
    pub version: u32,
    pub payload_ready: u32,
    pub command: u32,
    pub request_count: u32,
    pub write_address: u64,
    pub write_length: u32,
    pub status: u32,
    pub response_count: u32,
    pub total_data_used: u32,
    pub image_base: u64,
    pub pointer_size: u32,
    pub _pad: [u8; RCX_RPC_HEADER_SIZE - 52],
}

unsafe impl Zeroable for RcxRpcHeader {}
unsafe impl Pod for RcxRpcHeader {}

impl Default for RcxRpcHeader {
    fn default() -> Self {
        RcxRpcHeader {
            version: 0,
            payload_ready: 0,
            command: 0,
            request_count: 0,
            write_address: 0,
            write_length: 0,
            status: 0,
            response_count: 0,
            total_data_used: 0,
            image_base: 0,
            pointer_size: 0,
            _pad: [0; RCX_RPC_HEADER_SIZE - 52],
        }
    }
}

impl core::fmt::Debug for RcxRpcHeader {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RcxRpcHeader")
            .field("version", &self.version)
            .field("payload_ready", &self.payload_ready)
            .field("command", &self.command)
            .field("request_count", &self.request_count)
            .field("write_address", &self.write_address)
            .field("write_length", &self.write_length)
            .field("status", &self.status)
            .field("response_count", &self.response_count)
            .field("total_data_used", &self.total_data_used)
            .field("image_base", &self.image_base)
            .field("pointer_size", &self.pointer_size)
            .finish()
    }
}

pub fn shm_name(pid: u32) -> String {
    #[cfg(windows)]
    {
        format!("Local\\RCX_SHM_{pid}")
    }
    #[cfg(not(windows))]
    {
        format!("/rcx_shm_{pid}")
    }
}

pub fn req_name(pid: u32) -> String {
    #[cfg(windows)]
    {
        format!("Local\\RCX_REQ_{pid}")
    }
    #[cfg(not(windows))]
    {
        format!("/rcx_req_{pid}")
    }
}

pub fn rsp_name(pid: u32) -> String {
    #[cfg(windows)]
    {
        format!("Local\\RCX_RSP_{pid}")
    }
    #[cfg(not(windows))]
    {
        format!("/rcx_rsp_{pid}")
    }
}

const _: () = assert!(core::mem::size_of::<RcxRpcReadEntry>() == 16);
const _: () = assert!(core::mem::size_of::<RcxRpcModuleEntry>() == 24);
const _: () = assert!(core::mem::size_of::<RcxRpcRegionEntry>() == 32);
const _: () = assert!(core::mem::size_of::<RcxRpcHeader>() == RCX_RPC_HEADER_SIZE);
const _: () = assert!(core::mem::offset_of!(RcxRpcHeader, version) == 0);
const _: () = assert!(core::mem::offset_of!(RcxRpcHeader, payload_ready) == 4);
const _: () = assert!(core::mem::offset_of!(RcxRpcHeader, command) == 8);
const _: () = assert!(core::mem::offset_of!(RcxRpcHeader, request_count) == 12);
const _: () = assert!(core::mem::offset_of!(RcxRpcHeader, write_address) == 16);
const _: () = assert!(core::mem::offset_of!(RcxRpcHeader, write_length) == 24);
const _: () = assert!(core::mem::offset_of!(RcxRpcHeader, status) == 28);
const _: () = assert!(core::mem::offset_of!(RcxRpcHeader, response_count) == 32);
const _: () = assert!(core::mem::offset_of!(RcxRpcHeader, total_data_used) == 36);
const _: () = assert!(core::mem::offset_of!(RcxRpcHeader, image_base) == 40);
const _: () = assert!(core::mem::offset_of!(RcxRpcHeader, pointer_size) == 48);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_cpp_protocol() {
        #[cfg(windows)]
        {
            assert_eq!(shm_name(123), "Local\\RCX_SHM_123");
            assert_eq!(req_name(123), "Local\\RCX_REQ_123");
            assert_eq!(rsp_name(123), "Local\\RCX_RSP_123");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(shm_name(123), "/rcx_shm_123");
            assert_eq!(req_name(123), "/rcx_req_123");
            assert_eq!(rsp_name(123), "/rcx_rsp_123");
        }
    }

    #[test]
    fn wire_layout_matches_cpp_static_asserts() {
        assert_eq!(core::mem::size_of::<RcxRpcReadEntry>(), 16);
        assert_eq!(core::mem::size_of::<RcxRpcModuleEntry>(), 24);
        assert_eq!(core::mem::size_of::<RcxRpcRegionEntry>(), 32);
        assert_eq!(core::mem::size_of::<RcxRpcHeader>(), 4096);
    }
}
