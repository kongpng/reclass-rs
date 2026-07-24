//! The editor's popup-opener cluster — the type-selector, source-chooser, enum
//! picker, and hex-toolbar openers (plus their outcome plumbing) — extracted from
//! the oversized editor/mod.rs into a second `impl RcxEditor`. A child module of
//! `editor`, it keeps full access to RcxEditor's private fields and methods.

use super::*;
use crate::core::NodeKind;
use gpui::*;

/// Push a picked type `display_name` to the FRONT of `list`, removing any prior
/// occurrence (dedup-to-front) and capping the list at 8 entries — the C++
/// `RcxController::pushRecentType` (controller.cpp:4819). Empty names are ignored.
/// Free so it is unit-testable without a gpui view (item 3/11 recent-types list).
pub(crate) fn push_recent_type_into(list: &mut Vec<String>, display_name: &str) {
    if display_name.is_empty() {
        return;
    }
    list.retain(|n| n != display_name);
    list.insert(0, display_name.to_string());
    list.truncate(8);
}

/// The modifier the Type Selector should open pre-toggled with, so the footer
/// preview reads the field's *current* shape instead of the bare base type.
/// Mirrors the C++ `showTypePopup` preset (controller.cpp:4466-4474): only
/// `FieldType` mode carries modifiers; a primitive pointer lights `*`/`**` by
/// `ptr_depth`, a typed pointer (references a composite) always `*`, and an
/// array `[array_len]`.
pub(super) fn modifier_preset_for(
    mode: crate::ui::pickers::typeselectorpopup::TypePopupMode,
    kind: NodeKind,
    ptr_depth: i32,
    ref_id: u64,
    array_len: i32,
) -> Option<crate::ui::pickers::typeselectorpopup::Modifier> {
    use crate::ui::pickers::typeselectorpopup::{Modifier, TypePopupMode};
    if mode != TypePopupMode::FieldType {
        return None;
    }
    let is_ptr = matches!(kind, NodeKind::Pointer32 | NodeKind::Pointer64);
    if is_ptr && ptr_depth > 0 && ref_id == 0 {
        Some(if ptr_depth >= 2 {
            Modifier::PointerPointer
        } else {
            Modifier::Pointer
        })
    } else if is_ptr && ref_id != 0 {
        Some(Modifier::Pointer)
    } else if kind == NodeKind::Array {
        Some(Modifier::Array(array_len))
    } else {
        None
    }
}

impl super::RcxEditor {
    /// Open the [`TypeSelectorPopup`](crate::ui::pickers::typeselectorpopup::TypeSelectorPopup)
    /// over `target`'s current kind and subscribe to its outcome. On
    /// [`Chosen`](crate::ui::pickers::typeselectorpopup::TypeSelectorEvent::Chosen) apply the
    /// kind via `change_node_kind` then the chosen [`Modifier`]
    /// (pointer/array/etc.) via the matching controller ops + `apply_document`; on
    /// Cancel close the modal. Opened through the host's centered-modal overlay
    /// ([`RcxEditorEvent::OpenModal`]) — the same path window.rs uses for the
    /// command palette — NOT `window.open_dialog`, whose focus_trap broke Enter.
    pub(super) fn open_type_selector(
        &mut self,
        target: ContextTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Change-Type / `T` is the field-type flow (modifiers allowed).
        self.open_type_selector_in_mode(target, EditTarget::Type, window, cx);
    }

    /// Workspace "Change Type…" on a field: bring it into view (a child → view its
    /// parent struct), select it, then open the gutter Type Selector on it — the
    /// same picker as the editor's Change Type / `T`, driven from the project
    /// panel. The picker is a centered overlay, so it does not depend on the row's
    /// pixel position; the scroll + select just keep the edited row visible behind it.
    pub(crate) fn reveal_and_change_type(
        &mut self,
        node_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let idx0 = self.controller.tree().index_of_id(node_id);
        if idx0 < 0 {
            return;
        }
        let parent_id = self.controller.tree().nodes[idx0 as usize].parent_id;
        // A child field → view its parent struct so the row is on screen; a
        // top-level node is already its own view root.
        if parent_id != 0 {
            self.controller.set_view_root_id(parent_id);
        }
        self.recompose_view(cx);
        // Locate the field's row + re-resolved index in the recomposed view.
        let Some(line) = self
            .controller
            .last_result()
            .meta
            .iter()
            .position(|lm| lm.node_id == node_id && !lm.is_continuation)
        else {
            return;
        };
        let idx = self.controller.tree().index_of_id(node_id);
        if idx < 0 {
            return;
        }
        let kind = self.controller.tree().nodes[idx as usize].kind;
        self.controller
            .handle_node_click(line as i64, node_id, crate::controller::Modifiers::NONE);
        let _ = self.controller.take_events();
        self.caret_line = Some(line);
        self.scroll.scroll_to_item(line, ScrollStrategy::Center);
        let target = ContextTarget {
            line,
            node_idx: idx as usize,
            node_id,
            kind,
            sub_line: -1,
        };
        self.context_target = Some(target);
        self.open_type_selector(target, window, cx);
    }

    /// Replace the cross-document composite snapshot — the top-level structs from
    /// the OTHER open tabs — pushed by the window on every `rebuild_workspace` and
    /// consumed by [`full_type_entries`](Self::full_type_entries). No `cx.notify`:
    /// it only changes the NEXT Type Selector open, not the current frame.
    pub fn set_cross_doc_composites(&mut self, items: Vec<CrossDocComposite>) {
        if self.cross_doc_composites != items {
            self.cross_doc_composites = items;
        }
    }

    /// The full type catalogue: the built-in primitives + every user-declared
    /// composite (struct/class/enum) in the tree (item 6 — "composites + user
    /// structs"). Composites are appended after the primitives, mirroring the C++
    /// catalogue. `exclude_id` drops a struct from the list (so a struct cannot
    /// reference itself).
    ///
    /// In [`TypePopupMode::PointerTarget`] mode the synthetic **void** entry is
    /// prepended (item 43): a `Hex8`-backed primitive named "void" so a Ctrl+Click
    /// pointer-retarget can pick void (the C++ `PointerTarget` `voidEntry`,
    /// controller.cpp:4724). It applies as `refId = 0` via the PointerTarget branch
    /// of `apply_type_popup_result` (a primitive entry ⇒ refId 0).
    pub(super) fn full_type_entries(
        &self,
        exclude_id: u64,
        mode: crate::ui::pickers::typeselectorpopup::TypePopupMode,
    ) -> Vec<crate::ui::pickers::typeselectorpopup::TypeEntry> {
        use crate::ui::pickers::typeselectorpopup::{
            default_type_entries, TypeEntry, TypePopupMode,
        };
        let mut entries: Vec<TypeEntry> = Vec::new();
        if mode == TypePopupMode::PointerTarget {
            // Synthetic "void" target — a Hex8-backed primitive applied as refId 0.
            let mut void = TypeEntry::primitive(NodeKind::Hex8, "void");
            void.enabled = true;
            entries.push(void);
        }
        entries.extend(default_type_entries());
        let tree = self.controller.tree();
        let mut composites: Vec<TypeEntry> = Vec::new();
        for n in tree.nodes.iter() {
            // Named composite declarations (a struct with a type name), excluding
            // the self-reference target.
            // `parent_id == 0`: only TOP-LEVEL declarations belong in the catalogue
            // (the C++ `addComposites` guard, controller.cpp:4605). Embedded struct
            // INSTANCES also carry a struct_type_name but live under a parent and
            // would otherwise leak in with a non-canonical (instance) struct_id.
            if n.kind == NodeKind::Struct
                && n.parent_id == 0
                && !n.struct_type_name.is_empty()
                && n.id != exclude_id
            {
                // Composite size is the struct's actual byte extent (sum/extent of
                // its children) — matching C++ `e.sizeBytes = structSpan(n.id)`
                // (controller.cpp:4555) which feeds the popup size bar/preview
                // (typeselectorpopup.cpp:1515-1557), *not* the flat `size_for_kind`.
                let size = tree.struct_span(n.id).max(0);
                let keyword = if n.class_keyword.is_empty() {
                    "struct"
                } else {
                    n.class_keyword.as_str()
                };
                composites.push(TypeEntry::composite(
                    n.id,
                    &n.struct_type_name,
                    keyword,
                    size,
                ));
            }
        }
        // Dedup composites by type name (the same struct can appear via several
        // pointer refs); keep the first occurrence.
        composites.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        composites.dedup_by(|a, b| a.display_name == b.display_name);
        entries.extend(composites);
        // Cross-document composites: top-level structs from the OTHER open tabs,
        // pushed by the window (the C++ `m_projectDocs` block, controller.cpp:4753-
        // 4774). `struct_id == 0` ⇒ the apply path imports the type by NAME. Deduped
        // by name against everything already present — a local struct (or primitive)
        // of the same name wins — and runs in every mode, matching C++ (no mode gate).
        if !self.cross_doc_composites.is_empty() {
            let mut seen: std::collections::HashSet<String> =
                entries.iter().map(|e| e.display_name.to_string()).collect();
            for c in self.cross_doc_composites.iter() {
                if !seen.insert(c.name.clone()) {
                    continue;
                }
                entries.push(TypeEntry::composite(0, &c.name, &c.keyword, c.size));
            }
        }
        // Built-in Common Types catalogue (~47 entries: UNICODE_STRING, std::string,
        // FVector, GUID, Matrix4x4, ...) — the C++ `addComposites` appends these
        // after the project's own composites (controller.cpp:4778). `struct_id == 0`
        // makes the apply path import-by-name (find_or_create_struct_by_name).
        // Excluded from Root mode (cannot make the viewed struct a built-in); deduped
        // by name so a project struct of the same name wins.
        if mode != TypePopupMode::Root {
            use crate::ui::pickers::typeselectorpopup::KindGroup;
            let seen: std::collections::HashSet<String> =
                entries.iter().map(|e| e.display_name.to_string()).collect();
            for ct in crate::core::K_COMMON_TYPES.iter() {
                if seen.contains(ct.name) {
                    continue;
                }
                let mut e = TypeEntry::composite(0, ct.name, ct.class_keyword, ct.total_size);
                e.group = KindGroup::Common;
                entries.push(e);
            }
        }
        entries
    }

    /// Open the Type Selector for `target` in the mode implied by `edit_target`
    /// (item 6): `Type` → FieldType (modifiers), `ArrayElementType` → ArrayElement
    /// (modifiers), `PointerTarget` → PointerTarget (no modifiers). The catalogue
    /// includes composites + primitives.
    pub(super) fn open_type_selector_in_mode(
        &mut self,
        target: ContextTarget,
        edit_target: EditTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::pickers::typeselectorpopup::TypePopupMode;
        let mode = match edit_target {
            EditTarget::ArrayElementType => TypePopupMode::ArrayElement,
            EditTarget::PointerTarget => TypePopupMode::PointerTarget,
            _ => TypePopupMode::FieldType,
        };
        let entries = self.full_type_entries(target.node_id, mode);
        self.spawn_type_selector(entries, target, mode, window, cx);
    }

    /// Open the Root-mode Type Selector (item 2): the class-header chevron switches
    /// the *viewed* struct. We list every declared composite (Root mode hides the
    /// `*`/`[]` modifiers); on Chosen the kind is applied to the root node via
    /// [`apply_type_choice`].
    pub(super) fn open_root_type_selector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::pickers::typeselectorpopup::TypePopupMode;
        let root_id = self.controller.view_root_id();
        let root_idx = self.controller.tree().index_of_id(root_id);
        let kind = if root_idx >= 0 {
            self.controller.tree().nodes[root_idx as usize].kind
        } else {
            NodeKind::Struct
        };
        let target = ContextTarget {
            line: 0,
            node_idx: root_idx.max(0) as usize,
            node_id: root_id,
            kind,
            sub_line: 0,
        };
        // Root mode lists every declared composite so the user can re-root onto a
        // different struct (do NOT exclude the current root — it may be re-picked).
        let entries = self.full_type_entries(0, TypePopupMode::Root);
        self.spawn_type_selector(entries, target, TypePopupMode::Root, window, cx);
    }

    /// Shared opener: build the [`TypeSelectorPopup`] over `entries`, set `mode`,
    /// subscribe to its outcome (apply via [`apply_type_choice`]), and float it in
    /// the host's centered-modal overlay via [`RcxEditorEvent::OpenModal`].
    fn spawn_type_selector(
        &mut self,
        entries: Vec<crate::ui::pickers::typeselectorpopup::TypeEntry>,
        target: ContextTarget,
        mode: crate::ui::pickers::typeselectorpopup::TypePopupMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::pickers::typeselectorpopup::{
            TypePopupMode, TypeSelectorEvent, TypeSelectorPopup,
        };
        // The popup opens pre-highlighting the node's ACTUAL current type (the C++
        // `setTypes(.., &currentEntry)`): for a composite that means the referenced
        // struct id (pre-select by structId), for a primitive the kind. Also compute
        // the C++ footer-size baseline (`nodeSize = sizeForKind(node.kind)`, or the
        // ELEMENT kind in ArrayElement mode) + the tree's pointer size, and feed the
        // recent-type names so the "Recent" section appears (items 38/39/40/41).
        let (node_size, ptr_size, cur_struct_id, preset, cur_relative) = {
            let tree = self.controller.tree();
            let ps = tree.pointer_size;
            let idx = tree.index_of_id(target.node_id);
            if idx >= 0 {
                let n = &tree.nodes[idx as usize];
                let sz = if mode == TypePopupMode::ArrayElement {
                    crate::core::size_for_kind(n.element_kind)
                } else {
                    crate::core::size_for_kind(n.kind)
                };
                // The node already references a composite when its `ref_id` is set
                // (typed pointer / embedded struct / array-of-struct). Also derive
                // the modifier preset so the selector opens lit for the node's shape.
                let preset = modifier_preset_for(mode, n.kind, n.ptr_depth, n.ref_id, n.array_len);
                (sz, ps, n.ref_id, preset, n.is_relative)
            } else {
                (
                    crate::core::size_for_kind(target.kind),
                    ps,
                    0u64,
                    None,
                    false,
                )
            }
        };
        let recent = self.recent_type_names.clone();
        let popup = cx.new(|cx| {
            let mut p = TypeSelectorPopup::new_with_current(entries, target.kind, window, cx);
            p.set_mode(mode, window, cx);
            p.set_sizes(node_size, ptr_size);
            p.set_recent_names(recent, cx);
            // For a pointer node, pre-select the catalogue variant matching the
            // node's RVA flag so opening on an existing "(RVA)" pointer pins the
            // "(RVA)" row (the C++ `setTypes` pointer-isRelative match).
            p.set_current_relative(cur_relative, cx);
            // Pre-highlight the composite the node already references, by structId
            // (the C++ `m_currentEntry.entryKind == Composite` branch). For a plain
            // primitive node this is 0 and the kind pre-select (in new_with_current)
            // stands.
            if cur_struct_id != 0 {
                p.set_current_struct(cur_struct_id, cx);
            }
            // Pre-toggle the modifier matching the field's current shape (C++
            // showTypePopup preModId/preArrayCount) — applied LAST, since set_mode
            // clears any modifier. A typed/primitive pointer opens with `*`/`**`
            // lit, an array with `[len]`, so the footer reads the node's real type.
            if let Some(m) = preset {
                p.set_modifier(m, cx);
            }
            p
        });
        let focus = popup.read(cx).focus_handle(cx);
        let node_id = target.node_id;
        self._type_selector_sub = Some(cx.subscribe_in(
            &popup,
            window,
            move |this, _p, ev: &TypeSelectorEvent, _window, cx| match ev {
                TypeSelectorEvent::Chosen {
                    kind,
                    modifier,
                    create_new,
                    entry_kind,
                    struct_id,
                    display_name,
                    is_relative,
                } => {
                    cx.emit(RcxEditorEvent::CloseModal);
                    this._type_selector_sub = None;
                    if *create_new {
                        // "+ New": create a populated NewClass[_N] (8×Hex64) and
                        // embed THIS node as an instance of it (same as the editor
                        // "New Class" action) — not a bare empty struct.
                        this.controller.new_class_on_node(node_id);
                        this.apply_document(cx);
                    } else {
                        this.apply_type_choice(
                            mode,
                            node_id,
                            *kind,
                            *modifier,
                            *entry_kind,
                            *struct_id,
                            display_name,
                            *is_relative,
                            cx,
                        );
                    }
                }
                TypeSelectorEvent::Cancel => {
                    cx.emit(RcxEditorEvent::CloseModal);
                    this._type_selector_sub = None;
                }
            },
        ));
        // Open the picker in the HOST's centered-modal overlay (no gpui-component
        // `Dialog` focus_trap, so Enter applies the picked type + arrows navigate —
        // the #1 fix). The host renders + focuses the popup's own input handle; the
        // outcome subscription above stays on this editor.
        cx.emit(RcxEditorEvent::OpenModal {
            view: popup.into(),
            focus,
            // The type selector sets its own `w(380)`, so no card width is needed.
            width: None,
        });
        cx.notify();
    }

    /// Apply a TypeSelector choice by routing the WHOLE pick through the
    /// controller's `apply_type_popup_result` with a faithful `TypePopupChoice`
    /// (items 35/36/37). This is the single C++-parity apply path: it carries the
    /// composite identity (`struct_id` / `display_name`) so selecting an EXISTING
    /// composite references it by id (not a bare empty Struct); routes a primitive
    /// `*` through `is_valid_primitive_ptr_target` (element_kind + ptr_depth, NOT
    /// `convert_to_typed_pointer`); and keeps the `refId` for a composite `[]`
    /// array. The optional `modifier` (`*`/`**`/`[N]`) becomes the `full_text`
    /// suffix that `apply_type_popup_result` parses into a `TypeSpec`.
    #[allow(clippy::too_many_arguments)]
    fn apply_type_choice(
        &mut self,
        mode: crate::ui::pickers::typeselectorpopup::TypePopupMode,
        node_id: u64,
        kind: NodeKind,
        modifier: Option<crate::ui::pickers::typeselectorpopup::Modifier>,
        entry_kind: crate::ui::pickers::typeselectorpopup::EntryKind,
        struct_id: u64,
        display_name: &str,
        is_relative: bool,
        cx: &mut Context<Self>,
    ) {
        use crate::controller::{TypeEntryKind, TypePopupChoice, TypePopupMode as CMode};
        use crate::ui::pickers::typeselectorpopup::{EntryKind, Modifier, TypePopupMode};

        // Map the popup's mode → the controller's mode (same four cases).
        let cmode = match mode {
            TypePopupMode::Root => CMode::Root,
            TypePopupMode::FieldType => CMode::FieldType,
            TypePopupMode::ArrayElement => CMode::ArrayElement,
            TypePopupMode::PointerTarget => CMode::PointerTarget,
        };

        // The base display name: a primitive uses its canonical kind name, a
        // composite the struct/enum display name carried from the popup row.
        let base_name = if entry_kind == EntryKind::Composite && !display_name.is_empty() {
            display_name.to_string()
        } else {
            crate::core::kind_to_string(kind).to_string()
        };
        // The modifier suffix (`*` / `**` / `[N]`) → the `full_text` the controller
        // parses (`acceptCurrent` `fullText`); empty modifier ⇒ derive from name.
        let full_text = match modifier {
            Some(m @ (Modifier::Pointer | Modifier::PointerPointer | Modifier::Array(_))) => {
                format!("{}{}", base_name, m.suffix())
            }
            Some(Modifier::None) | None => String::new(),
        };

        let choice = TypePopupChoice {
            entry_kind: if entry_kind == EntryKind::Composite {
                TypeEntryKind::Composite
            } else {
                TypeEntryKind::Primitive
            },
            primitive_kind: kind,
            struct_id,
            display_name: base_name.clone(),
            full_text,
            create_new: false,
            // Carry the "(RVA)" pick so the controller toggles node.is_relative
            // for a pointer kind (apply_field_type Primitive branch).
            is_relative,
        };

        // Record the pick in the recent-types list (the C++ `pushRecentType` on
        // apply) so a subsequent open surfaces it in the "Recent" section.
        self.push_recent_type(&base_name);

        // Multi-selection Change-Type (`t` over a highlighted RANGE): apply the SAME
        // pick to EVERY selected node, not just the one the cursor is over. Only
        // FieldType batches — Root re-roots a single view, and Array/PointerTarget
        // are single-node contextual edits. With no (or a single) selection this is
        // the plain single-node apply on `node_id`.
        let batch_ids: Vec<u64> =
            if cmode == CMode::FieldType && self.controller.selected_ids().len() > 1 {
                self.selected_node_indices_ordered()
                    .iter()
                    .filter_map(|&idx| self.controller.tree().nodes.get(idx).map(|n| n.id))
                    .collect()
            } else {
                Vec::new()
            };
        if batch_ids.len() > 1 {
            self.controller
                .apply_type_popup_result_batch(cmode, &batch_ids, choice);
        } else {
            self.controller
                .apply_type_popup_result(cmode, node_id, choice);
        }
        self.apply_document(cx);
    }

    /// Push a picked type `display_name` to the front of the recent-types list,
    /// dedup-to-front and capped at 8 (the C++ `RcxController::pushRecentType`,
    /// controller.cpp:4819). Empty names are ignored.
    fn push_recent_type(&mut self, display_name: &str) {
        push_recent_type_into(&mut self.recent_type_names, display_name);
    }

    // ── Data-source picker (SourceChooserPopup; items 1/5, contract CONSUMES) ──

    /// Open the [`SourceChooserPopup`] over the controller's saved sources +
    /// providers (the class-header `source▾` chip click, item 1). Subscribes to the
    /// [`SourceChooserEvent`] and applies the pick through the controller's
    /// data-source API: `SourceSelected` → `switch_to_saved_source`,
    /// `ClearRequested` → `clear_sources`. Provider selection needs the app shell's
    /// file/attach dialogs (out of the editor's ownership), so it closes cleanly
    /// (the documented stub the controller itself uses for plugin sources). Opened
    /// in the host's centered-modal overlay ([`RcxEditorEvent::OpenModal`]),
    /// mirroring the type selector + command palette.
    pub(super) fn open_source_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Build the `(name, kind_label, active)` recent tuples from the controller's
        // saved sources, flagging the active one with its checkmark (PIC4).
        let active = self.controller.active_source_index();
        let recent: Vec<(String, String, bool)> = self
            .controller
            .saved_sources()
            .iter()
            .enumerate()
            .map(|(i, s)| (s.display_name.clone(), s.kind.clone(), i as i32 == active))
            .collect();
        let popup = SourceChooserPopup::view(recent, window, cx);
        let focus = popup.read(cx).focus_handle(cx);
        self._source_chooser_sub = Some(cx.subscribe_in(
            &popup,
            window,
            move |this, _p, ev: &SourceChooserEvent, _window, cx| {
                use crate::ui::pickers::sourcechooser::SourcePick;
                if let SourceChooserEvent::RemoveSaved(idx) = ev {
                    this.controller.remove_saved_source(*idx);
                    this.after_mutation(cx);
                    return;
                }
                cx.emit(RcxEditorEvent::CloseModal);
                this._source_chooser_sub = None;
                match ev {
                    SourceChooserEvent::Pick(SourcePick::SavedSource(idx)) => {
                        this.controller.switch_to_saved_source(*idx);
                        this.after_mutation(cx);
                    }
                    SourceChooserEvent::Clear => {
                        this.controller.clear_sources();
                        this.after_mutation(cx);
                    }
                    // Provider activation (Open File / Kernel / Process / …) drives
                    // the app shell's file/attach dialogs, which the editor does not
                    // own; close cleanly (documented stub, like plugin sources).
                    SourceChooserEvent::Pick(SourcePick::Provider(_))
                    | SourceChooserEvent::OpenFile
                    | SourceChooserEvent::Cancel => {
                        cx.notify();
                    }
                    SourceChooserEvent::RemoveSaved(_) => unreachable!(),
                }
            },
        ));
        // Open in the host's centered-modal overlay (no dialog focus_trap, so the
        // chooser's own key handling works). It sets only a `min_w`, so carry the
        // 520px card width the dialog used to provide — wide enough for the longest
        // "Name  (libXxxPlugin.dll)" row so the dll hints + footer don't clip.
        cx.emit(RcxEditorEvent::OpenModal {
            view: popup.into(),
            focus,
            width: Some(px(520.)),
        });
        cx.notify();
    }

    // ── Enum-value picker (EnumPickerPopup; item 8) ──

    /// Whether node `idx` is an enum (its resolved class keyword is `enum`).
    pub(super) fn node_is_enum(&self, idx: usize) -> bool {
        self.controller
            .tree()
            .nodes
            .get(idx)
            .is_some_and(|n| n.is_enum())
    }

    /// Open the [`EnumPickerPopup`] for the enum field at `idx` (item 8). Builds the
    /// member list from the node's `enum_members`, pre-selecting the current value,
    /// and on Chosen writes the value back through `set_node_value`.
    pub(super) fn open_enum_picker(
        &mut self,
        line: usize,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::pickers::enumpicker::{EnumPickerEvent, Member};
        let (enum_name, members, current) = {
            let n = &self.controller.tree().nodes[idx];
            let members: Vec<Member> = n
                .enum_members
                .iter()
                .map(|(name, value)| Member::new(name, *value))
                .collect();
            // The current value: read it through the value-history snapshot if any,
            // else default to 0 (the picker pre-selects the nearest member).
            (n.struct_type_name.clone(), members, 0i64)
        };
        if members.is_empty() {
            // No members → fall back to a plain inline value edit.
            self.begin_inline_edit(line, EditTarget::Value, window, cx);
            return;
        }
        let resolved_addr = self.line_meta(line).map(|lm| lm.offset_addr).unwrap_or(0);
        let sub_line = self.line_meta(line).map(|lm| lm.sub_line).unwrap_or(0);
        let popup = cx.new(|cx| {
            crate::ui::pickers::enumpicker::EnumPickerPopup::new(
                &enum_name, members, current, window, cx,
            )
        });
        let focus = popup.read(cx).focus_handle(cx);
        self._enum_picker_sub = Some(cx.subscribe_in(
            &popup,
            window,
            move |this, _p, ev: &EnumPickerEvent, _window, cx| match ev {
                EnumPickerEvent::Chosen(value) => {
                    cx.emit(RcxEditorEvent::CloseModal);
                    this._enum_picker_sub = None;
                    this.controller.set_node_value(
                        idx,
                        sub_line,
                        &value.to_string(),
                        false,
                        resolved_addr,
                    );
                    this.after_mutation(cx);
                }
                EnumPickerEvent::Dismissed => {
                    cx.emit(RcxEditorEvent::CloseModal);
                    this._enum_picker_sub = None;
                }
            },
        ));
        // Open in the host's centered-modal overlay (no dialog focus_trap, so
        // Up/Down/Enter drive the member list). The picker sets only a `min_w`, so
        // carry the 360px card width the dialog used to provide.
        cx.emit(RcxEditorEvent::OpenModal {
            view: popup.into(),
            focus,
            width: Some(px(360.)),
        });
        cx.notify();
    }

    // ── Hex size toolbar (HexToolbarPopup; item 9) ──

    /// Build the [`HexPopupContext`] for the hex node at `idx`: its current kind +
    /// raw bytes + up to 15 adjacent same-parent hex nodes (for join previews).
    fn build_hex_context(
        &self,
        idx: usize,
    ) -> Option<crate::ui::overlays::hextoolbar::HexPopupContext> {
        let selected_ids: Vec<u64> = self.controller.selected_ids().iter().copied().collect();
        crate::ui::overlays::hextoolbar::build_hex_popup_context(
            self.controller.tree(),
            self.controller.document().provider.as_ref(),
            idx,
            &selected_ids,
        )
    }

    /// Open the [`HexToolbarPopup`] for the hex node at `idx` (item 9). On
    /// `SizeSelected` apply the size change via `split_hex_node` (smaller) or
    /// `join_hex_nodes` (larger); Insert above/below + dismiss route accordingly.
    pub(super) fn open_hex_toolbar(
        &mut self,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::overlays::hextoolbar::{HexToolbarEvent, HexToolbarPopup};
        let Some(ctx) = self.build_hex_context(idx) else {
            return;
        };
        let node_id = ctx.node_id;
        let popup = cx.new(|cx| HexToolbarPopup::new(ctx, window, cx));
        let focus = popup.read(cx).focus_handle(cx);
        self._hex_toolbar_sub = Some(cx.subscribe_in(
            &popup,
            window,
            move |this, _p, ev: &HexToolbarEvent, _window, cx| match ev {
                HexToolbarEvent::SizeSelected(id, kind)
                | HexToolbarEvent::SuggestKind(id, kind) => {
                    cx.emit(RcxEditorEvent::CloseModal);
                    this._hex_toolbar_sub = None;
                    this.apply_hex_size(*id, *kind, cx);
                }
                HexToolbarEvent::InsertAbove(id) => {
                    cx.emit(RcxEditorEvent::CloseModal);
                    this._hex_toolbar_sub = None;
                    let i = this.controller.tree().index_of_id(*id);
                    if i >= 0 {
                        this.controller
                            .insert_node_above(i as usize, NodeKind::Hex64, "");
                        this.apply_document(cx);
                    }
                }
                HexToolbarEvent::InsertBelow(id) => {
                    cx.emit(RcxEditorEvent::CloseModal);
                    this._hex_toolbar_sub = None;
                    let i = this.controller.tree().index_of_id(*id);
                    if i >= 0 {
                        let (parent_id, offset) = this.insert_anchor(i as usize);
                        this.controller
                            .insert_node(parent_id, offset, NodeKind::Hex64, "");
                        this.apply_document(cx);
                    }
                }
                HexToolbarEvent::JoinSelected => {
                    // Re-validate the current selected run at activation time. The
                    // join must consume exactly the selected same-kind/same-parent
                    // contiguous rows, never an arbitrary following hex run.
                    cx.emit(RcxEditorEvent::CloseModal);
                    this._hex_toolbar_sub = None;
                    this.join_selected_hex_run(node_id, cx);
                }
                HexToolbarEvent::FillToOffset(id, offset) => {
                    // Item 11: actually FILL the gap from the node's end up to the
                    // typed offset with padding hex nodes (was a close-only no-op).
                    cx.emit(RcxEditorEvent::CloseModal);
                    this._hex_toolbar_sub = None;
                    this.fill_to_offset(*id, *offset, cx);
                }
                HexToolbarEvent::Dismissed => {
                    cx.emit(RcxEditorEvent::CloseModal);
                    this._hex_toolbar_sub = None;
                    cx.notify();
                }
            },
        ));
        // Open in the host's centered-modal overlay (no dialog focus_trap). The
        // hex toolbar sets only a `min_w`, so carry the 320px card width the dialog
        // used to provide.
        cx.emit(RcxEditorEvent::OpenModal {
            view: popup.into(),
            focus,
            width: Some(px(320.)),
        });
        cx.notify();
    }

    /// Join exactly the currently selected, validated hex run. The toolbar anchor
    /// must still be a member of the selection; a stale popup or changed selection
    /// therefore becomes a no-op instead of mutating unrelated rows.
    fn join_selected_hex_run(&mut self, anchor_id: u64, cx: &mut Context<Self>) {
        let selected_ids: Vec<u64> = self.controller.selected_ids().iter().copied().collect();
        let run = crate::ui::overlays::hextoolbar::selected_hex_run(
            self.controller.tree(),
            &selected_ids,
        );
        if !run.node_ids.contains(&anchor_id) {
            return;
        }
        let Some(target) = run.join_kind() else {
            return;
        };
        let Some(&first_id) = run.node_ids.first() else {
            return;
        };
        self.controller.join_hex_nodes(first_id, target);
        self.apply_document(cx);
    }

    /// Item 11: fill the gap from the hex node's end offset up to `target_offset`
    /// with padding hex nodes. Resolves the node, computes `cur_end = offset + size`,
    /// then inserts `Hex64`/`Hex8` fields (hex64 chunks then an 8-byte tail split as
    /// needed) into the parent at increasing offsets until the gap is covered. A
    /// non-positive gap is a no-op.
    fn fill_to_offset(&mut self, node_id: u64, target_offset: u64, cx: &mut Context<Self>) {
        let (parent_id, mut cur) = {
            let tree = self.controller.tree();
            let idx = tree.index_of_id(node_id);
            if idx < 0 {
                return;
            }
            let n = &tree.nodes[idx as usize];
            let size = crate::core::size_for_kind(n.kind).max(0) as u64;
            (n.parent_id, n.offset as u64 + size)
        };
        if target_offset <= cur || parent_id == 0 {
            return;
        }
        // Insert padding hex nodes covering [cur, target_offset). Prefer 8-byte
        // Hex64 chunks; the remainder is filled with Hex8 single bytes. Guard the
        // loop count so a pathological gap can't spin forever.
        let mut guard = 0;
        while cur < target_offset && guard < 4096 {
            let remaining = target_offset - cur;
            let (kind, step) = if remaining >= 8 {
                (NodeKind::Hex64, 8u64)
            } else {
                (NodeKind::Hex8, 1u64)
            };
            self.controller.insert_node(parent_id, cur as i32, kind, "");
            cur += step;
            guard += 1;
        }
        self.apply_document(cx);
    }

    /// Apply a hex-toolbar size choice (item 9): a smaller target splits the node,
    /// a larger target joins it with the following same-kind siblings, same size is
    /// a no-op. Routes to the controller's `split_hex_node` / `join_hex_nodes`.
    fn apply_hex_size(&mut self, node_id: u64, target: NodeKind, cx: &mut Context<Self>) {
        let idx = self.controller.tree().index_of_id(node_id);
        if idx < 0 {
            return;
        }
        let cur = self.controller.tree().nodes[idx as usize].kind;
        let cur_sz = crate::core::size_for_kind(cur);
        let tgt_sz = crate::core::size_for_kind(target);
        if tgt_sz == cur_sz {
            // Same size (e.g. a suggested non-hex kind): change kind directly.
            if target != cur {
                self.controller.change_node_kind(idx as usize, target);
            }
        } else if tgt_sz < cur_sz {
            self.controller.split_hex_node(node_id);
            // After a split the node becomes the next smaller hex; if the target is
            // smaller still, the user can split again from the refreshed toolbar.
        } else {
            self.controller.join_hex_nodes(node_id, target);
        }
        self.apply_document(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::modifier_preset_for;
    use crate::core::NodeKind;
    use crate::ui::pickers::typeselectorpopup::{Modifier, TypePopupMode};

    // P4 / C++ parity (controller.cpp:4466-4474): opening the Type Selector on an
    // existing field pre-toggles the modifier matching the field's current shape.
    #[test]
    fn modifier_preset_mirrors_cpp_show_type_popup() {
        // Single primitive pointer (ptr_depth 1, no ref) → `*`.
        assert_eq!(
            modifier_preset_for(TypePopupMode::FieldType, NodeKind::Pointer64, 1, 0, 0),
            Some(Modifier::Pointer)
        );
        // Double primitive pointer (ptr_depth ≥ 2, no ref) → `**`.
        assert_eq!(
            modifier_preset_for(TypePopupMode::FieldType, NodeKind::Pointer64, 2, 0, 0),
            Some(Modifier::PointerPointer)
        );
        // Typed pointer (references a composite) is always a single `*`, even at
        // depth ≥ 2 — the C++ isTypedPtr branch wins before the depth check.
        assert_eq!(
            modifier_preset_for(TypePopupMode::FieldType, NodeKind::Pointer64, 2, 42, 0),
            Some(Modifier::Pointer)
        );
        assert_eq!(
            modifier_preset_for(TypePopupMode::FieldType, NodeKind::Pointer32, 1, 7, 0),
            Some(Modifier::Pointer)
        );
        // Array carries its element count.
        assert_eq!(
            modifier_preset_for(TypePopupMode::FieldType, NodeKind::Array, 0, 0, 16),
            Some(Modifier::Array(16))
        );
        // A plain primitive field gets no modifier.
        assert_eq!(
            modifier_preset_for(TypePopupMode::FieldType, NodeKind::Int32, 0, 0, 0),
            None
        );
    }

    #[test]
    fn modifier_preset_only_in_field_type_mode() {
        // ArrayElement / PointerTarget / Root modes hide the modifiers, so even a
        // pointer node yields no preset (C++ guards the whole block on FieldType).
        for mode in [
            TypePopupMode::ArrayElement,
            TypePopupMode::PointerTarget,
            TypePopupMode::Root,
        ] {
            assert_eq!(
                modifier_preset_for(mode, NodeKind::Pointer64, 2, 42, 0),
                None,
                "mode {mode:?} must not preset a modifier"
            );
            assert_eq!(
                modifier_preset_for(mode, NodeKind::Array, 0, 0, 8),
                None,
                "mode {mode:?} must not preset a modifier"
            );
        }
    }
}
