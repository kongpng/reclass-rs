//! Target/session status model shared by the bottom status bar and the Target
//! dock panel.
//!
//! The provider is the source of truth. This module keeps the rendering surfaces
//! thin: they receive a compact [`TargetStatusSummary`] instead of reaching into
//! controller/source bookkeeping independently.

use std::path::Path;

use crate::controller::{RcxController, SavedSourceEntry};
use crate::core::{NodeKind, NodeTree};
use crate::provider::Provider;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TargetHealth {
    #[default]
    NoSource,
    Static,
    Live,
    Stale,
    Disconnected,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ReattachHealth {
    #[default]
    None,
    Ready,
    MissingFile,
    MissingTarget,
}

/// Cheap, paint-safe target summary. Expensive enumerations such as full module
/// and region lists stay in the inspector panel.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TargetStatusSummary {
    pub provider_kind: String,
    pub provider_name: String,
    pub target_label: String,
    pub valid: bool,
    pub live: bool,
    pub last_read_ok: bool,
    pub writable: bool,
    pub pointer_size: i32,
    pub base: u64,
    pub view_class: String,
    pub view_base: u64,
    pub view_formula: String,
    pub size: i32,
    pub saved_kind: String,
    pub saved_target: String,
    pub reattach: ReattachHealth,
}

impl TargetStatusSummary {
    pub fn for_controller(ctrl: &RcxController) -> Self {
        let idx = ctrl.active_source_index();
        let saved = (idx >= 0)
            .then(|| ctrl.saved_sources().get(idx as usize))
            .flatten();
        let data_path = ctrl.document().data_path.as_deref();
        let mut summary = Self::from_provider(ctrl.provider().as_ref(), saved, data_path);
        summary.view_class = view_class_name(ctrl.tree(), ctrl.view_root_id());
        summary.view_base = ctrl.tree().base_address;
        summary.view_formula = ctrl.tree().base_address_formula.clone();
        summary.last_read_ok = ctrl.last_read_ok();
        summary
    }

    pub fn from_provider(
        provider: &dyn Provider,
        saved: Option<&SavedSourceEntry>,
        data_path: Option<&Path>,
    ) -> Self {
        let provider_name = provider.name();
        let valid = provider.is_valid();
        if saved.is_none() && data_path.is_none() && !valid && provider_name.is_empty() {
            return Self::no_source();
        }

        let provider_kind = saved
            .map(|s| s.kind.clone())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| provider.kind());
        let target_label = saved
            .map(|s| s.display_name.clone())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                data_path.map(|p| match p.file_name().and_then(|s| s.to_str()) {
                    Some(name) => name.to_string(),
                    None => p.to_string_lossy().into_owned(),
                })
            })
            .or_else(|| (!provider_name.trim().is_empty()).then(|| provider_name.clone()))
            .unwrap_or_else(|| provider_kind.clone());

        TargetStatusSummary {
            provider_kind,
            provider_name,
            target_label,
            valid,
            live: provider.is_live(),
            last_read_ok: true,
            writable: provider.is_writable(),
            pointer_size: provider.pointer_size(),
            base: provider.base(),
            view_class: String::new(),
            view_base: provider.base(),
            view_formula: String::new(),
            size: provider.size(),
            saved_kind: saved.map(|s| s.kind.clone()).unwrap_or_default(),
            saved_target: saved
                .map(|s| {
                    if !s.provider_target.trim().is_empty() {
                        s.provider_target.clone()
                    } else if !s.file_path.trim().is_empty() {
                        s.file_path.clone()
                    } else {
                        s.display_name.clone()
                    }
                })
                .unwrap_or_default(),
            reattach: saved.map_or(ReattachHealth::None, reattach_health),
        }
    }

    pub fn no_source() -> Self {
        TargetStatusSummary {
            provider_kind: "None".to_string(),
            target_label: "No source".to_string(),
            ..Self::default()
        }
    }

    pub fn health(&self) -> TargetHealth {
        if self.provider_kind == "None" {
            TargetHealth::NoSource
        } else if !self.valid {
            TargetHealth::Disconnected
        } else if self.live && !self.last_read_ok {
            TargetHealth::Stale
        } else if self.live {
            TargetHealth::Live
        } else {
            TargetHealth::Static
        }
    }

    pub fn health_label(&self) -> &'static str {
        match self.health() {
            TargetHealth::NoSource => "no source",
            TargetHealth::Static => "static",
            TargetHealth::Live => "live",
            TargetHealth::Stale => "stale",
            TargetHealth::Disconnected => "disconnected",
        }
    }

    pub fn access_label(&self) -> &'static str {
        if self.writable {
            "W"
        } else {
            "RO"
        }
    }

    pub fn pointer_label(&self) -> String {
        match self.pointer_size {
            4 | 8 => format!("{}b", self.pointer_size * 8),
            n if n > 0 => format!("{n}B ptr"),
            _ => "ptr ?".to_string(),
        }
    }

    pub fn base_label(&self) -> String {
        if self.base == 0 {
            "base 0".to_string()
        } else {
            format!("base 0x{:X}", self.base)
        }
    }

    pub fn view_base_label(&self) -> String {
        if self.view_base == 0 {
            "view base 0".to_string()
        } else {
            format!("view base 0x{:X}", self.view_base)
        }
    }

    pub fn view_label(&self) -> String {
        let base = if self.view_base == 0 {
            "0".to_string()
        } else {
            format!("0x{:X}", self.view_base)
        };
        if self.view_class.trim().is_empty() {
            format!("view @ {base}")
        } else {
            format!("view {} @ {base}", self.view_class)
        }
    }

    pub fn size_label(&self) -> String {
        if self.size <= 0 {
            "size 0".to_string()
        } else {
            format!("size 0x{:X}", self.size)
        }
    }

    pub fn compact_label(&self) -> String {
        if self.provider_kind == "None" {
            return "No source".to_string();
        }
        format!("{}: {}", self.provider_kind, self.target_label)
    }

    pub fn reattach_label(&self) -> &'static str {
        match self.reattach {
            ReattachHealth::None => "not saved",
            ReattachHealth::Ready => "reattach ready",
            ReattachHealth::MissingFile => "reattach missing file",
            ReattachHealth::MissingTarget => "reattach missing target",
        }
    }

    pub fn tooltip_text(&self) -> String {
        format!(
            "{}\nhealth: {}\naccess: {} · {} · {}\n{}\nsaved source: {}",
            self.compact_label(),
            self.health_label(),
            self.pointer_label(),
            self.access_label(),
            self.size_label(),
            self.view_label(),
            self.reattach_label()
        )
    }
}

fn view_class_name(tree: &NodeTree, view_root: u64) -> String {
    let idx = if view_root != 0 {
        tree.index_of_id(view_root)
    } else {
        tree.nodes
            .iter()
            .position(|n| n.parent_id == 0 && n.kind == NodeKind::Struct)
            .map(|i| i as i32)
            .unwrap_or(-1)
    };
    if idx < 0 {
        return String::new();
    }
    let node = &tree.nodes[idx as usize];
    if !node.struct_type_name.trim().is_empty() {
        node.struct_type_name.clone()
    } else {
        node.name.clone()
    }
}

fn reattach_health(entry: &SavedSourceEntry) -> ReattachHealth {
    if entry.kind == "File" {
        if entry.file_path.trim().is_empty() {
            ReattachHealth::MissingTarget
        } else if Path::new(&entry.file_path).exists() {
            ReattachHealth::Ready
        } else {
            ReattachHealth::MissingFile
        }
    } else if !entry.provider_target.trim().is_empty() || !entry.display_name.trim().is_empty() {
        ReattachHealth::Ready
    } else {
        ReattachHealth::MissingTarget
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{RcxController, RcxDocument};
    use crate::core::{Node, NodeKind};
    use crate::provider::{BufferProvider, NullProvider};

    #[test]
    fn null_provider_reports_no_source() {
        let p = NullProvider;
        let summary = TargetStatusSummary::from_provider(&p, None, None);
        assert_eq!(summary.health(), TargetHealth::NoSource);
        assert_eq!(summary.compact_label(), "No source");
        assert_eq!(summary.reattach, ReattachHealth::None);
    }

    #[test]
    fn buffer_provider_reports_static_writable_target() {
        let p = BufferProvider::new(vec![0; 16], "sample.bin");
        let summary = TargetStatusSummary::from_provider(&p, None, None);
        assert_eq!(summary.health(), TargetHealth::Static);
        assert_eq!(summary.provider_kind, "File");
        assert_eq!(summary.target_label, "sample.bin");
        assert!(summary.writable);
        assert_eq!(summary.pointer_label(), "64b");
        assert_eq!(summary.size_label(), "size 0x10");
    }

    #[test]
    fn live_read_failure_reports_stale_until_recovery() {
        let stale = TargetStatusSummary {
            provider_kind: "Process".into(),
            valid: true,
            live: true,
            last_read_ok: false,
            ..TargetStatusSummary::default()
        };
        assert_eq!(stale.health(), TargetHealth::Stale);
        assert_eq!(stale.health_label(), "stale");

        let recovered = TargetStatusSummary {
            last_read_ok: true,
            ..stale
        };
        assert_eq!(recovered.health(), TargetHealth::Live);
    }

    #[test]
    fn invalid_saved_target_reports_disconnected() {
        let summary = TargetStatusSummary {
            provider_kind: "Process".into(),
            valid: false,
            live: true,
            last_read_ok: true,
            ..TargetStatusSummary::default()
        };
        assert_eq!(summary.health(), TargetHealth::Disconnected);
        assert_eq!(summary.health_label(), "disconnected");
    }

    #[test]
    fn saved_process_target_is_reattach_ready() {
        let p = BufferProvider::new(vec![0; 4], "fallback");
        let saved = SavedSourceEntry {
            kind: "processmemory".to_string(),
            display_name: "game.exe".to_string(),
            provider_target: "pid:1234:game.exe".to_string(),
            ..SavedSourceEntry::default()
        };
        let summary = TargetStatusSummary::from_provider(&p, Some(&saved), None);
        assert_eq!(summary.provider_kind, "processmemory");
        assert_eq!(summary.target_label, "game.exe");
        assert_eq!(summary.reattach, ReattachHealth::Ready);
        assert_eq!(summary.saved_target, "pid:1234:game.exe");
    }

    #[test]
    fn controller_summary_includes_active_view_class_and_base() {
        let mut doc = RcxDocument::new();
        doc.tree.base_address = 0x1400_0000;
        let root_idx = doc.tree.add_node(Node {
            kind: NodeKind::Struct,
            struct_type_name: "Module_game_exe".into(),
            name: "base".into(),
            parent_id: 0,
            ..Node::default()
        });
        let root_id = doc.tree.nodes[root_idx].id;
        doc.provider = std::sync::Arc::new(BufferProvider::new(vec![0; 16], "game.exe"));
        let mut ctrl = RcxController::new(doc);
        ctrl.set_view_root_id(root_id);

        let summary = TargetStatusSummary::for_controller(&ctrl);
        assert_eq!(summary.view_class, "Module_game_exe");
        assert_eq!(summary.view_base, 0x1400_0000);
        assert_eq!(summary.view_label(), "view Module_game_exe @ 0x14000000");
    }
}
