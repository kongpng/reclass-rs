//! `example-provider` — a minimal native Reclass **provider** plugin (design §6
//! Phase 3 deliverable, §G example). It exposes a tiny in-memory provider whose
//! `read()` serves a fixed ramp buffer, proving the §8 `RBox<dyn Provider_TO>`
//! hot-path crosses the ABI as a direct native call.
//!
//! Build: `cargo build -p example-provider` → a `cdylib` the host loads with
//! `reclass`'s `plugins`-feature loader.

use reclass_plugin::{
    CommandResult, Contribution, Host, Manifest, Permission, Plugin, ProcessInfo, Provider,
};

/// The provider this plugin contributes: a 256-byte ramp (`buf[i] == i`) in memory.
struct RampProvider {
    target: String,
}

impl Provider for RampProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        let start = addr as usize;
        let end = match start.checked_add(buf.len()) {
            Some(e) => e,
            None => return false,
        };
        if end > 256 {
            return false;
        }
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (start + i) as u8;
        }
        true
    }
    fn size(&self) -> i32 {
        256
    }
    fn name(&self) -> String {
        format!("ramp:{}", self.target)
    }
    fn kind(&self) -> String {
        "Example".to_string()
    }
    fn pointer_size(&self) -> i32 {
        8
    }
}

/// The plugin object. Carries the manifest + the single provider contribution.
#[derive(Default)]
struct ExampleProviderPlugin;

impl Plugin for ExampleProviderPlugin {
    fn manifest(&self) -> Manifest {
        Manifest::new("Example Provider", env!("CARGO_PKG_VERSION"))
            .author("Reclass (Rust port) examples")
            .description("A ramp-buffer demo provider, loaded as a native plugin.")
            .permissions(vec![Permission::ReadMemory, Permission::AddProvider])
    }

    fn contributions(&self) -> Vec<Contribution> {
        vec![Contribution::Provider {
            provides_process_list: true,
        }]
    }

    fn handle_command(&mut self, _id: &str, _args: &str, host: &mut Host<'_>) -> CommandResult {
        host.show_toast("example-provider: nothing to do");
        CommandResult::handled()
    }
}

reclass_plugin::export_plugin! {
    new_plugin: || ExampleProviderPlugin,
    can_handle: |target: &str| !target.is_empty(),
    create_provider: |target: &str| -> Result<RampProvider, String> {
        if target.is_empty() {
            return Err("example-provider: empty target".to_string());
        }
        Ok(RampProvider { target: target.to_string() })
    },
    enumerate_processes: || -> Vec<ProcessInfo> {
        vec![ProcessInfo {
            pid: 4321,
            name: "demo.exe".to_string(),
            path: "/demo/demo.exe".to_string(),
            is_32bit: false,
        }]
    },
}
