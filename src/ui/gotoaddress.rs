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
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::input::{Input, InputEvent, InputState};
    use gpui_component::{ActiveTheme, Disableable as _};

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
        ptr_size: i32,
        focus_handle: FocusHandle,
        _subscription: Subscription,
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
            // Re-evaluate on every keystroke (the C++ `textChanged` connection).
            let subscription = cx.subscribe_in(
                &input,
                window,
                |this, _input, ev: &InputEvent, _window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        this.on_text_changed(cx);
                    }
                },
            );
            GotoAddressDialog {
                input,
                state: GotoState::new(),
                recent,
                ptr_size,
                focus_handle: cx.focus_handle(),
                _subscription: subscription,
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

        /// Pick a recent entry into the input (`currentTextChanged`).
        fn pick_recent(&mut self, entry: String, window: &mut Window, cx: &mut Context<Self>) {
            self.input
                .update(cx, |s, cx| s.set_value(entry, window, cx));
            self.on_text_changed(cx);
        }
    }

    impl Focusable for GotoAddressDialog {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl EventEmitter<GotoEvent> for GotoAddressDialog {}

    impl Render for GotoAddressDialog {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::button::{Button, ButtonVariants as _};

            let can_go = self.state.can_go();
            let (status_text, status_color) = match self.state.status() {
                GotoStatus::Idle => (" ".to_string(), cx.theme().muted_foreground),
                GotoStatus::Resolved(v) => (format!("\u{2192} 0x{v:x}"), cx.theme().success),
                GotoStatus::Error(e) => (e, cx.theme().danger),
            };

            let recent_rows: Vec<_> = self
                .recent
                .iter()
                .map(|entry| {
                    let entry = entry.clone();
                    Button::new(SharedString::from(format!("goto-recent-{entry}")))
                        .ghost()
                        .w_full()
                        .label(entry.clone())
                        .on_click(cx.listener(move |this, _e, window, cx| {
                            this.pick_recent(entry.clone(), window, cx);
                        }))
                        .into_any_element()
                })
                .collect();

            gpui_component::v_flex()
                .id("rcx-goto-address")
                .track_focus(&self.focus_handle)
                .key_context("RcxGotoAddress")
                .gap_2()
                .p_3()
                .min_w(px(440.))
                .bg(cx.theme().background)
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "Enter an absolute address or expression \
                         (e.g. 0x7FF6..., <game.exe>+0x40, [ntdll!Ldr]):",
                        ),
                )
                .child(Input::new(&self.input).w_full())
                .child(
                    div()
                        .text_sm()
                        .font_family("monospace")
                        .text_color(status_color)
                        .child(status_text),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Recent:"),
                )
                .child(gpui_component::v_flex().gap_0p5().children(recent_rows))
                .child(
                    gpui_component::h_flex()
                        .justify_end()
                        .gap_2()
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
                        ),
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        clear_recent, load_recent, push_recent, push_recent_list, GotoState, GotoStatus,
        MAX_RECENT, RECENT_KEY,
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
}
