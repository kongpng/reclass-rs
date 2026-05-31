//! Hex-toolbar popup — pick a hex size / split / join / fill, attached to a hex
//! node (`hextoolbarpopup.{h,cpp}`, widgets-dialogs.md §12).
//!
//! Port of `HexToolbarPopup`: a custom-painted popover with a row of size buttons
//! (8/16/32/64/128) + a pin toggle, a live byte preview of the result, and (when
//! pinned) smart-suggestion / insert / join / fill-to-offset rows. Per the
//! cookbook (ARCHITECTURE §5) the popover shell is gpui-component and the painted
//! internals are custom; this module ports the **pure preview/info/join
//! algorithms** (unit-tested verbatim) + a popover view emitting the same signals.
//!
//! The byte-preview formatting (`hexLine`), the joinable-byte computation
//! (`maxJoinableBytes`), and the per-target preview/info text (`previewForKind` /
//! `infoForKind`) are faithful ports — exact strings matter for parity.
//!
//! Gated behind the `ui` feature for the view; the algorithms are always built.

use crate::core::kind::{is_hex_node, size_for_kind, NodeKind};

/// `kHexSizes` (`hextoolbarpopup.cpp:14`) — the selectable hex sizes.
pub const HEX_SIZES: [NodeKind; 5] = [
    NodeKind::Hex8,
    NodeKind::Hex16,
    NodeKind::Hex32,
    NodeKind::Hex64,
    NodeKind::Hex128,
];

/// `kSizeLabels` (`hextoolbarpopup.cpp:15`) — the size button labels.
pub const HEX_SIZE_LABELS: [&str; 5] = ["8", "16", "32", "64", "128"];

/// The type-name of a hex kind for the preview line (`kindMeta(target)->typeName`).
fn type_name_for(k: NodeKind) -> &'static str {
    crate::core::kind::kind_meta(k)
        .map(|m| m.type_name)
        .unwrap_or("hex8")
}

/// An adjacent same-parent hex node (`HexPopupContext::Adjacent`,
/// `hextoolbarpopup.h`) — used to preview joins.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Adjacent {
    pub exists: bool,
    pub kind: NodeKind,
    pub data: Vec<u8>,
}

/// `struct HexPopupContext` (`hextoolbarpopup.h:11`) — everything the popup needs
/// to render previews + smart suggestions for the hovered hex node.
///
/// Not `Eq` because `float_val` is an `f64` (the smart-suggestion float preview).
#[derive(Clone, PartialEq, Debug)]
pub struct HexPopupContext {
    pub node_id: u64,
    pub current_kind: NodeKind,
    /// The node's raw bytes.
    pub data: Vec<u8>,
    /// Up to 15 adjacent same-parent hex nodes (for join previews).
    pub nexts: Vec<Adjacent>,
    // Smart suggestions.
    pub has_ptr: bool,
    pub ptr_symbol: String,
    pub has_float: bool,
    pub float_val: f64,
    pub has_string: bool,
    pub string_preview: String,
    // Multi-select.
    pub multi_select_count: i32,
    pub multi_select_bytes: i32,
    pub multi_select_contiguous: bool,
    pub multi_select_kind: NodeKind,
}

impl Default for HexPopupContext {
    fn default() -> Self {
        HexPopupContext {
            node_id: 0,
            current_kind: NodeKind::Hex8,
            data: Vec::new(),
            nexts: Vec::new(),
            has_ptr: false,
            ptr_symbol: String::new(),
            has_float: false,
            float_val: 0.0,
            has_string: false,
            string_preview: String::new(),
            multi_select_count: 0,
            multi_select_bytes: 0,
            multi_select_contiguous: false,
            multi_select_kind: NodeKind::Hex8,
        }
    }
}

impl HexPopupContext {
    /// `maxJoinableBytes()` (`hextoolbarpopup.cpp:86`): current kind size + the
    /// run of contiguous following hex nodes of the **same kind**, stopping at a
    /// gap/mismatch or once the total reaches 16.
    pub fn max_joinable_bytes(&self) -> i32 {
        let mut total = size_for_kind(self.current_kind);
        for adj in &self.nexts {
            if !adj.exists || !is_hex_node(adj.kind) || adj.kind != self.current_kind {
                break;
            }
            total += size_for_kind(adj.kind);
            if total >= 16 {
                break;
            }
        }
        total
    }

    /// Whether converting to `target` is doable: `target ≤ current size` (split)
    /// OR `target ≤ joinable` (join) — the C++ `canDo` predicate for a size button.
    pub fn can_do(&self, target: NodeKind) -> bool {
        let tgt = size_for_kind(target);
        let cur = size_for_kind(self.current_kind);
        tgt <= cur || tgt <= self.max_joinable_bytes()
    }

    /// `previewForKind(target)` (`hextoolbarpopup.cpp:112`): a byte-preview of the
    /// result of converting to `target` — same size → one line; smaller → split
    /// into N lines; larger → merge with adjacent same-kind nodes, pad with `\0`.
    pub fn preview_for_kind(&self, target: NodeKind) -> String {
        let cur_sz = size_for_kind(self.current_kind) as usize;
        let tgt_sz = size_for_kind(target) as usize;
        let tgt_name = type_name_for(target);

        if tgt_sz == cur_sz {
            return hex_line(tgt_name, &self.data);
        }

        if tgt_sz < cur_sz {
            let count = if tgt_sz == 0 { 0 } else { cur_sz / tgt_sz };
            let mut lines = Vec::new();
            for i in 0..count {
                let start = i * tgt_sz;
                let end = (start + tgt_sz).min(self.data.len());
                let slice = if start < self.data.len() {
                    &self.data[start..end]
                } else {
                    &[]
                };
                lines.push(hex_line(tgt_name, slice));
            }
            return lines.join("\n");
        }

        // Larger: merge data + adjacent same-kind, truncate/pad to tgt_sz.
        let mut merged = self.data.clone();
        for adj in &self.nexts {
            if !adj.exists || adj.kind != self.current_kind {
                break;
            }
            merged.extend_from_slice(&adj.data);
            if merged.len() >= tgt_sz {
                break;
            }
        }
        merged.truncate(tgt_sz);
        if merged.len() < tgt_sz {
            merged.resize(tgt_sz, 0);
        }
        hex_line(tgt_name, &merged)
    }

    /// `infoForKind(target)` (`hextoolbarpopup.cpp:140`): the one-line info text —
    /// "current size" / "splits 1 X → N Y" / "joins N X → 1 Y" / "need N adjacent
    /// X to join".
    pub fn info_for_kind(&self, target: NodeKind) -> String {
        let cur_sz = size_for_kind(self.current_kind);
        let tgt_sz = size_for_kind(target);
        let cur_name = kind_type_name(self.current_kind);
        let tgt_name = kind_type_name(target);

        if tgt_sz == cur_sz {
            return "current size".to_string();
        }
        if tgt_sz < cur_sz {
            let n = if tgt_sz == 0 { 0 } else { cur_sz / tgt_sz };
            return format!("splits 1 {cur_name} \u{2192} {n} {tgt_name}");
        }
        let needed = if cur_sz == 0 { 0 } else { tgt_sz / cur_sz };
        if self.max_joinable_bytes() >= tgt_sz {
            return format!("joins {needed} {cur_name} \u{2192} 1 {tgt_name}");
        }
        format!("need {} adjacent {cur_name} to join", needed - 1)
    }

    /// The join kind for the current multi-selection, by total bytes
    /// (`2→Hex16, 4→Hex32, 8→Hex64, 16→Hex128`); valid only when contiguous and a
    /// power-of-two byte count. `None` if not joinable.
    pub fn multi_select_join_kind(&self) -> Option<NodeKind> {
        if self.multi_select_count <= 1 || !self.multi_select_contiguous {
            return None;
        }
        match self.multi_select_bytes {
            2 => Some(NodeKind::Hex16),
            4 => Some(NodeKind::Hex32),
            8 => Some(NodeKind::Hex64),
            16 => Some(NodeKind::Hex128),
            _ => None,
        }
    }
}

/// The display type-name of a kind (for info text; uses `kindToString`-style
/// lowercase type name).
fn kind_type_name(k: NodeKind) -> &'static str {
    type_name_for(k)
}

/// `hexLine(typeName, bytes)` (`hextoolbarpopup.cpp:97`): the preview line
/// `"<type left-just 7> <ascii>  <HEX HEX …>"` — ascii is printable-or-`.`, hex is
/// uppercase space-separated.
pub fn hex_line(type_name: &str, bytes: &[u8]) -> String {
    // Left-justify the type name to width 7.
    let type_field = format!("{type_name:<7}");
    let ascii: String = bytes
        .iter()
        .map(|&c| {
            if (0x20..0x7f).contains(&c) {
                c as char
            } else {
                '.'
            }
        })
        .collect();
    let hex: String = bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{type_field} {ascii}  {hex}")
}

/// The hit-action ids (`hextoolbarpopup.cpp:22`) — what a painted button does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HitAction {
    Size(NodeKind),
    Pin,
    Suggest,
    InsertAbove,
    InsertBelow,
    JoinSelected,
    FillGo,
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{HexToolbarEvent, HexToolbarPopup};

#[cfg(feature = "ui")]
mod view {
    use super::{HexPopupContext, HEX_SIZES, HEX_SIZE_LABELS};
    use crate::core::kind::NodeKind;
    use crate::ui::design::{color, tokens};
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::input::{Input, InputState};
    use gpui_component::{ActiveTheme as _, Sizable as _};

    /// The popup's outcome (the C++ signals).
    #[derive(Clone, Debug)]
    pub enum HexToolbarEvent {
        /// Pick a hex size (`sizeSelected(nodeId, kind)`).
        SizeSelected(u64, NodeKind),
        /// Insert a hex node above (`insertAbove`).
        InsertAbove(u64),
        /// Insert a hex node below (`insertBelow`).
        InsertBelow(u64),
        /// Join the current multi-selection (`joinSelected`).
        JoinSelected,
        /// Fill to a target offset (`fillToOffset(nodeId, targetOffset)`).
        FillToOffset(u64, u64),
        /// Dismissed.
        Dismissed,
    }

    /// The hex-toolbar popover view. Pinned mode (persistent) is modeled as a
    /// flag; the C++ uses a distinct window type, but the visible toolbar is the
    /// same (widgets-dialogs §24 Q3).
    pub struct HexToolbarPopup {
        ctx: HexPopupContext,
        pinned: bool,
        offset_input: Entity<InputState>,
        focus_handle: FocusHandle,
    }

    impl HexToolbarPopup {
        /// Build the toolbar for a hex node context.
        pub fn new(ctx: HexPopupContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
            let offset_input = cx.new(|cx| InputState::new(window, cx).placeholder("offset"));
            HexToolbarPopup {
                ctx,
                pinned: false,
                offset_input,
                focus_handle: cx.focus_handle(),
            }
        }

        /// Whether the toolbar is pinned (persistent).
        pub fn is_pinned(&self) -> bool {
            self.pinned
        }

        /// The current context.
        pub fn context(&self) -> &HexPopupContext {
            &self.ctx
        }

        fn pick_size(&mut self, kind: NodeKind, cx: &mut Context<Self>) {
            if kind != self.ctx.current_kind {
                cx.emit(HexToolbarEvent::SizeSelected(self.ctx.node_id, kind));
            }
            if !self.pinned {
                cx.emit(HexToolbarEvent::Dismissed);
            }
        }

        fn toggle_pin(&mut self, cx: &mut Context<Self>) {
            self.pinned = !self.pinned;
            cx.notify();
        }

        fn fill_go(&mut self, cx: &mut Context<Self>) {
            let text = self.offset_input.read(cx).value().to_string();
            // Parse as hex (the C++ parses m_offsetEdit as hex16).
            let cleaned = text
                .trim()
                .trim_start_matches("0x")
                .trim_start_matches("0X");
            if let Ok(off) = u64::from_str_radix(cleaned, 16) {
                cx.emit(HexToolbarEvent::FillToOffset(self.ctx.node_id, off));
            }
        }
    }

    impl Focusable for HexToolbarPopup {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl EventEmitter<HexToolbarEvent> for HexToolbarPopup {}

    impl Render for HexToolbarPopup {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::button::{Button, ButtonVariants as _};

            let accent = color::accent(cx);
            let fg = color::text(cx);
            let muted = color::text_muted(cx);
            let disabled = color::text_disabled(cx);
            let hover_bg = color::hover_overlay(cx);
            let sel_bg = color::selected_bg(cx);
            let border = color::border(cx);

            // Size button row — a segmented chip group (Zed toggle row): each size
            // is a small chip, the current kind soft-accent-filled, undoable sizes
            // dimmed + non-interactive.
            let size_buttons: Vec<AnyElement> = HEX_SIZES
                .iter()
                .zip(HEX_SIZE_LABELS.iter())
                .map(|(&kind, label)| {
                    let is_current = kind == self.ctx.current_kind;
                    let can_do = self.ctx.can_do(kind);
                    let chip_fg = if is_current {
                        accent
                    } else if can_do {
                        fg
                    } else {
                        disabled
                    };
                    gpui_component::h_flex()
                        .id(("hex-size", kind as usize))
                        .h(px(22.))
                        .min_w(px(28.))
                        .px(px(tokens::space::MD))
                        .items_center()
                        .justify_center()
                        .rounded(px(tokens::radius::MD))
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(chip_fg)
                        .when(is_current, |d| {
                            d.bg(sel_bg).font_weight(FontWeight::SEMIBOLD)
                        })
                        .when(can_do && !is_current, |d| {
                            d.cursor_pointer().hover(|s| s.bg(hover_bg))
                        })
                        .when(can_do, |d| {
                            d.on_click(cx.listener(move |this, _e, _w, cx| {
                                this.pick_size(kind, cx);
                            }))
                        })
                        .child(label.to_string())
                        .into_any_element()
                })
                .collect();

            // The hovered-size preview/info (default to the current kind).
            let preview = self.ctx.preview_for_kind(self.ctx.current_kind);
            let info = self.ctx.info_for_kind(self.ctx.current_kind);

            super::super::design::elevated_surface(cx)
                .id("rcx-hex-toolbar")
                .track_focus(&self.focus_handle)
                .key_context("RcxHexToolbar")
                .flex()
                .flex_col()
                .min_w(px(280.))
                .p(px(tokens::space::MD))
                .gap(px(tokens::space::MD))
                .text_size(px(tokens::font::UI_MD))
                .child(
                    gpui_component::h_flex()
                        .w_full()
                        .gap(px(tokens::space::XS))
                        .items_center()
                        .children(size_buttons)
                        // Pin toggle pushed to the right.
                        .child(
                            gpui_component::h_flex()
                                .id("hex-pin")
                                .ml_auto()
                                .h(px(22.))
                                .px(px(tokens::space::SM))
                                .items_center()
                                .justify_center()
                                .rounded(px(tokens::radius::MD))
                                .text_color(if self.pinned { accent } else { muted })
                                .cursor_pointer()
                                .when(self.pinned, |d| d.bg(sel_bg))
                                .when(!self.pinned, |d| d.hover(|s| s.bg(hover_bg)))
                                .on_click(cx.listener(|this, _e, _w, cx| this.toggle_pin(cx)))
                                .child("\u{1F4CC}"),
                        ),
                )
                // Monospace byte preview block on a slightly inset surface.
                .child(
                    div()
                        .w_full()
                        .px(px(tokens::space::MD))
                        .py(px(tokens::space::SM))
                        .rounded(px(tokens::radius::MD))
                        .bg(cx.theme().background)
                        .border_1()
                        .border_color(border)
                        .font_family(tokens::font::MONO_FAMILY)
                        .text_size(px(tokens::font::EDITOR_SIZE))
                        .text_color(fg)
                        .whitespace_nowrap()
                        .child(preview),
                )
                .child(
                    div()
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(muted)
                        .child(info),
                )
                .when(self.pinned, |this| {
                    this.child(div().h(px(tokens::border::THIN)).w_full().bg(border))
                        .child(
                            gpui_component::h_flex()
                                .gap(px(tokens::space::XS))
                                .child(
                                    Button::new("hex-ins-above")
                                        .ghost()
                                        .small()
                                        .label("+ hex64 above")
                                        .on_click(cx.listener(|this, _e, _window, cx| {
                                            cx.emit(HexToolbarEvent::InsertAbove(this.ctx.node_id));
                                        })),
                                )
                                .child(
                                    Button::new("hex-ins-below")
                                        .ghost()
                                        .small()
                                        .label("+ hex64 below")
                                        .on_click(cx.listener(|this, _e, _window, cx| {
                                            cx.emit(HexToolbarEvent::InsertBelow(this.ctx.node_id));
                                        })),
                                ),
                        )
                        .child(
                            gpui_component::h_flex()
                                .gap(px(tokens::space::XS))
                                .items_center()
                                .child(
                                    div()
                                        .text_size(px(tokens::font::UI_SM))
                                        .text_color(muted)
                                        .child("Fill to"),
                                )
                                .child(Input::new(&self.offset_input).w(px(96.)))
                                .child(
                                    Button::new("hex-fill-go")
                                        .small()
                                        .primary()
                                        .label("Go")
                                        .on_click(
                                            cx.listener(|this, _e, _window, cx| this.fill_go(cx)),
                                        ),
                                ),
                        )
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{hex_line, Adjacent, HexPopupContext};
    use crate::core::kind::NodeKind;

    fn ctx_with(kind: NodeKind, data: Vec<u8>) -> HexPopupContext {
        HexPopupContext {
            current_kind: kind,
            data,
            ..Default::default()
        }
    }

    #[test]
    fn hex_line_formats_type_ascii_hex() {
        // "hex8  " left-justified to 7, ascii "AB", hex "41 42".
        let line = hex_line("hex8", b"AB");
        assert_eq!(line, "hex8    AB  41 42");
    }

    #[test]
    fn hex_line_nonprintable_becomes_dot() {
        let line = hex_line("hex16", &[0x00, 0x41, 0xff, 0x42]);
        // ascii: . A . B ; hex uppercase.
        assert_eq!(line, "hex16   .A.B  00 41 FF 42");
    }

    #[test]
    fn max_joinable_same_kind_run() {
        let mut ctx = ctx_with(NodeKind::Hex32, vec![0; 4]);
        ctx.nexts = vec![
            Adjacent {
                exists: true,
                kind: NodeKind::Hex32,
                data: vec![0; 4],
            },
            Adjacent {
                exists: true,
                kind: NodeKind::Hex32,
                data: vec![0; 4],
            },
        ];
        // 4 + 4 + 4 = 12.
        assert_eq!(ctx.max_joinable_bytes(), 12);
    }

    #[test]
    fn max_joinable_stops_at_mismatch() {
        let mut ctx = ctx_with(NodeKind::Hex32, vec![0; 4]);
        ctx.nexts = vec![
            Adjacent {
                exists: true,
                kind: NodeKind::Hex32,
                data: vec![0; 4],
            },
            Adjacent {
                exists: true,
                kind: NodeKind::Hex16,
                data: vec![0; 2],
            }, // mismatch
            Adjacent {
                exists: true,
                kind: NodeKind::Hex32,
                data: vec![0; 4],
            },
        ];
        // 4 + 4, then stops at the Hex16 → 8.
        assert_eq!(ctx.max_joinable_bytes(), 8);
    }

    #[test]
    fn max_joinable_caps_at_16() {
        let mut ctx = ctx_with(NodeKind::Hex64, vec![0; 8]);
        ctx.nexts = vec![
            Adjacent {
                exists: true,
                kind: NodeKind::Hex64,
                data: vec![0; 8],
            },
            Adjacent {
                exists: true,
                kind: NodeKind::Hex64,
                data: vec![0; 8],
            },
        ];
        // 8 + 8 = 16, capped.
        assert_eq!(ctx.max_joinable_bytes(), 16);
    }

    #[test]
    fn can_do_split_and_join() {
        let ctx = ctx_with(NodeKind::Hex32, vec![0; 4]);
        // Splitting to a smaller size is always doable.
        assert!(ctx.can_do(NodeKind::Hex8));
        assert!(ctx.can_do(NodeKind::Hex16));
        assert!(ctx.can_do(NodeKind::Hex32));
        // Joining to Hex64 needs 8 joinable bytes; with no adjacents → only 4.
        assert!(!ctx.can_do(NodeKind::Hex64));
    }

    #[test]
    fn info_current_size() {
        let ctx = ctx_with(NodeKind::Hex32, vec![0; 4]);
        assert_eq!(ctx.info_for_kind(NodeKind::Hex32), "current size");
    }

    #[test]
    fn info_split_text() {
        let ctx = ctx_with(NodeKind::Hex32, vec![0; 4]);
        // Hex32 (4B) → Hex8 (1B): splits 1 hex32 → 4 hex8.
        assert_eq!(
            ctx.info_for_kind(NodeKind::Hex8),
            "splits 1 hex32 \u{2192} 4 hex8"
        );
    }

    #[test]
    fn info_join_text_when_possible() {
        let mut ctx = ctx_with(NodeKind::Hex32, vec![0; 4]);
        ctx.nexts = vec![Adjacent {
            exists: true,
            kind: NodeKind::Hex32,
            data: vec![0; 4],
        }];
        // Hex32 → Hex64: needs 2 hex32; joinable = 8 ≥ 8 → joins 2 hex32 → 1 hex64.
        assert_eq!(
            ctx.info_for_kind(NodeKind::Hex64),
            "joins 2 hex32 \u{2192} 1 hex64"
        );
    }

    #[test]
    fn info_join_text_when_not_enough() {
        let ctx = ctx_with(NodeKind::Hex32, vec![0; 4]);
        // Hex32 → Hex64 with no adjacents: need 1 adjacent hex32 to join.
        assert_eq!(
            ctx.info_for_kind(NodeKind::Hex64),
            "need 1 adjacent hex32 to join"
        );
    }

    #[test]
    fn preview_same_size_one_line() {
        let ctx = ctx_with(NodeKind::Hex16, vec![0x41, 0x42]);
        let p = ctx.preview_for_kind(NodeKind::Hex16);
        assert_eq!(p.lines().count(), 1);
        assert!(p.contains("41 42"));
    }

    #[test]
    fn preview_split_multiple_lines() {
        let ctx = ctx_with(NodeKind::Hex32, vec![0x41, 0x42, 0x43, 0x44]);
        let p = ctx.preview_for_kind(NodeKind::Hex8);
        // 4 bytes / 1 byte = 4 lines.
        assert_eq!(p.lines().count(), 4);
    }

    #[test]
    fn preview_join_pads_to_target() {
        let mut ctx = ctx_with(NodeKind::Hex32, vec![0x41, 0x42, 0x43, 0x44]);
        ctx.nexts = vec![Adjacent {
            exists: true,
            kind: NodeKind::Hex32,
            data: vec![0x45, 0x46, 0x47, 0x48],
        }];
        // Hex32 → Hex64 (8 bytes): merges the two 4-byte nodes → 8 bytes, one line.
        let p = ctx.preview_for_kind(NodeKind::Hex64);
        assert_eq!(p.lines().count(), 1);
        assert!(p.contains("41 42 43 44 45 46 47 48"));
    }

    #[test]
    fn multi_select_join_kind_by_bytes() {
        let mut ctx = ctx_with(NodeKind::Hex8, vec![0]);
        ctx.multi_select_count = 4;
        ctx.multi_select_contiguous = true;
        ctx.multi_select_bytes = 4;
        assert_eq!(ctx.multi_select_join_kind(), Some(NodeKind::Hex32));
        // Non-power-of-two → None.
        ctx.multi_select_bytes = 3;
        assert_eq!(ctx.multi_select_join_kind(), None);
        // Non-contiguous → None.
        ctx.multi_select_bytes = 8;
        ctx.multi_select_contiguous = false;
        assert_eq!(ctx.multi_select_join_kind(), None);
    }
}
