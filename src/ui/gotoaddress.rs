//! Goto-Address dialog — jump to any [`AddressParser`]-evaluable expression
//! (`gotoaddressdialog.h`, widgets-dialogs.md §7).
//!
//! Port of `GotoAddressDialog`: a modal that takes an address formula
//! (`0x7FF6…`, `<game.exe>+0x40`, `[ntdll!Ldr]`, `ntdll!RtlAllocateHeap`),
//! live-validates it through [`crate::addr::AddressParser`] as the user types
//! (showing the resolved hex or the parser's error), and on accept pushes the
//! formula onto a recent list (most-recent-first, deduped, capped at 12) shared
//! via the app [`SettingsStore`] under `"gotoAddress/recent"`.
//!
//! Split (mirroring the foundation-stage philosophy: gpui-free logic + a thin
//! view):
//! - [`RECENT_KEY`] / [`MAX_RECENT`] — the C++ `kSettingsKey` / `kMaxRecent`.
//! - [`push_recent`] / [`load_recent`] / [`clear_recent`] / [`store_recent`] —
//!   the recent-list reducers + the [`SettingsStore`] (de)serialization, ported
//!   verbatim from the static `pushRecent`/`loadRecent`/`clearRecent` and
//!   unit-tested against `test_goto_address.cpp` (dedup-to-top, cap, order).
//! - [`GotoState`] — the live-validation reducer (`onTextChanged` + `accept`),
//!   pure over an [`AddressParserCallbacks`], unit-tested headlessly.
//! - [`GotoAddressDialog`] / [`GotoEvent`] — the gpui view raising
//!   `Go(formula,address)` / `Cancel` (the C++ `accept`/`reject`).
//!
//! Gated behind the `ui` feature.

use crate::addr::{AddressParseResult, AddressParser, AddressParserCallbacks};
use crate::theme::manager::SettingsStore;

/// `kSettingsKey` (`gotoaddressdialog.h:29`) — the recent-list settings key,
/// shared with anything else that reuses the same list.
pub const RECENT_KEY: &str = "gotoAddress/recent";

/// `kMaxRecent` (`gotoaddressdialog.h:30`) — recent-list cap (most-recent-first).
pub const MAX_RECENT: usize = 12;

/// Serialize a recent list to the single settings string. The C++ stored a real
/// `QStringList`; here the [`SettingsStore`] holds one string per key, so the
/// list is newline-joined (entries are address formulas — never contain `\n`).
fn join_recent(list: &[String]) -> String {
    list.join("\n")
}

/// Parse a recent list back from the settings string (the inverse of
/// [`join_recent`]). Empty / missing → empty list; blank lines are dropped.
fn split_recent(raw: &str) -> Vec<String> {
    if raw.is_empty() {
        return Vec::new();
    }
    raw.lines()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `GotoAddressDialog::loadRecent()` (`gotoaddressdialog.h:123`) — read the
/// persisted recent list (most-recent-first) from `store`.
pub fn load_recent(store: &dyn SettingsStore) -> Vec<String> {
    store
        .get(RECENT_KEY)
        .map(|s| split_recent(&s))
        .unwrap_or_default()
}

/// `GotoAddressDialog::clearRecent()` (`gotoaddressdialog.h:139`).
pub fn clear_recent(store: &mut dyn SettingsStore) {
    store.set(RECENT_KEY, "");
}

/// Persist a recent list to `store` (the storage side of [`push_recent`]; the
/// C++ `QSettings::setValue`).
pub fn store_recent(store: &mut dyn SettingsStore, list: &[String]) {
    store.set(RECENT_KEY, &join_recent(list));
}

/// The pure list transform behind `GotoAddressDialog::pushRecent`
/// (`gotoaddressdialog.h:128`): trim `entry`; if empty, return `list` unchanged;
/// else `removeAll(entry)` then `prepend` (dedup → top) and cap to [`MAX_RECENT`]
/// by dropping from the end. **Most-recent-first.** Returns the new list.
pub fn push_recent_list(list: &[String], entry: &str) -> Vec<String> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return list.to_vec();
    }
    let mut out: Vec<String> = Vec::with_capacity(list.len() + 1);
    out.push(trimmed.to_string());
    for e in list {
        if e != trimmed {
            out.push(e.clone());
        }
    }
    out.truncate(MAX_RECENT);
    out
}

/// `GotoAddressDialog::pushRecent(entry)` against the app [`SettingsStore`] —
/// load, [`push_recent_list`], store. No-op for an empty (trimmed) entry.
pub fn push_recent(store: &mut dyn SettingsStore, entry: &str) {
    let list = load_recent(store);
    let next = push_recent_list(&list, entry);
    store_recent(store, &next);
}

/// Step the recent-list selection within a list of `len` entries, mirroring the
/// C++ `QListWidget` keyboard navigation the dialog routes Up/Down into
/// (`gotoaddressdialog.h:152` `keyPressEvent` Down-into-list, then the list's own
/// Up/Down). `current` is the current selected row (`None` = no selection yet, as
/// when focus first moves into the list); `delta` is +1 (Down) or -1 (Up).
///
/// Rules (matching `QListWidget::setCurrentRow` clamping, not wrapping):
/// - empty list → `None`;
/// - no current + Down → row 0 (focus lands on the first row, the C++
///   `setCurrentRow(0)` on Down-into-list); no current + Up → last row;
/// - otherwise clamp `current + delta` to `0..len` (no wraparound, like Qt).
pub fn step_recent_selection(current: Option<usize>, delta: isize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len - 1;
    match current {
        None => Some(if delta < 0 { last } else { 0 }),
        Some(cur) => {
            let next = (cur as isize + delta).clamp(0, last as isize);
            Some(next as usize)
        }
    }
}

/// The status line the live validator shows under the input (`onTextChanged`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum GotoStatus {
    /// Empty input — blank status, Go disabled (`m_status = " "`).
    Idle,
    /// Resolved OK — `"→ 0x<hex>"` in the cold-heat color; Go enabled.
    Resolved(u64),
    /// Parse error — the parser's message (or `"invalid expression"`); Go disabled.
    Error(String),
}

/// The live-validation reducer — the gpui-free heart of the dialog
/// (`onTextChanged` + the `accept` guard; `gotoaddressdialog.h:145,164`).
///
/// Holds the last evaluated text, resolved address, and ok flag. Feed it the
/// input text on every change via [`evaluate`](GotoState::evaluate); read
/// [`status`](GotoState::status) / [`can_go`](GotoState::can_go) for the view,
/// and call [`accept`](GotoState::accept) to apply the C++ "can't go nowhere"
/// guard before committing.
#[derive(Clone, Debug, Default)]
pub struct GotoState {
    text: String,
    resolved: u64,
    last_ok: bool,
    status: GotoStatusCache,
}

/// Cached status (kept out of the public reducer surface; mirrors `m_status`).
#[derive(Clone, Debug, Default)]
struct GotoStatusCache {
    value: Option<GotoStatusKind>,
}

#[derive(Clone, Debug)]
enum GotoStatusKind {
    Idle,
    Resolved(u64),
    Error(String),
}

impl GotoState {
    /// A fresh reducer (empty input, Go disabled).
    pub fn new() -> Self {
        let mut s = GotoState::default();
        s.status.value = Some(GotoStatusKind::Idle);
        s
    }

    /// `onTextChanged(text)` (`gotoaddressdialog.h:164`): trim; empty → idle (Go
    /// disabled, resolved 0). Else evaluate via [`AddressParser::evaluate`] with
    /// the given `ptr_size` + callbacks: ok → resolved + Go enabled; error →
    /// error status + Go disabled. Returns the resulting [`GotoStatus`].
    pub fn evaluate(
        &mut self,
        text: &str,
        ptr_size: i32,
        cbs: Option<&AddressParserCallbacks<'_>>,
    ) -> GotoStatus {
        self.text = text.to_string();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            self.resolved = 0;
            self.last_ok = false;
            self.status.value = Some(GotoStatusKind::Idle);
            return GotoStatus::Idle;
        }
        let result: AddressParseResult = AddressParser::evaluate(trimmed, ptr_size, cbs);
        if result.ok {
            self.resolved = result.value;
            self.last_ok = true;
            self.status.value = Some(GotoStatusKind::Resolved(result.value));
            GotoStatus::Resolved(result.value)
        } else {
            self.resolved = 0;
            self.last_ok = false;
            let msg = if result.error.is_empty() {
                "invalid expression".to_string()
            } else {
                result.error.clone()
            };
            self.status.value = Some(GotoStatusKind::Error(msg.clone()));
            GotoStatus::Error(msg)
        }
    }

    /// The current status line.
    pub fn status(&self) -> GotoStatus {
        match &self.status.value {
            Some(GotoStatusKind::Resolved(v)) => GotoStatus::Resolved(*v),
            Some(GotoStatusKind::Error(e)) => GotoStatus::Error(e.clone()),
            _ => GotoStatus::Idle,
        }
    }

    /// Whether the Go button is enabled (`m_okButton->isEnabled()`): true iff the
    /// last evaluation resolved successfully.
    pub fn can_go(&self) -> bool {
        self.last_ok
    }

    /// The resolved absolute address (`resolvedAddress()`); 0 if none.
    pub fn resolved(&self) -> u64 {
        self.resolved
    }

    /// The trimmed input formula (`formula()`, preserved for rebases).
    pub fn formula(&self) -> String {
        self.text.trim().to_string()
    }

    /// The `accept()` override guard (`gotoaddressdialog.h:145`): refuse if
    /// nothing resolved (`resolved == 0 && !last_ok`); else return the
    /// `(formula, address)` to commit (the caller pushes it to the recent list).
    pub fn accept(&self) -> Option<(String, u64)> {
        if self.resolved == 0 && !self.last_ok {
            return None;
        }
        Some((self.formula(), self.resolved))
    }
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{GotoAddressDialog, GotoEvent};

#[cfg(feature = "ui")]
mod view {
    use super::{GotoState, GotoStatus};
    use crate::addr::AddressParserCallbacks;
    use crate::ui::design::{color, tokens};
    use crate::ui::dialogs::modal;
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::input::{Input, InputEvent, InputState};
    use gpui_component::{ActiveTheme, Disableable as _};

    /// The "Base Address" help table (PIC5) — example expression on the left, the
    /// addressing mode it demonstrates on the right. The C++ goto dialog shows the
    /// same rich-text legend so the user knows the accepted expression forms.
    const ADDRESS_FORMS: [(&str, &str); 5] = [
        ("0x7FF61234ABCD", "hex address"),
        ("<app.exe>", "module base"),
        ("<app.exe> + 0x1A0", "module + offset"),
        ("[<app.exe> + 0x58]", "follow pointer"),
        ("ntdll!SymbolName", "PDB symbol"),
    ];

    /// The dialog's outcome, raised to the host (the C++ `accept`/`reject`).
    #[derive(Clone, Debug)]
    pub enum GotoEvent {
        /// Go pressed / Enter on a resolved formula: `(formula, resolved_addr)`.
        Go(String, u64),
        /// Cancel / Esc.
        Cancel,
    }

    /// The Goto-Address dialog view — input + live status + recent list.
    ///
    /// Evaluation needs the live module/symbol callbacks (provided by the host
    /// controller); for a parity-faithful but self-contained view this holds the
    /// pointer size and re-evaluates with no callbacks (pure syntax + literals)
    /// when none are wired, exactly as [`AddressParser::evaluate(.., None)`] does.
    pub struct GotoAddressDialog {
        input: Entity<InputState>,
        state: GotoState,
        recent: Vec<String>,
        /// The keyboard-highlighted recent row (the C++ `m_recentList`
        /// `currentRow`); `None` until focus moves into the list (Down from the
        /// input) or a row is hovered.
        recent_selected: Option<usize>,
        /// Set while [`pick_recent`](Self::pick_recent) programmatically writes the
        /// input so the resulting `Change` event does not clear the recent
        /// highlight (only genuine user typing clears it).
        suppress_recent_clear: bool,
        ptr_size: i32,
        focus_handle: FocusHandle,
        _subscriptions: Vec<Subscription>,
    }

    impl GotoAddressDialog {
        /// Build the dialog seeded with the recent list (most-recent-first).
        pub fn new(
            recent: Vec<String>,
            ptr_size: i32,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("0x..."));
            // Re-evaluate on every keystroke (the C++ `textChanged` connection),
            // and run Go on Enter when the formula resolves (the C++
            // `m_input` `returnPressed` → `accept()` connection).
            let subscription = cx.subscribe_in(
                &input,
                window,
                |this, _input, ev: &InputEvent, _window, cx| match ev {
                    InputEvent::Change => {
                        // Genuine user typing detaches from the recent list (the
                        // highlight no longer reflects the input); a programmatic
                        // recent-pick suppresses this so the highlight persists.
                        if this.suppress_recent_clear {
                            this.suppress_recent_clear = false;
                        } else {
                            this.recent_selected = None;
                        }
                        this.on_text_changed(cx);
                    }
                    InputEvent::PressEnter { .. } => this.confirm(cx),
                    _ => {}
                },
            );
            GotoAddressDialog {
                input,
                state: GotoState::new(),
                recent,
                recent_selected: None,
                suppress_recent_clear: false,
                ptr_size,
                focus_handle: cx.focus_handle(),
                _subscriptions: vec![subscription],
            }
        }

        /// Read-only access to the validation reducer (for tests / wiring).
        pub fn state(&self) -> &GotoState {
            &self.state
        }

        /// The recent entries shown in the list.
        pub fn recent(&self) -> &[String] {
            &self.recent
        }

        fn on_text_changed(&mut self, cx: &mut Context<Self>) {
            let text = self.input.read(cx).value().to_string();
            // No callbacks wired here → syntax + literal evaluation (the parser
            // resolves modules/derefs to 0 in this mode); the host swaps in live
            // callbacks when attaching a provider.
            self.state
                .evaluate(&text, self.ptr_size, None::<&AddressParserCallbacks<'_>>);
            cx.notify();
        }

        /// Commit if the formula resolved (the Go button / Enter path).
        pub fn confirm(&mut self, cx: &mut Context<Self>) {
            if let Some((formula, addr)) = self.state.accept() {
                cx.emit(GotoEvent::Go(formula, addr));
            }
        }

        /// Cancel the dialog (Esc / Cancel button).
        pub fn cancel(&mut self, cx: &mut Context<Self>) {
            cx.emit(GotoEvent::Cancel);
        }

        /// Copy a recent entry into the input and re-evaluate
        /// (`m_recentList currentTextChanged` → `m_input->setText(text)`). Does not
        /// commit — the C++ only fills the input on a selection change.
        fn pick_recent(&mut self, entry: String, window: &mut Window, cx: &mut Context<Self>) {
            // The set_value below emits a `Change`; mark it as programmatic so the
            // subscription does not clear the recent highlight.
            self.suppress_recent_clear = true;
            self.input
                .update(cx, |s, cx| s.set_value(entry, window, cx));
            self.on_text_changed(cx);
        }

        /// Select recent row `ix` (clamped), copy its text into the input, and
        /// re-evaluate — the C++ `setCurrentRow` + `currentTextChanged` pair.
        fn select_recent(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
            if let Some(entry) = self.recent.get(ix).cloned() {
                self.recent_selected = Some(ix);
                self.pick_recent(entry, window, cx);
                cx.notify();
            }
        }

        /// Activate the highlighted recent entry — the C++ `itemActivated`
        /// (Enter / double-click): fill the input from the item then `accept()` if
        /// the formula resolves. With no highlighted row this is a plain confirm.
        fn confirm_recent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            if let Some(ix) = self.recent_selected {
                if let Some(entry) = self.recent.get(ix).cloned() {
                    self.pick_recent(entry, window, cx);
                }
            }
            self.confirm(cx);
        }

        /// Capture-phase key handling (the C++ `keyPressEvent` override + the
        /// recent-list's own navigation): Escape cancels; Down from the input moves
        /// focus into the recent list (selecting row 0); Up/Down then walk the
        /// recents (clamped); Enter activates the highlighted recent (or Go). Returns
        /// `true` when handled so the caller stops propagation.
        fn handle_nav_key(
            &mut self,
            key: &str,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> bool {
            match key {
                "escape" => {
                    self.cancel(cx);
                    true
                }
                "down" => {
                    if self.recent.is_empty() {
                        return false;
                    }
                    let next =
                        super::step_recent_selection(self.recent_selected, 1, self.recent.len());
                    if let Some(ix) = next {
                        self.select_recent(ix, window, cx);
                    }
                    true
                }
                "up" => {
                    // Up only navigates once focus is in the recent list; before
                    // that it stays in the input (the C++ list owns Up).
                    if self.recent.is_empty() || self.recent_selected.is_none() {
                        return false;
                    }
                    let next =
                        super::step_recent_selection(self.recent_selected, -1, self.recent.len());
                    if let Some(ix) = next {
                        self.select_recent(ix, window, cx);
                    }
                    true
                }
                "enter" => {
                    // Enter on a highlighted recent activates it; otherwise the
                    // input's own returnPressed handles Go (let it through).
                    if self.recent_selected.is_some() {
                        self.confirm_recent(window, cx);
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            }
        }

        /// The "Base Address" help legend (PIC5): a titled card listing the
        /// accepted address-expression forms (example → mode) plus the operator
        /// hint, styled like a Zed popover section.
        fn render_help(&self, cx: &Context<Self>) -> impl IntoElement {
            let mono = SharedString::from(tokens::font::mono_family());
            let rows = ADDRESS_FORMS.into_iter().map(|(example, meaning)| {
                gpui_component::h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(px(tokens::space::LG))
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::accent(cx))
                            .child(example),
                    )
                    .child(
                        div()
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(color::text_muted(cx))
                            .child(meaning),
                    )
            });

            gpui_component::v_flex()
                .w_full()
                .p(px(tokens::space::LG))
                .gap(px(tokens::space::SM))
                .rounded(px(tokens::radius::LG))
                .border_1()
                .border_color(color::border(cx))
                .bg(color::panel_bg(cx))
                .child(
                    div()
                        .text_size(px(tokens::font::UI_SM))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(color::text(cx))
                        .child("Base Address"),
                )
                .children(rows)
                .child(
                    div()
                        .pt(px(tokens::space::XS))
                        .text_size(px(tokens::font::UI_XS))
                        .text_color(color::text_muted(cx))
                        .child("Operators: + - * << >> & | ^  ·  all numbers are hexadecimal"),
                )
        }
    }

    impl Focusable for GotoAddressDialog {
        /// Return the ADDRESS INPUT's focus handle so the host's
        /// `window.focus(dialog.focus_handle)` on open lands keystrokes in the
        /// field — opening the dialog and immediately typing an address (the C++
        /// `m_input->setFocus()` in the constructor). Without this the dialog card
        /// (`track_focus(&self.focus_handle)`) holds focus and typing does nothing
        /// (the dead-input failure mode the cross-cutting note flags). The
        /// `RcxGotoAddress` `key_context` + capture-phase key handler still receive
        /// Escape / Down-into-recents / Up-Down / Enter because the focused input is
        /// a descendant of the `track_focus` card.
        fn focus_handle(&self, cx: &App) -> FocusHandle {
            self.input.read(cx).focus_handle(cx)
        }
    }

    impl EventEmitter<GotoEvent> for GotoAddressDialog {}

    impl Render for GotoAddressDialog {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::button::{Button, ButtonVariants as _};

            let can_go = self.state.can_go();
            // Clamp the 460-wide card to the window so it stays fully on-screen on a
            // narrow window (QA #1; the dialog layer centers with no edge clamp).
            let card_w = modal::clamp_width(460., window);
            let card_max_h = modal::clamp_height(560., 120., window);
            let mono = SharedString::from(tokens::font::mono_family());
            let (status_text, status_color) = match self.state.status() {
                GotoStatus::Idle => (" ".to_string(), color::text_muted(cx)),
                GotoStatus::Resolved(v) => (format!("\u{2192} 0x{v:x}"), cx.theme().success),
                GotoStatus::Error(e) => (e, cx.theme().danger),
            };

            // The recent entries as compact Zed list-rows (mono, selectable). The
            // keyboard-highlighted row gets the selected fill; hovering selects a
            // row (mouse + keyboard stay in sync), and a click selects-then-confirms
            // (the C++ `itemActivated`).
            let recent_selected = self.recent_selected;
            let recent_rows: Vec<AnyElement> = self
                .recent
                .iter()
                .enumerate()
                .map(|(ix, entry)| {
                    let entry = entry.clone();
                    let selected = recent_selected == Some(ix);
                    crate::ui::design::zed_list_row(
                        SharedString::from(format!("goto-recent-{ix}-{entry}")),
                        selected,
                        cx,
                    )
                    .font_family(mono.clone())
                    .text_size(px(tokens::font::UI_SM))
                    .cursor_pointer()
                    .on_mouse_move(cx.listener(move |this, _e, _window, cx| {
                        if this.recent_selected != Some(ix) {
                            this.recent_selected = Some(ix);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _e, window, cx| {
                        this.select_recent(ix, window, cx);
                        this.confirm_recent(window, cx);
                    }))
                    .child(entry)
                    .into_any_element()
                })
                .collect();

            let help = self.render_help(cx);

            let body = modal::body(cx)
                .child(modal::help_text(
                    "Enter an absolute address or an expression to resolve, \
                     then press Go.",
                    cx,
                ))
                .child(modal::field_label("Address", cx))
                .child(Input::new(&self.input).w_full().font_family(mono.clone()))
                .child(
                    div()
                        .h(px(18.))
                        .font_family(mono.clone())
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(status_color)
                        .child(status_text),
                )
                .child(help)
                .when(!self.recent.is_empty(), |b| {
                    b.child(crate::ui::design::section_label("Recent", cx))
                        .child(
                            gpui_component::v_flex()
                                .gap(px(tokens::space::XXS))
                                .children(recent_rows),
                        )
                });

            let footer = modal::footer(cx)
                .child(
                    Button::new("goto-cancel")
                        .label("Cancel")
                        .on_click(cx.listener(|this, _e, _window, cx| this.cancel(cx))),
                )
                .child(
                    Button::new("goto-go")
                        .primary()
                        .label("Go")
                        .when(!can_go, |b| b.disabled(true))
                        .on_click(cx.listener(|this, _e, _window, cx| this.confirm(cx))),
                );

            modal::card(cx)
                .id("rcx-goto-address")
                .track_focus(&self.focus_handle)
                .key_context("RcxGotoAddress")
                // Capture-phase key handling so Escape/Down-into-recents/Up-Down/
                // Enter reach the dialog even while the input owns focus (the C++
                // `keyPressEvent` override + the recent-list navigation).
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    if this.handle_nav_key(ev.keystroke.key.as_str(), window, cx) {
                        cx.stop_propagation();
                    }
                }))
                .w(card_w)
                .max_h(card_max_h)
                .child(
                    modal::header_with_close("Go to Address", "goto-close", cx.listener(|this, _e, _window, cx| this.cancel(cx)), cx),
                )
                .child(body)
                .child(footer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        clear_recent, load_recent, push_recent, push_recent_list, step_recent_selection, GotoState,
        GotoStatus, MAX_RECENT, RECENT_KEY,
    };
    use crate::addr::AddressParserCallbacks;
    use crate::theme::manager::MemSettings;

    // ── recent-list reducers (test_goto_address.cpp) ──

    #[test]
    fn push_recent_most_recent_first() {
        let mut store = MemSettings::new();
        clear_recent(&mut store);
        push_recent(&mut store, "0xAA");
        push_recent(&mut store, "0xBB");
        push_recent(&mut store, "0xCC");
        let recent = load_recent(&store);
        assert_eq!(recent, vec!["0xCC", "0xBB", "0xAA"]);
    }

    #[test]
    fn push_recent_dedup_moves_to_top() {
        let mut store = MemSettings::new();
        clear_recent(&mut store);
        push_recent(&mut store, "0xAA");
        push_recent(&mut store, "0xBB");
        push_recent(&mut store, "0xAA"); // dup → moves to top
        let recent = load_recent(&store);
        assert_eq!(recent, vec!["0xAA", "0xBB"]);
    }

    #[test]
    fn push_recent_caps_at_max() {
        let mut store = MemSettings::new();
        clear_recent(&mut store);
        for i in 0..20u64 {
            push_recent(&mut store, &format!("0x{i:x}"));
        }
        let recent = load_recent(&store);
        assert_eq!(recent.len(), MAX_RECENT);
        // The most recent (0x13) is first; the oldest kept is 0x13-11 = 0x8.
        assert_eq!(recent[0], format!("0x{:x}", 19));
    }

    #[test]
    fn push_recent_ignores_empty() {
        let base = vec!["0xAA".to_string()];
        assert_eq!(push_recent_list(&base, "   "), base);
        assert_eq!(push_recent_list(&base, ""), base);
        // Trims before storing.
        assert_eq!(push_recent_list(&base, "  0xBB  "), vec!["0xBB", "0xAA"]);
    }

    #[test]
    fn clear_recent_empties_the_list() {
        let mut store = MemSettings::new();
        push_recent(&mut store, "0xAA");
        clear_recent(&mut store);
        assert!(load_recent(&store).is_empty());
    }

    #[test]
    fn recent_key_is_stable() {
        assert_eq!(RECENT_KEY, "gotoAddress/recent");
    }

    // ── live-validation reducer (test_goto_address.cpp) ──

    #[test]
    fn empty_input_disables_go() {
        let mut s = GotoState::new();
        assert!(!s.can_go());
        s.evaluate("", 8, None::<&AddressParserCallbacks<'_>>);
        assert!(!s.can_go());
        assert_eq!(s.status(), GotoStatus::Idle);
        // accept() refuses to "go nowhere".
        assert_eq!(s.accept(), None);
    }

    #[test]
    fn hex_literal_resolves_and_enables_go() {
        let mut s = GotoState::new();
        let st = s.evaluate("0x1234", 8, None::<&AddressParserCallbacks<'_>>);
        assert_eq!(st, GotoStatus::Resolved(0x1234));
        assert!(s.can_go());
        assert_eq!(s.resolved(), 0x1234);
        assert_eq!(s.accept(), Some(("0x1234".to_string(), 0x1234)));
    }

    #[test]
    fn nonsense_disables_go() {
        let mut s = GotoState::new();
        let st = s.evaluate("xyz nonsense", 8, None::<&AddressParserCallbacks<'_>>);
        assert!(matches!(st, GotoStatus::Error(_)));
        assert!(!s.can_go());
        assert_eq!(s.resolved(), 0);
    }

    #[test]
    fn module_relative_resolves_with_callbacks() {
        // <game.exe> + 0x40 with game.exe → 0x140000000 resolves to 0x140000040.
        let cbs = AddressParserCallbacks {
            resolve_module: Some(Box::new(|name: &str| {
                if name == "game.exe" {
                    (0x140000000, true)
                } else {
                    (0, false)
                }
            })),
            ..Default::default()
        };
        let mut s = GotoState::new();
        let st = s.evaluate("<game.exe> + 0x40", 8, Some(&cbs));
        assert_eq!(st, GotoStatus::Resolved(0x140000040));
        assert!(s.can_go());
        assert_eq!(s.resolved(), 0x140000040);
    }

    #[test]
    fn formula_is_trimmed() {
        let mut s = GotoState::new();
        s.evaluate("  0xAB  ", 8, None::<&AddressParserCallbacks<'_>>);
        assert_eq!(s.formula(), "0xAB");
    }

    // ── recent-list keyboard navigation (the C++ keyPressEvent Down-into-list +
    //    QListWidget Up/Down clamping) ──

    #[test]
    fn step_recent_selection_empty_is_none() {
        assert_eq!(step_recent_selection(None, 1, 0), None);
        assert_eq!(step_recent_selection(Some(0), -1, 0), None);
    }

    #[test]
    fn step_recent_selection_down_from_input_lands_on_first() {
        // Down with no current row → row 0 (focus moves into the list).
        assert_eq!(step_recent_selection(None, 1, 3), Some(0));
    }

    #[test]
    fn step_recent_selection_up_from_input_lands_on_last() {
        assert_eq!(step_recent_selection(None, -1, 3), Some(2));
    }

    #[test]
    fn step_recent_selection_clamps_without_wrapping() {
        // Down past the end stays on the last row (Qt setCurrentRow clamps).
        assert_eq!(step_recent_selection(Some(2), 1, 3), Some(2));
        // Up past the start stays on the first row.
        assert_eq!(step_recent_selection(Some(0), -1, 3), Some(0));
        // Mid-list moves normally.
        assert_eq!(step_recent_selection(Some(1), 1, 3), Some(2));
        assert_eq!(step_recent_selection(Some(1), -1, 3), Some(0));
    }
}
