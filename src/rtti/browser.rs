//! RTTI Browser — the modal viewer + the field-resolution / text-report logic
//! behind Tools ▸ RTTI Browser (Ctrl+Shift+R).
//!
//! Faithful port of `src/rttibrowser.h` (`RttiBrowserDialog`,
//! `RttiBrowserDialog::buildTextReport`) and the `MainWindow::showRttiBrowser`
//! flow plus the Tools-menu field resolution (`main.cpp:1523-1561` &
//! `main.cpp:4395-4413`).
//!
//! Split (the foundation-stage philosophy: gpui-free logic + a thin view):
//! - [`build_text_report`] — the "Copy as tree" / test text dump (← C++
//!   `buildTextReport`), pure and unit-tested.
//! - [`resolve_rtti`] — walk MSVC ([`walk_rtti`]), then fall back to Itanium
//!   ([`walk_rtti_itanium`]); the C++ `showRttiBrowser` only ran the MSVC walker,
//!   but the PORTING spec asks the browser to try both ABIs so an Itanium target
//!   resolves too. Returns the first `ok` [`RttiInfo`], else the MSVC failure
//!   (the more descriptive error for the common Windows target).
//! - [`resolve_field_vtable`] — the Tools-menu field gate (`main.cpp:1539-1561`):
//!   mask the selected id, find the node, require a 4/8-byte word
//!   (Hex32/Hex64/Pointer32/Pointer64), compute its absolute address, read the
//!   stored word, reject null. Returns the candidate vtable VA.
//! - [`header_lines`] — the metadata strip shown above the tabs, kept pure so the
//!   exact label/value pairs are unit-tested (vtable / module / imagebase / COL /
//!   offset).
//! - [`RttiBrowserDialog`] / [`RttiBrowserEvent`] (gpui, `ui` feature) — the modal
//!   with a header summary, a Hierarchy tab, a Vtable tab, and a "Copy as tree"
//!   button (← `RttiBrowserDialog`).
//!
//! ## Host wiring (`MainWindow::open_rtti_browser`, `window.rs` — other owner)
//!
//! The window-level handler replaces its notify-only stub with (the C++
//! `showRttiBrowser` flow + the Itanium fallback):
//!
//! ```ignore
//! use crate::rtti::browser::{resolve_field_vtable, resolve_rtti, RttiBrowserDialog, RttiBrowserEvent};
//!
//! let Some(editor) = self.document_area.read(cx).active_editor().cloned() else { return };
//! let ed = editor.read(cx);
//! let ctrl = ed.controller();
//! let tree = ctrl.tree();
//! let ptr_size = if tree.pointer_size > 0 { tree.pointer_size } else { 8 };
//! let sel: Vec<u64> = ctrl.selected_ids().iter().copied().collect();
//! let Some(prov) = ctrl.provider() else { self.notify("No active provider", ..); return };
//! let vtable = match resolve_field_vtable(tree, &sel, prov.as_ref()) {
//!     Ok(v) => v,
//!     Err(e) => { self.notify(e.message(), window, cx); return; }
//! };
//! let info = resolve_rtti(prov.as_ref(), vtable, ptr_size, 64);
//! if !info.ok {
//!     self.notify(if info.error.is_empty() { "No RTTI structures found." } else { &info.error }, ..);
//!     return;
//! }
//! let dlg = cx.new(|cx| RttiBrowserDialog::new(info, window, cx));
//! let focus = dlg.read(cx).focus_handle(cx);
//! self.goto_sub = Some(cx.subscribe_in(&dlg, window, |_t, _d, ev: &RttiBrowserEvent, window, cx| {
//!     match ev { RttiBrowserEvent::Close => window.close_dialog(cx) }
//! }));
//! let dlg_for_modal = dlg.clone();
//! window.open_dialog(cx, move |d, _w, _c| d.w(px(720.)).margin_top(px(80.)).close_button(false).child(dlg_for_modal.clone()));
//! window.focus(&focus, cx);
//! ```

use crate::core::kind::NodeKind;
use crate::core::tree::NodeTree;
use crate::provider::Provider;
use crate::rtti::walk::{walk_rtti_itanium_with_modules, walk_rtti_with_modules, RttiInfo};

/// `RttiBrowserDialog::buildTextReport(info)` (`rttibrowser.h:139`). A plain-text
/// dump used by "Copy as tree" and tests. Byte-for-byte field order/format as the
/// C++: lowercase `0x%x` hex (no leading zeros), `(no symbol)` placeholder, and a
/// 2-wide right-justified slot index.
pub fn build_text_report(info: &RttiInfo) -> String {
    let class_name = if info.demangled_name.is_empty() {
        &info.raw_name
    } else {
        &info.demangled_name
    };

    let mut out = String::new();
    out.push_str(&format!("Class: {class_name}\n"));
    if !info.abi.is_empty() {
        out.push_str(&format!("ABI:   {}\n", info.abi));
    }
    if !info.raw_name.is_empty() {
        out.push_str(&format!("Raw:   {}\n", info.raw_name));
    }
    out.push_str(&format!("Vtable: 0x{:x}\n", info.vtable_address));
    if !info.module_name.is_empty() {
        out.push_str(&format!("Module: {}\n", info.module_name));
    }
    out.push_str(&format!("COL:    0x{:x}\n", info.complete_locator));
    out.push_str(&format!("Offset: {}\n\n", info.offset));

    out.push_str(&format!("Hierarchy ({}):\n", info.bases.len()));
    for b in &info.bases {
        let name = if b.demangled_name.is_empty() {
            &b.raw_name
        } else {
            &b.demangled_name
        };
        out.push_str(&format!("  {name}\n"));
    }

    out.push_str(&format!("\nVtable ({}):\n", info.vtable.len()));
    for m in &info.vtable {
        let sym = if m.symbol.is_empty() {
            "(no symbol)"
        } else {
            &m.symbol
        };
        // `%1,2` right-justifies the slot to width 2 (Qt's `arg(m.slot, 2)`).
        out.push_str(&format!("  [{:>2}] 0x{:x}  {}\n", m.slot, m.address, sym));
    }
    out
}

/// Walk RTTI at `vtable_addr`, trying MSVC first then Itanium.
///
/// The C++ `showRttiBrowser` calls only `walkRtti` (MSVC); the PORTING spec wants
/// the browser to resolve either ABI, so we fall back to `walkRttiItanium` when
/// the MSVC walk fails. Returns the first `ok` result; if **both** fail, the MSVC
/// [`RttiInfo`] is returned (its error is the more useful one for the common
/// Windows target, and it preserves the C++ behaviour when only MSVC is present).
///
/// `pointer_size` is 4 (x86) or 8 (x64); `max_vtable_slots` caps enumeration
/// (the C++ default is 64).
pub fn resolve_rtti(
    prov: &dyn Provider,
    vtable_addr: u64,
    pointer_size: i32,
    max_vtable_slots: i32,
) -> RttiInfo {
    let modules = prov.modules_cached();
    let msvc = walk_rtti_with_modules(prov, &modules, vtable_addr, pointer_size, max_vtable_slots);
    if msvc.ok {
        return msvc;
    }
    let itanium =
        walk_rtti_itanium_with_modules(prov, &modules, vtable_addr, pointer_size, max_vtable_slots);
    if itanium.ok {
        return itanium;
    }
    msvc
}

/// Why a selected field can't be used as an RTTI vtable source. Each variant maps
/// to a C++ `setAppStatus(...)` string in `main.cpp:1523-1561`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RttiFieldError {
    /// No selection, or more than one row selected (`sel.size() != 1`).
    NoSingleSelection,
    /// The masked id doesn't resolve to a node (`indexOfId < 0`).
    NodeNotFound,
    /// The node isn't a 4- or 8-byte word (not Hex32/64 or Pointer32/64).
    NotAWord,
    /// `computeOffset` returned negative (the field has no concrete address).
    NegativeOffset,
    /// The word read at the field address is 0 ("Field is null").
    NullValue,
}

impl RttiFieldError {
    /// The exact C++ `setAppStatus` message for this rejection.
    pub fn message(&self) -> &'static str {
        match self {
            RttiFieldError::NoSingleSelection => "Select a hex/pointer field first",
            RttiFieldError::NodeNotFound => "Select a hex/pointer field first",
            RttiFieldError::NotAWord => "Selected field isn't a 4/8-byte word",
            RttiFieldError::NegativeOffset => "Select a hex/pointer field first",
            RttiFieldError::NullValue => "Field is null",
        }
    }
}

/// `true` for the four field kinds the Tools-menu gate accepts: Hex64 / Pointer64
/// (8-byte) and Hex32 / Pointer32 (4-byte) (`main.cpp:1545-1547`).
fn is_rtti_word_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Hex64 | NodeKind::Pointer64 | NodeKind::Hex32 | NodeKind::Pointer32
    )
}

/// Resolve the candidate vtable VA from the user's single selected field
/// (`main.cpp:1539-1561`).
///
/// `selected_ids` is the controller's current selection (the raw ids, possibly
/// carrying footer/array-elem/member sub-line bits). Steps, verbatim from the
/// C++:
///   1. Require exactly one selected id; mask off the footer / array-elem /
///      member sub-line bits to recover the node id.
///   2. `indexOfId` → the node; require a Hex32/64 or Pointer32/64 kind.
///   3. `computeOffset` (reject negative) → `baseAddress + offset` = the field's
///      absolute address.
///   4. Read an 8-byte word for the 64-bit kinds, else a 4-byte word; reject 0.
///
/// Returns the read word (the vtable candidate) on success.
pub fn resolve_field_vtable(
    tree: &NodeTree,
    selected_ids: &[u64],
    prov: &dyn Provider,
) -> Result<u64, RttiFieldError> {
    if selected_ids.len() != 1 {
        return Err(RttiFieldError::NoSingleSelection);
    }
    let nid = crate::core::linemeta::base_node_id_from_sel_id(selected_ids[0]);

    let idx = tree.index_of_id(nid);
    if idx < 0 {
        return Err(RttiFieldError::NodeNotFound);
    }
    let node = &tree.nodes[idx as usize];
    if !is_rtti_word_kind(node.kind) {
        return Err(RttiFieldError::NotAWord);
    }
    let is64 = matches!(node.kind, NodeKind::Hex64 | NodeKind::Pointer64);

    let off = tree.compute_offset(idx);
    if off < 0 {
        return Err(RttiFieldError::NegativeOffset);
    }
    let addr = tree.base_address.wrapping_add(off as u64);

    let val = if is64 {
        prov.read_u64(addr)
    } else {
        prov.read_u32(addr) as u64
    };
    if val == 0 {
        return Err(RttiFieldError::NullValue);
    }
    Ok(val)
}

/// The header summary shown above the tabs, as `(label, value)` pairs — the
/// metadata line the C++ packs into one rich-text `<div>` (`rttibrowser.h:67-79`):
/// vtable, module (when known), imagebase (when non-zero), COL, offset. Kept pure
/// so the exact fields/format are unit-tested independently of the gpui view.
pub fn header_lines(info: &RttiInfo) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(5);
    out.push(("vtable".to_owned(), format!("0x{:x}", info.vtable_address)));
    if !info.module_name.is_empty() {
        out.push(("module".to_owned(), info.module_name.clone()));
    }
    if info.image_base != 0 {
        out.push(("imagebase".to_owned(), format!("0x{:x}", info.image_base)));
    }
    out.push(("COL".to_owned(), format!("0x{:x}", info.complete_locator)));
    out.push(("offset".to_owned(), info.offset.to_string()));
    out
}

/// The class name shown in the title / header (`demangledName` when present, else
/// the raw mangled name) — the C++ ternary repeated in the dialog and the report.
pub fn display_class_name(info: &RttiInfo) -> &str {
    if info.demangled_name.is_empty() {
        &info.raw_name
    } else {
        &info.demangled_name
    }
}

// ── gpui modal view (ui feature) ─────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{RttiBrowserDialog, RttiBrowserEvent};

#[cfg(feature = "ui")]
mod view {
    use super::{build_text_report, display_class_name, header_lines};
    use crate::rtti::walk::RttiInfo;
    use crate::ui::design::{color, tokens, zed_list_row};
    use crate::ui::dialogs::modal;
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;

    /// Which tab is showing (the C++ `QTabWidget` index).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Tab {
        /// Base classes in resolution order.
        Hierarchy,
        /// Virtual method table: slot / address / symbol.
        Vtable,
    }

    /// The dialog outcome raised to the host (the C++ `accept`).
    #[derive(Clone, Debug)]
    pub enum RttiBrowserEvent {
        /// Close pressed / Esc.
        Close,
    }

    /// The RTTI Browser modal — header metadata, a Hierarchy tab, a Vtable tab,
    /// and "Copy as tree". Built from an already-walked [`RttiInfo`] (the host
    /// runs [`resolve_rtti`](super::resolve_rtti) and only opens this when `ok`).
    pub struct RttiBrowserDialog {
        info: RttiInfo,
        tab: Tab,
        focus_handle: FocusHandle,
    }

    impl RttiBrowserDialog {
        /// Build the viewer for a successful RTTI walk.
        pub fn new(info: RttiInfo, _window: &mut Window, cx: &mut Context<Self>) -> Self {
            RttiBrowserDialog {
                info,
                tab: Tab::Hierarchy,
                focus_handle: cx.focus_handle(),
            }
        }

        /// The dialog's focus handle (so the host can focus it on open).
        pub fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }

        fn close(&mut self, cx: &mut Context<Self>) {
            cx.emit(RttiBrowserEvent::Close);
        }

        fn copy_report(&mut self, cx: &mut Context<Self>) {
            let text = build_text_report(&self.info);
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }

        /// Handle Escape (close) — capture-phase, like the other modals.
        fn handle_nav_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
            if key == "escape" {
                self.close(cx);
                return true;
            }
            false
        }

        /// One header metadata line, mono-faced and muted, label + value chips
        /// separated by the C++ `·` middot.
        fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
            let name = display_class_name(&self.info).to_string();
            let abi = self.info.abi.clone();
            let raw = self.info.raw_name.clone();
            let show_raw = !raw.is_empty() && raw != name;
            let mono = SharedString::from(tokens::font::mono_family());

            let meta = header_lines(&self.info);
            let mut meta_row = gpui_component::h_flex()
                .flex_wrap()
                .gap(px(tokens::space::SM))
                .font_family(mono.clone())
                .text_size(px(tokens::font::UI_SM))
                .text_color(color::text_muted(cx));
            for (i, (label, value)) in meta.iter().enumerate() {
                if i > 0 {
                    meta_row = meta_row.child(div().text_color(color::text_muted(cx)).child("·"));
                }
                meta_row = meta_row.child(format!("{label} {value}"));
            }

            gpui_component::v_flex()
                .w_full()
                .gap(px(tokens::space::XS))
                .child(
                    gpui_component::h_flex()
                        .items_baseline()
                        .gap(px(tokens::space::MD))
                        .child(
                            div()
                                .text_size(px(tokens::font::UI_LG))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(color::text(cx))
                                .child(name),
                        )
                        .when(!abi.is_empty(), |b| {
                            b.child(
                                div()
                                    .text_size(px(tokens::font::UI_SM))
                                    .text_color(color::text_muted(cx))
                                    .child(format!("({abi})")),
                            )
                        }),
                )
                .when(show_raw, |b| {
                    b.child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text_muted(cx))
                            .child(raw.clone()),
                    )
                })
                .child(meta_row)
        }

        /// The tab strip: two Zed-style underlined tabs with live counts.
        fn render_tabs(&self, cx: &Context<Self>) -> impl IntoElement {
            let active = self.tab;
            let hier_n = self.info.bases.len();
            let vt_n = self.info.vtable.len();
            gpui_component::h_flex()
                .w_full()
                .gap(px(tokens::space::LG))
                .border_b_1()
                .border_color(color::border(cx))
                .child(self.tab_button(
                    "rcx-rtti-tab-hier",
                    Tab::Hierarchy,
                    format!("Hierarchy ({hier_n})"),
                    active == Tab::Hierarchy,
                    cx,
                ))
                .child(self.tab_button(
                    "rcx-rtti-tab-vt",
                    Tab::Vtable,
                    format!("Vtable ({vt_n})"),
                    active == Tab::Vtable,
                    cx,
                ))
        }

        fn tab_button(
            &self,
            id: &'static str,
            tab: Tab,
            label: String,
            active: bool,
            cx: &Context<Self>,
        ) -> impl IntoElement {
            let accent = color::accent(cx);
            div()
                .id(id)
                .px(px(tokens::space::SM))
                .py(px(tokens::space::SM))
                .text_size(px(tokens::font::UI_SM))
                .cursor_pointer()
                .border_b_2()
                .when(active, |b| {
                    b.border_color(accent).text_color(color::text(cx))
                })
                .when(!active, |b| {
                    b.border_color(gpui::transparent_black())
                        .text_color(color::text_muted(cx))
                        .hover(|s| s.text_color(color::text(cx)))
                })
                .on_click(cx.listener(move |this, _e, _window, cx| {
                    if this.tab != tab {
                        this.tab = tab;
                        cx.notify();
                    }
                }))
                .child(label)
        }

        /// Hierarchy tab body — one row per base class (demangled, raw on the
        /// right), or an empty-state line.
        fn render_hierarchy(&self, cx: &Context<Self>) -> impl IntoElement {
            let mono = SharedString::from(tokens::font::mono_family());
            let mut list = gpui_component::v_flex()
                .w_full()
                .gap(px(tokens::space::XXS))
                .font_family(mono.clone())
                .text_size(px(tokens::font::UI_SM));
            if self.info.bases.is_empty() {
                return list
                    .child(modal::help_text("No base classes.", cx))
                    .into_any_element();
            }
            for (ix, b) in self.info.bases.iter().enumerate() {
                let name = if b.demangled_name.is_empty() {
                    b.raw_name.clone()
                } else {
                    b.demangled_name.clone()
                };
                let raw = b.raw_name.clone();
                let show_raw = !raw.is_empty() && raw != name;
                list = list.child(
                    zed_list_row(SharedString::from(format!("rcx-rtti-hier-{ix}")), false, cx)
                        .justify_between()
                        .child(div().child(name))
                        .when(show_raw, |r| {
                            r.child(div().text_color(color::text_muted(cx)).child(raw.clone()))
                        }),
                );
            }
            list.into_any_element()
        }

        /// Vtable tab body — one row per virtual method: slot, address, symbol
        /// (`(no symbol)` when unresolved), or an empty-state line.
        fn render_vtable(&self, cx: &Context<Self>) -> impl IntoElement {
            let mono = SharedString::from(tokens::font::mono_family());
            let mut list = gpui_component::v_flex()
                .w_full()
                .gap(px(tokens::space::XXS))
                .font_family(mono.clone())
                .text_size(px(tokens::font::UI_SM));
            if self.info.vtable.is_empty() {
                return list
                    .child(modal::help_text("No vtable entries.", cx))
                    .into_any_element();
            }
            for m in &self.info.vtable {
                let sym = if m.symbol.is_empty() {
                    "(no symbol)".to_string()
                } else {
                    m.symbol.clone()
                };
                let sym_muted = m.symbol.is_empty();
                let slot = m.slot;
                let addr = m.address;
                list = list.child(
                    zed_list_row(SharedString::from(format!("rcx-rtti-vt-{slot}")), false, cx)
                        .gap(px(tokens::space::MD))
                        .child(
                            div()
                                .w(px(36.))
                                .flex_none()
                                .text_color(color::text_muted(cx))
                                .child(format!("[{slot}]")),
                        )
                        .child(
                            div()
                                .w(px(140.))
                                .flex_none()
                                .text_color(color::text(cx))
                                .child(format!("0x{addr:x}")),
                        )
                        .child(
                            div()
                                .flex_1()
                                .when(sym_muted, |b| b.text_color(color::text_muted(cx)))
                                .when(!sym_muted, |b| b.text_color(color::text(cx)))
                                .child(sym),
                        ),
                );
            }
            list.into_any_element()
        }
    }

    impl EventEmitter<RttiBrowserEvent> for RttiBrowserDialog {}

    impl Render for RttiBrowserDialog {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::button::{Button, ButtonVariants as _};

            // The C++ resizes the dialog to 720×480; clamp both to the window.
            let card_w = modal::clamp_width(720., window);
            let card_h = modal::clamp_height(480., 80., window);

            let tab_body = match self.tab {
                Tab::Hierarchy => self.render_hierarchy(cx).into_any_element(),
                Tab::Vtable => self.render_vtable(cx).into_any_element(),
            };

            let body = modal::body(cx)
                .child(self.render_header(cx))
                .child(self.render_tabs(cx))
                .child(
                    // The scrollable tab content fills the remaining height.
                    gpui_component::v_flex()
                        .id("rcx-rtti-scroll")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_y_scroll()
                        .child(tab_body),
                );

            // Copy is the left utility action; Close is the right primary, matching
            // the C++ `btnRow` (copy left, stretch, close right).
            let footer = modal::footer(cx)
                .justify_between()
                .child(
                    Button::new("rcx-rtti-copy")
                        .label("Copy as tree")
                        .on_click(cx.listener(|this, _e, _window, cx| this.copy_report(cx))),
                )
                .child(
                    Button::new("rcx-rtti-close")
                        .primary()
                        .label("Close")
                        .on_click(cx.listener(|this, _e, _window, cx| this.close(cx))),
                );

            modal::card(cx)
                .id("rcx-rtti-browser")
                .track_focus(&self.focus_handle)
                .key_context("RcxRttiBrowser")
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                    if this.handle_nav_key(ev.keystroke.key.as_str(), cx) {
                        cx.stop_propagation();
                    }
                }))
                .w(card_w)
                .h(card_h)
                .child(
                    modal::header(format!("RTTI · {}", display_class_name(&self.info)), cx).child(
                        modal::close_button(
                            "rcx-rtti-header-close",
                            cx.listener(|this, _e, _window, cx| this.close(cx)),
                            cx,
                        ),
                    ),
                )
                .child(body)
                .child(footer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    use crate::core::node::Node;
    use crate::provider::{BufferProvider, ModuleEntry};
    use crate::rtti::walk::{RttiBaseClass, RttiVirtualMethod};

    fn sample_info() -> RttiInfo {
        RttiInfo {
            ok: true,
            error: String::new(),
            abi: "MSVC".to_owned(),
            vtable_address: 0x1_4000_1000,
            image_base: 0x1_4000_0000,
            module_name: "game.exe".to_owned(),
            complete_locator: 0x1_4000_9000,
            offset: 0,
            raw_name: ".?AVFoo@@".to_owned(),
            demangled_name: "Foo".to_owned(),
            bases: vec![
                RttiBaseClass {
                    raw_name: ".?AVFoo@@".to_owned(),
                    demangled_name: "Foo".to_owned(),
                    depth: 0,
                },
                RttiBaseClass {
                    raw_name: ".?AVBar@@".to_owned(),
                    demangled_name: "Bar".to_owned(),
                    depth: 1,
                },
            ],
            vtable: vec![
                RttiVirtualMethod {
                    slot: 0,
                    address: 0x1_4000_2000,
                    symbol: "Foo::vfunc0".to_owned(),
                },
                RttiVirtualMethod {
                    slot: 1,
                    address: 0x1_4000_2010,
                    symbol: String::new(),
                },
            ],
        }
    }

    // ── buildTextReport (rttibrowser.h:139; the C++ test_rtti.cpp report check) ──
    #[test]
    fn text_report_matches_cpp_format() {
        let report = build_text_report(&sample_info());
        let expected = "Class: Foo\n\
            ABI:   MSVC\n\
            Raw:   .?AVFoo@@\n\
            Vtable: 0x140001000\n\
            Module: game.exe\n\
            COL:    0x140009000\n\
            Offset: 0\n\
            \n\
            Hierarchy (2):\n\
            \u{20}\u{20}Foo\n\
            \u{20}\u{20}Bar\n\
            \n\
            Vtable (2):\n\
            \u{20}\u{20}[ 0] 0x140002000  Foo::vfunc0\n\
            \u{20}\u{20}[ 1] 0x140002010  (no symbol)\n";
        assert_eq!(report, expected);
    }

    // The slot index is right-justified to width 2 (Qt `arg(slot, 2)`).
    #[test]
    fn text_report_slot_is_width_2() {
        let mut info = sample_info();
        info.vtable = vec![RttiVirtualMethod {
            slot: 12,
            address: 0xABC,
            symbol: "x".to_owned(),
        }];
        let report = build_text_report(&info);
        assert!(report.contains("  [12] 0xabc  x\n"), "{report}");
    }

    // Raw/ABI/Module lines are omitted when empty (the C++ `if (!…isEmpty())`).
    #[test]
    fn text_report_omits_empty_optional_lines() {
        let mut info = sample_info();
        info.abi.clear();
        info.raw_name.clear();
        info.module_name.clear();
        let report = build_text_report(&info);
        assert!(!report.contains("ABI:"), "{report}");
        assert!(!report.contains("Raw:"), "{report}");
        assert!(!report.contains("Module:"), "{report}");
        // Class falls back to the raw name (here empty) — still present.
        assert!(report.starts_with("Class: "), "{report}");
    }

    // Class falls back to raw name when there's no demangled name.
    #[test]
    fn display_name_falls_back_to_raw() {
        let mut info = sample_info();
        info.demangled_name.clear();
        assert_eq!(display_class_name(&info), ".?AVFoo@@");
    }

    // ── header_lines: vtable / module / imagebase / COL / offset ──
    #[test]
    fn header_lines_full() {
        let lines = header_lines(&sample_info());
        assert_eq!(
            lines,
            vec![
                ("vtable".to_owned(), "0x140001000".to_owned()),
                ("module".to_owned(), "game.exe".to_owned()),
                ("imagebase".to_owned(), "0x140000000".to_owned()),
                ("COL".to_owned(), "0x140009000".to_owned()),
                ("offset".to_owned(), "0".to_owned()),
            ]
        );
    }

    // Module + imagebase are dropped when unknown (the C++ `if`s).
    #[test]
    fn header_lines_drops_unknown_module_and_base() {
        let mut info = sample_info();
        info.module_name.clear();
        info.image_base = 0;
        let lines = header_lines(&info);
        let labels: Vec<&str> = lines.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, vec!["vtable", "COL", "offset"]);
    }

    // ── resolve_field_vtable: the Tools-menu gate (main.cpp:1539-1561) ──

    /// A provider whose 8 bytes at `addr` read back `word`; everything else is 0.
    fn word_provider(addr: u64, word: u64) -> BufferProvider {
        let mut data = vec![0u8; (addr as usize) + 16];
        data[addr as usize..addr as usize + 8].copy_from_slice(&word.to_le_bytes());
        BufferProvider::new(data, "word")
    }

    fn tree_with_field(kind: NodeKind, offset: i32) -> (NodeTree, u64) {
        let mut tree = NodeTree::default();
        tree.base_address = 0x1000;
        let node = Node {
            id: 42,
            kind,
            offset,
            ..Default::default()
        };
        tree.nodes = vec![node];
        (tree, 42)
    }

    #[test]
    fn resolve_field_reads_pointer64_word() {
        let (tree, id) = tree_with_field(NodeKind::Pointer64, 0x10);
        // Field absolute addr = base(0x1000) + offset(0x10) = 0x1010.
        let prov = word_provider(0x1010, 0xDEAD_BEEF);
        let got = resolve_field_vtable(&tree, &[id], &prov).unwrap();
        assert_eq!(got, 0xDEAD_BEEF);
    }

    #[test]
    fn resolve_field_reads_hex32_truncated() {
        let (tree, id) = tree_with_field(NodeKind::Hex32, 0);
        // base 0x1000, only the low 4 bytes are the field value.
        let prov = word_provider(0x1000, 0x1_2233_4455);
        let got = resolve_field_vtable(&tree, &[id], &prov).unwrap();
        assert_eq!(got, 0x2233_4455); // 32-bit read drops the high dword.
    }

    #[test]
    fn resolve_field_requires_single_selection() {
        let (tree, id) = tree_with_field(NodeKind::Pointer64, 0);
        let prov = word_provider(0x1000, 1);
        assert_eq!(
            resolve_field_vtable(&tree, &[], &prov),
            Err(RttiFieldError::NoSingleSelection)
        );
        assert_eq!(
            resolve_field_vtable(&tree, &[id, id + 1], &prov),
            Err(RttiFieldError::NoSingleSelection)
        );
    }

    #[test]
    fn resolve_field_rejects_non_word_kind() {
        let (tree, id) = tree_with_field(NodeKind::Int32, 0);
        let prov = word_provider(0x1000, 1);
        assert_eq!(
            resolve_field_vtable(&tree, &[id], &prov),
            Err(RttiFieldError::NotAWord)
        );
    }

    #[test]
    fn resolve_field_rejects_null_value() {
        let (tree, id) = tree_with_field(NodeKind::Pointer64, 0);
        let prov = word_provider(0x1000, 0);
        assert_eq!(
            resolve_field_vtable(&tree, &[id], &prov),
            Err(RttiFieldError::NullValue)
        );
    }

    #[test]
    fn resolve_field_rejects_unknown_id() {
        let (tree, _id) = tree_with_field(NodeKind::Pointer64, 0);
        let prov = word_provider(0x1000, 1);
        assert_eq!(
            resolve_field_vtable(&tree, &[9999], &prov),
            Err(RttiFieldError::NodeNotFound)
        );
    }

    // The selection id may carry footer/array/member sub-line bits — they're
    // masked off before lookup (main.cpp:1539).
    #[test]
    fn resolve_field_masks_subline_bits() {
        use crate::core::linemeta::{K_FOOTER_ID_BIT, K_MEMBER_BIT};
        let (tree, id) = tree_with_field(NodeKind::Pointer64, 0);
        let prov = word_provider(0x1000, 0x55);
        let decorated = id | K_FOOTER_ID_BIT | K_MEMBER_BIT;
        assert_eq!(resolve_field_vtable(&tree, &[decorated], &prov), Ok(0x55));
    }

    // ── resolve_rtti: MSVC-first, Itanium fallback ──

    /// A one-module provider (so Itanium's in-module checks pass).
    struct ModProv {
        inner: BufferProvider,
        base: u64,
        size: u64,
        module_calls: Cell<usize>,
    }
    impl Provider for ModProv {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            self.inner.read(addr, buf)
        }
        fn size(&self) -> i32 {
            self.inner.size()
        }
        fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            self.module_calls.set(self.module_calls.get() + 1);
            vec![ModuleEntry {
                name: "m".to_owned(),
                full_path: "m".to_owned(),
                base: self.base,
                size: self.size,
            }]
        }
    }

    // Build the same synthetic Itanium image the walker tests use, inline.
    fn itanium_image(mangled: &str) -> Vec<u8> {
        const BASE: u64 = 0x10000;
        const VT: u64 = 0x1000;
        const TI: u64 = 0x1100;
        const NAME: u64 = 0x1180;
        const TIVT: u64 = 0x1200;
        let mut data = vec![0u8; (BASE as usize) + 0x10000];
        let wq = |d: &mut [u8], off: u64, v: u64| {
            let off = off as usize;
            d[off..off + 8].copy_from_slice(&v.to_le_bytes());
        };
        wq(&mut data, BASE + VT - 16, 0);
        wq(&mut data, BASE + VT - 8, BASE + TI);
        for i in 0..3u64 {
            wq(&mut data, BASE + VT + i * 8, BASE + 0x100 + i * 0x10);
        }
        wq(&mut data, BASE + VT + 3 * 8, 0);
        wq(&mut data, BASE + TI, BASE + TIVT);
        wq(&mut data, BASE + TI + 8, BASE + NAME);
        let no = (BASE + NAME) as usize;
        let nb = mangled.as_bytes();
        data[no..no + nb.len()].copy_from_slice(nb);
        data[no + nb.len()] = 0;
        wq(&mut data, BASE + TIVT, 0xFEED_FACE);
        data
    }

    #[test]
    fn resolve_rtti_falls_back_to_itanium() {
        let prov = ModProv {
            inner: BufferProvider::new(itanium_image("3Foo"), "m"),
            base: 0x10000,
            size: 0x10000,
            module_calls: Cell::new(0),
        };
        // MSVC walk fails on this image (COL signature bad); Itanium succeeds.
        let info = resolve_rtti(&prov, 0x10000 + 0x1000, 8, 64);
        assert!(info.ok, "{}", info.error);
        assert_eq!(info.abi, "Itanium");
        assert_eq!(info.demangled_name, "Foo");
        assert_eq!(
            prov.module_calls.get(),
            1,
            "resolver should share one module snapshot across MSVC and Itanium walkers"
        );
    }

    #[test]
    fn resolve_rtti_returns_msvc_error_when_both_fail() {
        // A blank provider with no modules — both walkers fail; MSVC error wins.
        let prov = BufferProvider::new(vec![0u8; 0x100], "blank");
        let info = resolve_rtti(&prov, 0x40, 8, 64);
        assert!(!info.ok);
        // MSVC's first failure is the meta-pointer read (it's all zeros).
        assert!(
            info.error.contains("meta pointer") || info.error.contains("COL"),
            "{}",
            info.error
        );
    }
}
