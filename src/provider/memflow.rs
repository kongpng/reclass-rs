//! memflow-backed live process provider.
//!
//! This module intentionally depends only on `memflow` core. Connectors
//! (`qemu`, `kvm`, `pcileech`, `winio`, ...) and OS layers (`win32`) are loaded
//! through memflow's runtime [`Inventory`], so supporting a third-party connector
//! is a plugin installation/configuration concern rather than a Reclass rebuild.

use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::plugin::contract::ProcessInfo;
use crate::provider::{
    read_pages_in_runs, MemoryRegion, ModuleEntry, ModuleLookup, PageMap, Provider, RegionType,
};

use memflow::cglue::CTup3;
use memflow::prelude::v1::*;

/// JSON-encoded provider target used by the in-tree `processmemory` provider,
/// the GPUI attach dialog, and MCP `source.switch`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MemflowAttachConfig {
    /// Dynamic connector name, e.g. `qemu`, `kvm`, `pcileech`, `winio`.
    pub connector: String,
    /// Raw memflow connector args, without the connector name.
    ///
    /// Examples: `win10`, `win10:memmap=map`, `:cache=true`.
    pub connector_args: String,
    /// Dynamic OS plugin name. Defaults to `win32`.
    pub os: String,
    /// Raw memflow OS args, without the OS name.
    ///
    /// For win32 extra args without a target, include the leading colon, e.g.
    /// `:dtb=0x1234`.
    pub os_args: String,
    /// PID wins over `process_name` when both are present.
    pub pid: Option<u32>,
    pub process_name: String,
    /// Writes are disabled unless this is explicitly true.
    pub writable: bool,
    /// Extra plugin directories scanned after memflow's default inventory paths.
    pub inventory_dirs: Vec<String>,
}

impl Default for MemflowAttachConfig {
    fn default() -> Self {
        Self {
            connector: String::new(),
            connector_args: String::new(),
            os: "win32".to_string(),
            os_args: String::new(),
            pid: None,
            process_name: String::new(),
            writable: false,
            inventory_dirs: Vec::new(),
        }
    }
}

impl MemflowAttachConfig {
    pub fn from_target(target: &str) -> std::result::Result<Self, String> {
        let cfg: Self = serde_json::from_str(target)
            .map_err(|err| format!("invalid memflow target JSON: {err}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn to_target(&self) -> std::result::Result<String, String> {
        serde_json::to_string(self).map_err(|err| format!("serialize memflow target: {err}"))
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.os.trim().is_empty() {
            return Err("memflow OS plugin is required, usually \"win32\"".to_string());
        }
        if self.pid.is_none() && self.process_name.trim().is_empty() {
            return Err("memflow attach requires pid or processName".to_string());
        }
        Ok(())
    }

    fn chain_step(name: &str, args: &str) -> String {
        let name = name.trim();
        let args = args.trim();
        if args.is_empty() {
            name.to_string()
        } else {
            format!("{name}:{args}")
        }
    }

    fn os_chain_steps(&self) -> (Vec<String>, Vec<String>) {
        let connectors = if self.connector.trim().is_empty() {
            Vec::new()
        } else {
            vec![Self::chain_step(&self.connector, &self.connector_args)]
        };
        let os_layers = vec![Self::chain_step(&self.os, &self.os_args)];
        (connectors, os_layers)
    }
}

/// Runtime inventory summary for UI/MCP diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemflowInventoryInfo {
    pub connectors: Vec<String>,
    pub os_layers: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn default_plugin_dir() -> Option<String> {
    let path = if cfg!(unix) {
        directories::BaseDirs::new()?
            .home_dir()
            .join(".local")
            .join("lib")
            .join("memflow")
    } else {
        directories::UserDirs::new()?
            .document_dir()?
            .join("memflow")
    };
    let _ = std::fs::create_dir_all(&path);
    Some(path.display().to_string())
}

pub fn inventory_info(extra_dirs: &[String]) -> MemflowInventoryInfo {
    let mut inventory = Inventory::scan();
    let mut warnings = Vec::new();
    for dir in extra_dirs {
        let trimmed = dir.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Err(err) = inventory.add_dir(Path::new(trimmed)) {
            warnings.push(format!("{trimmed}: {err}"));
        }
    }
    let mut connectors = inventory.available_connectors();
    connectors.sort();
    connectors.dedup();
    let mut os_layers = inventory.available_os();
    os_layers.sort();
    os_layers.dedup();
    MemflowInventoryInfo {
        connectors,
        os_layers,
        warnings,
    }
}

fn build_os(cfg: &MemflowAttachConfig) -> std::result::Result<OsInstanceArcBox<'static>, String> {
    let mut inventory = Inventory::scan();
    for dir in &cfg.inventory_dirs {
        let trimmed = dir.trim();
        if trimmed.is_empty() {
            continue;
        }
        inventory
            .add_dir(Path::new(trimmed))
            .map_err(|err| format!("scan memflow plugin dir {trimmed}: {err}"))?;
    }

    let (connector_steps, os_steps) = cfg.os_chain_steps();
    let conn_iter = connector_steps
        .iter()
        .enumerate()
        .map(|(i, step)| (i * 2, step.as_str()));
    let os_iter = os_steps.iter().enumerate().map(|(i, step)| {
        (
            i * 2 + usize::from(!connector_steps.is_empty()),
            step.as_str(),
        )
    });
    let chain =
        OsChain::new(conn_iter, os_iter).map_err(|err| format!("build memflow OS chain: {err}"))?;
    inventory
        .builder()
        .os_chain(chain)
        .build()
        .map_err(|err| format!("open memflow OS chain: {err}"))
}

fn process_info_by_config(
    os: &mut OsInstanceArcBox<'static>,
    cfg: &MemflowAttachConfig,
) -> std::result::Result<memflow::os::ProcessInfo, String> {
    if let Some(pid) = cfg.pid {
        return os
            .process_info_by_pid(pid)
            .map_err(|err| format!("memflow process pid {pid} not found: {err}"));
    }

    let wanted = cfg.process_name.trim();
    let list = os
        .process_info_list()
        .map_err(|err| format!("enumerate memflow processes: {err}"))?;
    list.into_iter()
        .find(|info| info.name.as_ref().eq_ignore_ascii_case(wanted))
        .ok_or_else(|| format!("memflow process \"{wanted}\" not found"))
}

pub fn enumerate_processes(
    cfg: &MemflowAttachConfig,
) -> std::result::Result<Vec<ProcessInfo>, String> {
    let mut os = build_os(cfg)?;
    let mut rows: Vec<ProcessInfo> = os
        .process_info_list()
        .map_err(|err| format!("enumerate memflow processes: {err}"))?
        .into_iter()
        .map(|info| ProcessInfo {
            pid: info.pid,
            name: info.name.as_ref().to_string(),
            path: info.path.as_ref().to_string(),
            is_32bit: pointer_bits(info.proc_arch) == 32,
        })
        .collect();
    rows.sort_by(|a, b| b.pid.cmp(&a.pid).then_with(|| a.name.cmp(&b.name)));
    Ok(rows)
}

fn pointer_bits(arch: ArchitectureIdent) -> i32 {
    match arch {
        ArchitectureIdent::X86(bits, _) => i32::from(bits),
        ArchitectureIdent::AArch64(_) => 64,
        ArchitectureIdent::Unknown(_) => 64,
    }
}

fn address_to_u64(address: Address) -> u64 {
    address.to_umem() as u64
}

pub struct MemflowProvider {
    config: MemflowAttachConfig,
    process: Mutex<IntoProcessInstanceArcBox<'static>>,
    pid: u32,
    process_name: String,
    pointer_size: i32,
    base: u64,
    module_lookup: ModuleLookup,
}

impl MemflowProvider {
    pub fn attach(config: MemflowAttachConfig) -> std::result::Result<Self, String> {
        config.validate()?;
        let mut os = build_os(&config)?;
        let info = process_info_by_config(&mut os, &config)?;
        let pid = info.pid;
        let process_name = info.name.as_ref().to_string();
        let pointer_size = pointer_bits(info.proc_arch) / 8;
        let mut process = os
            .into_process_by_info(info)
            .map_err(|err| format!("open memflow process {process_name} ({pid}): {err}"))?;
        let base = process
            .primary_module()
            .ok()
            .map(|module| address_to_u64(module.base))
            .unwrap_or(0);
        let module_lookup = ModuleLookup::new(enumerate_memflow_modules(&mut process));

        Ok(Self {
            config,
            process: Mutex::new(process),
            pid,
            process_name,
            pointer_size,
            base,
            module_lookup,
        })
    }

    fn with_process<T>(
        &self,
        fallback: T,
        f: impl FnOnce(&mut IntoProcessInstanceArcBox<'static>) -> T,
    ) -> T {
        match self.process.lock() {
            Ok(mut process) => f(&mut process),
            Err(_) => fallback,
        }
    }
}

impl Provider for MemflowProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        if buf.is_empty() {
            return true;
        }
        self.with_process(false, |process| {
            process
                .read_raw_into(Address::from(addr), buf)
                .data_part()
                .is_ok()
        })
    }

    fn read_pages(&self, pages: &[u64]) -> PageMap {
        self.with_process(PageMap::new(), |process| {
            read_pages_in_runs(pages, |addr, buf| {
                process
                    .read_raw_into(Address::from(addr), buf)
                    .data_part()
                    .is_ok()
            })
        })
    }

    fn size(&self) -> i32 {
        i32::MAX
    }

    fn write(&self, addr: u64, data: &[u8]) -> bool {
        if !self.config.writable {
            return false;
        }
        if data.is_empty() {
            return true;
        }
        self.with_process(false, |process| {
            process
                .write_raw(Address::from(addr), data)
                .data_part()
                .is_ok()
        })
    }

    fn is_writable(&self) -> bool {
        self.config.writable
    }

    fn name(&self) -> String {
        if self.process_name.is_empty() {
            format!("pid {}", self.pid)
        } else {
            format!("{} (pid {})", self.process_name, self.pid)
        }
    }

    fn is_live(&self) -> bool {
        true
    }

    fn prefers_coalesced_rescan_reads(&self) -> bool {
        true
    }

    fn kind(&self) -> String {
        "Process".to_string()
    }

    fn pointer_size(&self) -> i32 {
        self.pointer_size
    }

    fn base(&self) -> u64 {
        self.base
    }

    fn get_symbol(&self, addr: u64) -> String {
        self.module_lookup.symbol_for_addr_upper(addr)
    }

    fn symbol_to_address(&self, name: &str) -> u64 {
        self.module_lookup.symbol_to_address(name)
    }

    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        self.with_process(Vec::new(), |process| {
            process
                .mapped_mem_vec(0)
                .into_iter()
                .map(|CTup3(base, size, page_type)| MemoryRegion {
                    base: address_to_u64(base),
                    size: size as u64,
                    readable: true,
                    writable: page_type.contains(PageType::WRITEABLE),
                    executable: !page_type.contains(PageType::NOEXEC),
                    module_name: String::new(),
                    region_type: RegionType::Private,
                })
                .collect()
        })
    }

    fn trusts_enumerated_region_readability(&self) -> bool {
        true
    }

    fn enumerate_modules(&self) -> Vec<ModuleEntry> {
        self.module_lookup.clone_modules()
    }

    fn is_readable(&self, _addr: u64, len: i32) -> bool {
        len >= 0
    }
}

fn enumerate_memflow_modules(process: &mut IntoProcessInstanceArcBox<'static>) -> Vec<ModuleEntry> {
    process
        .module_list()
        .unwrap_or_default()
        .into_iter()
        .map(|module| ModuleEntry {
            name: module.name.as_ref().to_string(),
            full_path: module.path.as_ref().to_string(),
            base: address_to_u64(module.base),
            size: module.size as u64,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_to_win32_and_read_only() {
        let cfg = MemflowAttachConfig {
            pid: Some(4),
            ..MemflowAttachConfig::default()
        };
        assert_eq!(cfg.os, "win32");
        assert!(!cfg.writable);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn config_target_round_trip() {
        let cfg = MemflowAttachConfig {
            connector: "qemu".to_string(),
            connector_args: "win10".to_string(),
            os: "win32".to_string(),
            os_args: ":dtb=0x1234".to_string(),
            pid: Some(1234),
            process_name: String::new(),
            writable: true,
            inventory_dirs: vec!["/tmp/memflow".to_string()],
        };
        let target = cfg.to_target().unwrap();
        let decoded = MemflowAttachConfig::from_target(&target).unwrap();
        assert_eq!(decoded, cfg);
    }

    #[test]
    fn config_requires_attach_target() {
        let cfg = MemflowAttachConfig::default();
        assert!(cfg.validate().unwrap_err().contains("pid or processName"));
    }

    #[test]
    fn chain_step_preserves_raw_args() {
        let cfg = MemflowAttachConfig {
            connector: "qemu".to_string(),
            connector_args: "vm:memmap=map".to_string(),
            os_args: ":dtb=0x1234".to_string(),
            pid: Some(1),
            ..MemflowAttachConfig::default()
        };
        let (connectors, os_layers) = cfg.os_chain_steps();
        assert_eq!(connectors, ["qemu:vm:memmap=map"]);
        assert_eq!(os_layers, ["win32::dtb=0x1234"]);
    }

    #[test]
    fn module_lookup_indexes_address_name_path_and_file_name() {
        let modules = vec![
            ModuleEntry {
                name: "Second.dll".to_string(),
                full_path: r"C:\Game\Second.dll".to_string(),
                base: 0x3000,
                size: 0x1000,
            },
            ModuleEntry {
                name: "FirstModule".to_string(),
                full_path: r"C:\Game\Bin\First.dll".to_string(),
                base: 0x1000,
                size: 0x1000,
            },
        ];
        let lookup = ModuleLookup::new(modules);

        assert_eq!(lookup.find_by_addr(0x1004).unwrap().name, "FirstModule");
        assert!(lookup.find_by_addr(0x2000).is_none());
        assert_eq!(lookup.symbol_to_address("firstmodule"), 0x1000);
        assert_eq!(lookup.symbol_to_address("first.dll"), 0x1000);
        assert_eq!(lookup.symbol_to_address(r"c:\game\bin\first.dll"), 0x1000);
        assert_eq!(lookup.symbol_to_address("second.dll"), 0x3000);
    }
}
