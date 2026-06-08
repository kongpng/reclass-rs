//! The in-tree built-in provider plugins: File, Process Memory, Remote Process
//! Memory, Buffer, Snapshot, Null, plus memflow as a separate connector-backed
//! provider when `memflow-provider` is enabled.
//!
//! Each is a tiny [`Plugin`] whose only contribution is a
//! [`Contribution::Provider`] wrapping an in-tree provider
//! ([`FileProvider`]/[`BufferProvider`]/[`SnapshotProvider`]/[`NullProvider`],
//! first-party live providers, plus [`MemflowProvider`](crate::provider::MemflowProvider)).
//! This is the **bootstrap loader** of design §2 ("In-tree is the bootstrap — zero
//! ABI risk, proves the contract"): the built-ins flow through the same contract a
//! native plugin will, so [`PluginManager`](crate::plugin::manager::PluginManager)
//! treats them uniformly.
//!
//! These do **not** change observable behavior: the controller still attaches a
//! `BufferProvider::from_file` for the "File" source (cpp_reference §0 — the C++
//! "File" source is also a buffered file). The plugin path is the registration /
//! listing layer, not a new attach mechanism.

use std::sync::Arc;

#[cfg(all(windows, feature = "kernel-provider"))]
use crate::plugin::contract::{CommandResult, CommandSlot};
use crate::plugin::contract::{Contribution, Plugin};
use crate::plugin::manifest::{Permission, PluginManifest};
use crate::plugin::provider_spec::{ProviderSpec, SharedProvider};
#[cfg(all(windows, feature = "kernel-provider"))]
use crate::provider::KernelMemoryProvider;
#[cfg(feature = "process-provider")]
use crate::provider::LocalProcessProvider;
#[cfg(feature = "remote-process-provider")]
use crate::provider::RemoteProcessProvider;
#[cfg(all(windows, feature = "windbg-provider"))]
use crate::provider::WinDbgMemoryProvider;
use crate::provider::{BufferProvider, FileProvider, NullProvider, SnapshotProvider};
#[cfg(feature = "memflow-provider")]
use crate::provider::{MemflowAttachConfig, MemflowProvider};

/// The "File" source (cpp_reference §3 — the only built-in the C++ surfaced).
/// `can_handle` accepts any non-empty path; `create_provider` mmaps it via
/// [`FileProvider`], falling back to an in-RAM [`BufferProvider`] when mmap fails
/// (e.g. a zero-length file), matching the controller's tolerant file attach.
pub struct FilePlugin {
    manifest: PluginManifest,
}

impl Default for FilePlugin {
    fn default() -> Self {
        FilePlugin {
            manifest: PluginManifest::builtin(
                "File",
                "Reads a project's data from a binary file on disk.",
                vec![Permission::Filesystem, Permission::AddProvider],
            ),
        }
    }
}

impl Plugin for FilePlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    fn contributions(&self) -> Vec<Contribution> {
        vec![Contribution::Provider(ProviderSpec::new(
            |target| !target.is_empty(),
            |target| match FileProvider::open(target) {
                Ok(p) => Ok(Arc::new(p) as SharedProvider),
                // mmap can fail on empty files; fall back to a buffered read so the
                // "File" source still attaches (the controller's behavior).
                Err(_) => Ok(Arc::new(BufferProvider::from_file(target)) as SharedProvider),
            },
        ))]
    }
}

/// The "Buffer" source — an in-memory byte buffer (imports / tests). `can_handle`
/// treats the target as a file path to slurp into RAM via
/// [`BufferProvider::from_file`]; an empty target yields an empty buffer.
pub struct BufferPlugin {
    manifest: PluginManifest,
}

impl Default for BufferPlugin {
    fn default() -> Self {
        BufferPlugin {
            manifest: PluginManifest::builtin(
                "Buffer",
                "Reads from an in-memory byte buffer (imports / tests).",
                vec![Permission::AddProvider],
            ),
        }
    }
}

impl Plugin for BufferPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    fn contributions(&self) -> Vec<Contribution> {
        vec![Contribution::Provider(ProviderSpec::new(
            |_| true,
            |target| {
                let p = if target.is_empty() {
                    BufferProvider::default()
                } else {
                    BufferProvider::from_file(target)
                };
                Ok(Arc::new(p) as SharedProvider)
            },
        ))]
    }
}

/// The "Snapshot" source — a captured memory snapshot (cpp_reference §9). Built by
/// the controller from async reads; the spec creates an empty snapshot (no backing
/// provider, no pages) so the listing/contract path is exercised. The real
/// snapshot is assembled by the controller, not attached by target string, so
/// `can_handle` is `false` (this plugin only contributes the registry entry).
pub struct SnapshotPlugin {
    manifest: PluginManifest,
}

impl Default for SnapshotPlugin {
    fn default() -> Self {
        SnapshotPlugin {
            manifest: PluginManifest::builtin(
                "Snapshot",
                "Reads from a captured memory snapshot.",
                vec![Permission::AddProvider],
            ),
        }
    }
}

impl Plugin for SnapshotPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    fn contributions(&self) -> Vec<Contribution> {
        vec![Contribution::Provider(ProviderSpec::new(
            // Snapshots are controller-assembled, not target-attached.
            |_| false,
            |_| Ok(Arc::new(SnapshotProvider::new(None, Default::default(), 0)) as SharedProvider),
        ))]
    }
}

/// The "Null" source — the detached provider a fresh document holds
/// (cpp_reference §0; `null_provider.h`). Every read returns zero.
pub struct NullPlugin {
    manifest: PluginManifest,
}

impl Default for NullPlugin {
    fn default() -> Self {
        NullPlugin {
            manifest: PluginManifest::builtin(
                "Null",
                "The detached source — every read returns zero.",
                vec![],
            ),
        }
    }
}

impl Plugin for NullPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    fn contributions(&self) -> Vec<Contribution> {
        vec![Contribution::Provider(ProviderSpec::new(
            |_| false,
            |_| Ok(Arc::new(NullProvider) as SharedProvider),
        ))]
    }
}

/// The local OS "Process Memory" source.
#[cfg(feature = "process-provider")]
pub struct ProcessMemoryPlugin {
    manifest: PluginManifest,
}

#[cfg(feature = "process-provider")]
impl Default for ProcessMemoryPlugin {
    fn default() -> Self {
        ProcessMemoryPlugin {
            manifest: PluginManifest::builtin(
                "Process Memory",
                "Reads and writes a local live process through the current OS APIs.",
                vec![
                    Permission::ReadMemory,
                    Permission::WriteMemory,
                    Permission::AddProvider,
                ],
            ),
        }
    }
}

#[cfg(feature = "process-provider")]
impl Plugin for ProcessMemoryPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn contributions(&self) -> Vec<Contribution> {
        vec![Contribution::Provider(
            ProviderSpec::new(LocalProcessProvider::can_handle, |target| {
                Ok(Arc::new(LocalProcessProvider::attach(target)?) as SharedProvider)
            })
            .with_enumerate(|| Some(LocalProcessProvider::enumerate_processes())),
        )]
    }
}

/// The remote payload-backed "Remote Process Memory" source.
#[cfg(feature = "remote-process-provider")]
pub struct RemoteProcessMemoryPlugin {
    manifest: PluginManifest,
}

#[cfg(feature = "remote-process-provider")]
impl Default for RemoteProcessMemoryPlugin {
    fn default() -> Self {
        let _ = rcx_payload::artifact_dependency_marker as fn();
        RemoteProcessMemoryPlugin {
            manifest: PluginManifest::builtin(
                "Remote Process Memory",
                "Reads and writes a live process through an injected rcx_payload.",
                vec![
                    Permission::ReadMemory,
                    Permission::WriteMemory,
                    Permission::AddProvider,
                ],
            ),
        }
    }
}

#[cfg(feature = "remote-process-provider")]
impl Plugin for RemoteProcessMemoryPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn contributions(&self) -> Vec<Contribution> {
        let spec = ProviderSpec::new(RemoteProcessProvider::can_handle, |target| {
            Ok(Arc::new(RemoteProcessProvider::attach(target)?) as SharedProvider)
        });
        #[cfg(feature = "process-provider")]
        let spec = spec.with_enumerate(|| Some(LocalProcessProvider::enumerate_processes()));
        vec![Contribution::Provider(spec)]
    }
}

/// The Windows rcxdrv.sys-backed "Kernel Memory" source.
#[cfg(all(windows, feature = "kernel-provider"))]
pub struct KernelMemoryPlugin {
    manifest: PluginManifest,
}

#[cfg(all(windows, feature = "kernel-provider"))]
impl Default for KernelMemoryPlugin {
    fn default() -> Self {
        KernelMemoryPlugin {
            manifest: PluginManifest::builtin(
                "Kernel Memory",
                "Reads and writes process or physical memory through rcxdrv.sys.",
                vec![
                    Permission::ReadMemory,
                    Permission::WriteMemory,
                    Permission::AddProvider,
                ],
            ),
        }
    }
}

#[cfg(all(windows, feature = "kernel-provider"))]
impl Plugin for KernelMemoryPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn contributions(&self) -> Vec<Contribution> {
        vec![
            Contribution::Provider(
                ProviderSpec::new(KernelMemoryProvider::can_handle, |target| {
                    Ok(Arc::new(KernelMemoryProvider::attach(target)?) as SharedProvider)
                })
                .with_enumerate(|| Some(KernelMemoryProvider::enumerate_processes())),
            ),
            Contribution::Command {
                id: "kernel.unload_driver".to_string(),
                title: "Unload Kernel Driver".to_string(),
                slot: CommandSlot::SourceMenu,
            },
        ]
    }

    fn handle_command(
        &mut self,
        id: &str,
        _args: serde_json::Value,
        _host: &mut dyn crate::plugin::host::PluginHost,
    ) -> CommandResult {
        if id != "kernel.unload_driver" {
            return CommandResult::default();
        }
        match KernelMemoryProvider::unload_driver_service() {
            Ok(()) => CommandResult::toast("Kernel driver unloaded"),
            Err(err) => CommandResult::toast(err),
        }
    }
}

/// The Windows DbgEng-backed "WinDbg Memory" source.
#[cfg(all(windows, feature = "windbg-provider"))]
pub struct WinDbgMemoryPlugin {
    manifest: PluginManifest,
}

#[cfg(all(windows, feature = "windbg-provider"))]
impl Default for WinDbgMemoryPlugin {
    fn default() -> Self {
        WinDbgMemoryPlugin {
            manifest: PluginManifest::builtin(
                "WinDbg Memory",
                "Reads memory through DbgEng from a debug server, live process, or dump file.",
                vec![
                    Permission::ReadMemory,
                    Permission::WriteMemory,
                    Permission::AddProvider,
                ],
            ),
        }
    }
}

#[cfg(all(windows, feature = "windbg-provider"))]
impl Plugin for WinDbgMemoryPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn contributions(&self) -> Vec<Contribution> {
        vec![Contribution::Provider(ProviderSpec::new(
            WinDbgMemoryProvider::can_handle,
            |target| Ok(Arc::new(WinDbgMemoryProvider::attach(target)?) as SharedProvider),
        ))]
    }
}

/// The memflow-backed "Memflow Process Memory" source. The target is a JSON-encoded
/// [`MemflowAttachConfig`] so the UI and MCP can share one provider factory.
#[cfg(feature = "memflow-provider")]
pub struct MemflowProcessPlugin {
    manifest: PluginManifest,
}

#[cfg(feature = "memflow-provider")]
impl Default for MemflowProcessPlugin {
    fn default() -> Self {
        MemflowProcessPlugin {
            manifest: PluginManifest::builtin(
                "Memflow Process Memory",
                "Reads a live process through memflow connector and OS plugins.",
                vec![Permission::AddProvider],
            ),
        }
    }
}

#[cfg(feature = "memflow-provider")]
impl Plugin for MemflowProcessPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn contributions(&self) -> Vec<Contribution> {
        vec![Contribution::Provider(ProviderSpec::new(
            |target| MemflowAttachConfig::from_target(target).is_ok(),
            |target| {
                let cfg = MemflowAttachConfig::from_target(target)?;
                let provider = MemflowProvider::attach(cfg)?;
                Ok(Arc::new(provider) as SharedProvider)
            },
        ))]
    }
}

/// The in-tree built-in plugins, in the C++ registration order the Manage
/// Plugins dialog lists.
pub fn builtin_plugins() -> Vec<Box<dyn Plugin>> {
    let mut plugins: Vec<Box<dyn Plugin>> = Vec::new();
    plugins.push(Box::new(FilePlugin::default()));
    #[cfg(feature = "process-provider")]
    plugins.push(Box::new(ProcessMemoryPlugin::default()));
    #[cfg(feature = "remote-process-provider")]
    plugins.push(Box::new(RemoteProcessMemoryPlugin::default()));
    #[cfg(all(windows, feature = "kernel-provider"))]
    plugins.push(Box::new(KernelMemoryPlugin::default()));
    #[cfg(all(windows, feature = "windbg-provider"))]
    plugins.push(Box::new(WinDbgMemoryPlugin::default()));
    #[cfg(feature = "memflow-provider")]
    plugins.push(Box::new(MemflowProcessPlugin::default()));
    plugins.push(Box::new(BufferPlugin::default()));
    plugins.push(Box::new(SnapshotPlugin::default()));
    plugins.push(Box::new(NullPlugin::default()));
    plugins
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::contract::Contribution;

    fn provider_spec(p: &dyn Plugin) -> ProviderSpec {
        p.contributions()
            .into_iter()
            .find_map(|c| match c {
                Contribution::Provider(spec) => Some(spec),
                _ => None,
            })
            .expect("built-in contributes a provider")
    }

    fn builtin_ids() -> Vec<&'static str> {
        let mut ids = vec!["file"];
        #[cfg(feature = "process-provider")]
        ids.push("processmemory");
        #[cfg(feature = "remote-process-provider")]
        ids.push("remoteprocessmemory");
        #[cfg(all(windows, feature = "kernel-provider"))]
        ids.push("kernelmemory");
        #[cfg(all(windows, feature = "windbg-provider"))]
        ids.push("windbgmemory");
        #[cfg(feature = "memflow-provider")]
        ids.push("memflowprocessmemory");
        ids.extend(["buffer", "snapshot", "null"]);
        ids
    }

    #[test]
    fn each_builtin_has_a_derived_identifier() {
        assert_eq!(FilePlugin::default().manifest().identifier(), "file");
        assert_eq!(BufferPlugin::default().manifest().identifier(), "buffer");
        assert_eq!(
            SnapshotPlugin::default().manifest().identifier(),
            "snapshot"
        );
        assert_eq!(NullPlugin::default().manifest().identifier(), "null");
        #[cfg(feature = "process-provider")]
        assert_eq!(
            ProcessMemoryPlugin::default().manifest().identifier(),
            "processmemory"
        );
        #[cfg(feature = "remote-process-provider")]
        assert_eq!(
            RemoteProcessMemoryPlugin::default().manifest().identifier(),
            "remoteprocessmemory"
        );
        #[cfg(all(windows, feature = "kernel-provider"))]
        assert_eq!(
            KernelMemoryPlugin::default().manifest().identifier(),
            "kernelmemory"
        );
        #[cfg(all(windows, feature = "windbg-provider"))]
        assert_eq!(
            WinDbgMemoryPlugin::default().manifest().identifier(),
            "windbgmemory"
        );
        #[cfg(feature = "memflow-provider")]
        assert_eq!(
            MemflowProcessPlugin::default().manifest().identifier(),
            "memflowprocessmemory"
        );
    }

    #[test]
    fn builtin_plugins_are_in_cpp_listing_order() {
        let ids: Vec<String> = builtin_plugins()
            .iter()
            .map(|p| p.manifest().identifier())
            .collect();
        assert_eq!(ids, builtin_ids());
    }

    #[test]
    fn file_plugin_creates_a_buffer_for_empty_path_fallback() {
        // A nonexistent path: FileProvider::open errors → BufferProvider fallback
        // (empty buffer, size 0) — still a valid attach, no panic.
        let p = FilePlugin::default();
        let spec = provider_spec(&p);
        assert!(spec.can_handle("/some/path"));
        assert!(!spec.can_handle(""));
        let prov = spec.create_provider("/definitely/not/a/real/file").unwrap();
        assert_eq!(prov.size(), 0);
    }

    #[test]
    fn buffer_plugin_creates_empty_buffer_for_empty_target() {
        let spec = provider_spec(&BufferPlugin::default());
        let prov = spec.create_provider("").unwrap();
        assert_eq!(prov.size(), 0);
        assert_eq!(prov.kind(), "File");
    }

    #[test]
    fn file_plugin_reads_a_real_temp_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("rcx-plugin-test-{}.bin", std::process::id()));
        std::fs::write(&path, [0xAA, 0xBB, 0xCC, 0xDD]).unwrap();

        let spec = provider_spec(&FilePlugin::default());
        let prov = spec.create_provider(path.to_str().unwrap()).unwrap();
        assert_eq!(prov.size(), 4);
        let mut buf = [0u8; 2];
        assert!(prov.read(1, &mut buf));
        assert_eq!(buf, [0xBB, 0xCC]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn null_and_snapshot_specs_decline_target_attach() {
        // Both are not target-attachable (Null is the detached source; Snapshot is
        // controller-assembled) but still create a valid provider for the contract.
        let null = provider_spec(&NullPlugin::default());
        assert!(!null.can_handle("anything"));
        assert_eq!(null.create_provider("").unwrap().size(), 0);

        let snap = provider_spec(&SnapshotPlugin::default());
        assert!(!snap.can_handle("anything"));
        // Snapshot with no pages reads as zero / size 0.
        let prov = snap.create_provider("").unwrap();
        assert_eq!(prov.size(), 0);
    }
}
