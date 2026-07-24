//! Tooltips & hover previews — the arrow tooltip placement + the global
//! hover-state owner (`rcxtooltip.h`, `tooltip_bridge.h`, `hover_preview.h`;
//! widgets-dialogs.md §15,§20).
//!
//! Port of `RcxTooltip` + `GlobalTooltipBridge`. The C++ paints a custom rounded
//! tooltip with a triangular arrow whose tip touches the anchor, and installs an
//! app-wide event filter that replaces Qt's default tooltip with a single shared
//! `RcxTooltip` — tracking *which widget owns the visible tip* and only dismissing
//! on that widget's leave / focus-loss / click / window-deactivate (the "narrow
//! dismissal" that fixes per-tick flicker). Per the cookbook (ARCHITECTURE §5) the
//! rendering maps onto gpui-component's `Tooltip` / `HoverCard`; this module ports
//! the **placement geometry** (`showAt`) and the **hover-state ownership**
//! (`GlobalTooltipBridge`) — the parts the C++ tests lock (`test_tooltip_event`,
//! `test_tooltip_flicker`) — as gpui-free, unit-tested logic.
//!
//! Gated behind the `ui` feature only for the hover-preview trait helpers; the
//! placement + ownership logic is always built/tested.

/// `RcxTooltip` geometry constants (`rcxtooltip.h:37`).
pub const ARROW_H: f32 = 8.0;
pub const ARROW_W: f32 = 14.0;
pub const RADIUS: f32 = 0.0;
pub const PAD: f32 = 10.0;
pub const GAP: f32 = 4.0;
pub const MAX_W: f32 = 550.0;

/// Whether the tooltip body is placed above or below the anchor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArrowSide {
    /// Body below the anchor; the arrow points up, tip at `anchor.y` →
    /// `body.y == anchor.y` (`test_tooltip_event`: arrow-below).
    Below,
    /// Body above the anchor; the arrow points down, tip at `anchor.y` →
    /// `body.y + body.height == anchor.y` (`test_tooltip_event`: arrow-above).
    Above,
}

/// The resolved tooltip frame: top-left position + arrow side + arrow x within the
/// screen (the recomputed `m_ax` so the tip stays over the anchor).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TooltipFrame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub side: ArrowSide,
    /// Arrow tip x (screen coords), clamped within the body.
    pub arrow_x: f32,
}

/// Choose the tooltip placement for an anchor point (`showAt`,
/// `rcxtooltip.h:118`).
///
/// `prefer_above`: when true, place above unless there's no room above (then
/// below); when false (legacy), place below if it fits, else above. The body is
/// horizontally bounded to `[0, screen_w]`, and the arrow x is recomputed so the
/// triangle's tip stays over the anchor (clamped within the body span).
pub fn place_tooltip(
    anchor_x: f32,
    anchor_y: f32,
    width: f32,
    height: f32,
    screen_w: f32,
    screen_h: f32,
    prefer_above: bool,
) -> TooltipFrame {
    let room_above = anchor_y - height - ARROW_H >= 0.0;
    let room_below = anchor_y + height + ARROW_H <= screen_h;

    let side = if prefer_above {
        if room_above {
            ArrowSide::Above
        } else {
            ArrowSide::Below
        }
    } else {
        // Legacy: below if it fits, else above.
        if room_below {
            ArrowSide::Below
        } else {
            ArrowSide::Above
        }
    };

    // Vertical position so the arrow tip touches the anchor.
    let y = match side {
        // Body below: arrow points up; tip at anchor.y → body top = anchor.y.
        ArrowSide::Below => anchor_y,
        // Body above: arrow points down; tip at anchor.y → body bottom = anchor.y.
        ArrowSide::Above => anchor_y - height,
    };

    // Horizontal: center on the anchor, then clamp to the screen.
    let mut x = anchor_x - width / 2.0;
    if x < 0.0 {
        x = 0.0;
    }
    if x + width > screen_w {
        x = (screen_w - width).max(0.0);
    }

    // Arrow x: over the anchor, clamped within the body (with a small inset so the
    // rounded corners don't clip the triangle).
    let inset = RADIUS + ARROW_W / 2.0;
    let mut arrow_x = anchor_x;
    let min_ax = x + inset;
    let max_ax = x + width - inset;
    if max_ax >= min_ax {
        arrow_x = arrow_x.clamp(min_ax, max_ax);
    } else {
        arrow_x = x + width / 2.0;
    }

    TooltipFrame {
        x,
        y,
        width,
        height,
        side,
        arrow_x,
    }
}

/// A stable identifier for the element that currently owns the visible tooltip
/// (the C++ `m_target` `QPointer<QWidget>` → a logical owner key here).
pub type TipOwner = u64;

/// The app-wide hover-state owner (`GlobalTooltipBridge`, `tooltip_bridge.h:24`).
///
/// Tracks which element owns the visible tip + the last text, and applies the
/// **narrow dismissal** rule so a stationary mouse over the same element doesn't
/// re-show (idempotent guard) and a child element's leave doesn't kill the tip.
/// This is the flicker fix the `test_tooltip_flicker` stream guards (1 show / 0
/// hide over a stationary stream).
#[derive(Clone, Debug, Default)]
pub struct HoverState {
    target: Option<TipOwner>,
    last_text: String,
    visible: bool,
}

/// What a hover/leave/etc. event resolves to (drives the view: show / no-op /
/// hide).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TipAction {
    /// Show the tip for `owner` with `text` (a new/changed hover).
    Show { owner: TipOwner, text: String },
    /// Do nothing (idempotent: same owner + text + already visible).
    NoOp,
    /// Hide the tip.
    Hide,
}

impl HoverState {
    /// A fresh hover state (nothing shown).
    pub fn new() -> Self {
        HoverState::default()
    }

    /// Whether a tip is currently shown.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// The current tip owner, if any (`tooltipTarget()` test accessor).
    pub fn target(&self) -> Option<TipOwner> {
        self.target
    }

    /// Handle a hover over `owner` carrying `text` (`QEvent::ToolTip`,
    /// `tooltip_bridge.h`):
    /// - empty text → clear + hide (return [`TipAction::Hide`]);
    /// - **idempotent guard**: same owner + same text + already visible → no
    ///   re-show ([`TipAction::NoOp`]) — the flicker fix;
    /// - else set the new owner/text + show ([`TipAction::Show`]).
    pub fn on_hover(&mut self, owner: TipOwner, text: &str) -> TipAction {
        if text.is_empty() {
            self.target = None;
            self.last_text.clear();
            self.visible = false;
            return TipAction::Hide;
        }
        if self.visible && self.target == Some(owner) && self.last_text == text {
            // Same widget + same text + already visible → suppress the re-show.
            return TipAction::NoOp;
        }
        self.target = Some(owner);
        self.last_text = text.to_string();
        self.visible = true;
        TipAction::Show {
            owner,
            text: text.to_string(),
        }
    }

    /// A `Leave` event from `owner` (`tooltip_bridge.h`): dismiss **only** if
    /// `owner` is the current target (so a child element's Leave doesn't kill the
    /// tip). Returns [`TipAction::Hide`] iff it dismissed.
    pub fn on_leave(&mut self, owner: TipOwner) -> TipAction {
        if self.target == Some(owner) {
            self.visible = false;
            // Keep the target/text so a re-enter of the same owner can re-show.
            TipAction::Hide
        } else {
            TipAction::NoOp
        }
    }

    /// A `MouseButtonPress` anywhere: dismiss but keep the target/text (the C++
    /// dismisses on press but does not clear the target).
    pub fn on_mouse_press(&mut self) -> TipAction {
        if self.visible {
            self.visible = false;
            TipAction::Hide
        } else {
            TipAction::NoOp
        }
    }

    /// A `WindowDeactivate` / `FocusOut`: clear the target + dismiss (the broad
    /// dismissal — losing focus always tears the tip down).
    pub fn on_window_deactivate(&mut self) -> TipAction {
        self.target = None;
        self.last_text.clear();
        let was = self.visible;
        self.visible = false;
        if was {
            TipAction::Hide
        } else {
            TipAction::NoOp
        }
    }
}

// ── hover-preview registry (HoverPreviewRegistry) ────────────────────────────

/// A pluggable hover-preview (`class HoverPreview`, `hover_preview.h:52`).
///
/// The editor's hover host shows one of N preview views of the hovered row.
/// Adding a view = one [`HoverPreview`] impl + registering it. The concrete
/// previews live in the editor subsystem; this is the trait + registry seam
/// (the host owns dwell timing, anchor, last-pick persistence, Tab cycling).
pub trait HoverPreview {
    /// A stable id (settings key for the last-picked preview).
    fn id(&self) -> &str;
    /// The tab label shown for this preview.
    fn tab_label(&self) -> &str;
    /// Whether this preview is eligible for the hovered row (cheap; every tick).
    fn eligible(&self, line_kind: crate::core::LineKind) -> bool;
}

/// The hover-preview registry (`class HoverPreviewRegistry`,
/// `hover_preview.h:92`): registration order is the default tie-break.
#[derive(Default)]
pub struct HoverPreviewRegistry {
    previews: Vec<Box<dyn HoverPreview>>,
}

impl HoverPreviewRegistry {
    /// A fresh, empty registry.
    pub fn new() -> Self {
        HoverPreviewRegistry {
            previews: Vec::new(),
        }
    }

    /// Register a preview (takes ownership; `add`).
    pub fn add(&mut self, preview: Box<dyn HoverPreview>) {
        self.previews.push(preview);
    }

    /// The number of registered previews (`size`).
    pub fn len(&self) -> usize {
        self.previews.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.previews.is_empty()
    }

    /// The previews eligible for the hovered row, in registration order
    /// (`eligibleFor`).
    pub fn eligible_for(&self, line_kind: crate::core::LineKind) -> Vec<&dyn HoverPreview> {
        self.previews
            .iter()
            .filter(|p| p.eligible(line_kind))
            .map(|p| p.as_ref())
            .collect()
    }
}

// ── gpui hover-popover view (feature-gated) ──────────────────────────────────

/// The structured content of a hover popover (`RcxTooltip` body): a title plus a
/// body that is either free text, a two-column "value → description" help table
/// (PIC5 "Base Address"), or a monospace value/time history list (PIC1 "Previous
/// Values"). gpui-free so it can be unit-tested and assembled headlessly.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TooltipBody {
    /// Plain wrapped body text.
    Text(String),
    /// A two-column help table: each `(example, description)` pair (the example
    /// column is monospace).
    HelpTable(Vec<(String, String)>),
    /// A history list: each `(value, when)` pair (the value column is monospace,
    /// the relative-time hint is muted).
    History(Vec<(String, String)>),
}

/// A complete hover popover: an optional title + a [`TooltipBody`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TooltipContent {
    pub title: Option<String>,
    pub body: TooltipBody,
}

impl TooltipContent {
    /// A plain text tip (no title).
    pub fn text(text: &str) -> Self {
        TooltipContent {
            title: None,
            body: TooltipBody::Text(text.to_string()),
        }
    }

    /// The "Base Address" help popover (PIC5): a titled two-column table plus a
    /// trailing operators/notes line folded into the table-following text.
    pub fn help(title: &str, rows: Vec<(String, String)>) -> Self {
        TooltipContent {
            title: Some(title.to_string()),
            body: TooltipBody::HelpTable(rows),
        }
    }

    /// The "Previous Values" history popover (PIC1): a titled value/time list.
    pub fn history(title: &str, rows: Vec<(String, String)>) -> Self {
        TooltipContent {
            title: Some(title.to_string()),
            body: TooltipBody::History(rows),
        }
    }
}

#[cfg(feature = "ui")]
pub use view::render_tooltip;

#[cfg(feature = "ui")]
mod view {
    use super::{TooltipBody, TooltipContent, MAX_W, PAD};
    use crate::ui::design::{color, tokens};
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;

    /// Build the Zed hover-popover element for a [`TooltipContent`]: an elevated
    /// surface (popover bg, 1px border, `MD` radius, soft shadow) with a semibold
    /// title and a body laid out per its kind. Monospace is used for the value
    /// columns (the C++ tip paints addresses in the editor mono font).
    ///
    /// Returned as an `AnyElement` so the hover host can anchor it via the overlay
    /// layer at the geometry from [`place_tooltip`](super::place_tooltip).
    pub fn render_tooltip(content: &TooltipContent, cx: &App) -> AnyElement {
        let title: Option<AnyElement> = content.title.as_ref().map(|t| {
            div()
                .text_size(px(tokens::font::UI_MD))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color::text(cx))
                .child(t.clone())
                .into_any_element()
        });

        let body: AnyElement = match &content.body {
            TooltipBody::Text(text) => div()
                .text_size(px(tokens::font::UI_SM))
                .text_color(color::text_muted(cx))
                .child(text.clone())
                .into_any_element(),
            TooltipBody::HelpTable(rows) => gpui_component::v_flex()
                .gap(px(tokens::space::XS))
                .children(rows.iter().map(|(example, desc)| {
                    gpui_component::h_flex()
                        .w_full()
                        .gap(px(tokens::space::XL))
                        .items_baseline()
                        .child(
                            div()
                                .flex_none()
                                .min_w(px(150.))
                                .font_family(tokens::font::mono_family())
                                .text_size(px(tokens::font::EDITOR_SIZE))
                                .text_color(color::text(cx))
                                .child(example.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(tokens::font::UI_SM))
                                .text_color(color::text_muted(cx))
                                .child(desc.clone()),
                        )
                }))
                .into_any_element(),
            TooltipBody::History(rows) => gpui_component::v_flex()
                .gap(px(tokens::space::XS))
                .children(rows.iter().map(|(value, when)| {
                    gpui_component::h_flex()
                        .w_full()
                        .gap(px(tokens::space::LG))
                        .items_baseline()
                        .justify_between()
                        .child(
                            div()
                                .font_family(tokens::font::mono_family())
                                .text_size(px(tokens::font::EDITOR_SIZE))
                                .text_color(color::text(cx))
                                .child(value.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(tokens::font::UI_XS))
                                .text_color(color::text_muted(cx))
                                .child(when.clone()),
                        )
                }))
                .into_any_element(),
        };

        crate::ui::design::elevated_surface(cx)
            .rounded(px(0.0))
            .max_w(px(MAX_W))
            .p(px(PAD))
            .child(
                gpui_component::v_flex()
                    .gap(px(tokens::space::MD))
                    .when_some(title, |this, t| this.child(t))
                    .child(body),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        place_tooltip, ArrowSide, HoverState, TipAction, TooltipBody, TooltipContent, ARROW_H,
    };

    // ── placement (test_tooltip_event.cpp) ──

    #[test]
    fn arrow_below_places_body_at_anchor_y() {
        // Plenty of room below (legacy: below if it fits). body.y == anchor.y.
        let f = place_tooltip(500.0, 300.0, 200.0, 80.0, 1920.0, 1080.0, false);
        assert_eq!(f.side, ArrowSide::Below);
        assert_eq!(f.y, 300.0);
    }

    #[test]
    fn arrow_above_bottom_touches_anchor_y() {
        // prefer_above with room above → above; body bottom == anchor.y.
        let f = place_tooltip(500.0, 600.0, 200.0, 80.0, 1920.0, 1080.0, true);
        assert_eq!(f.side, ArrowSide::Above);
        assert!((f.y + f.height - 600.0).abs() < 0.001);
    }

    #[test]
    fn falls_back_when_no_room() {
        // prefer_above but anchor near the top → no room above → below.
        let f = place_tooltip(500.0, ARROW_H, 200.0, 80.0, 1920.0, 1080.0, true);
        assert_eq!(f.side, ArrowSide::Below);
        // Legacy below but no room below (anchor near bottom) → above.
        let f2 = place_tooltip(500.0, 1075.0, 200.0, 80.0, 1920.0, 1080.0, false);
        assert_eq!(f2.side, ArrowSide::Above);
    }

    #[test]
    fn body_stays_within_left_screen_edge() {
        // Anchor near the left edge → body clamped to x >= 0.
        let f = place_tooltip(10.0, 300.0, 200.0, 80.0, 1920.0, 1080.0, false);
        assert!(f.x >= 0.0);
        // Arrow still points at the anchor (clamped within body).
        assert!(f.arrow_x >= f.x && f.arrow_x <= f.x + f.width);
    }

    #[test]
    fn body_stays_within_right_screen_edge() {
        // Anchor near the right edge → body clamped to fit.
        let f = place_tooltip(1910.0, 300.0, 200.0, 80.0, 1920.0, 1080.0, false);
        assert!(f.x + f.width <= 1920.0);
        assert!(f.arrow_x >= f.x && f.arrow_x <= f.x + f.width);
    }

    #[test]
    fn arrow_tracks_anchor_when_centered() {
        // A centered anchor with room → arrow x ≈ anchor x.
        let f = place_tooltip(500.0, 300.0, 200.0, 80.0, 1920.0, 1080.0, false);
        assert!((f.arrow_x - 500.0).abs() < 0.001);
    }

    // ── hover-state ownership (test_tooltip_flicker.cpp) ──

    #[test]
    fn first_hover_shows() {
        let mut s = HoverState::new();
        let a = s.on_hover(1, "tip text");
        assert_eq!(
            a,
            TipAction::Show {
                owner: 1,
                text: "tip text".to_string()
            }
        );
        assert!(s.is_visible());
        assert_eq!(s.target(), Some(1));
    }

    #[test]
    fn stationary_rehover_is_noop() {
        // The flicker fix: same owner + same text + already visible → no re-show.
        let mut s = HoverState::new();
        s.on_hover(1, "tip");
        // Drive a stream of identical hovers — exactly one show, no churn.
        for _ in 0..10 {
            assert_eq!(s.on_hover(1, "tip"), TipAction::NoOp);
        }
        assert!(s.is_visible());
    }

    #[test]
    fn changed_text_reshows() {
        let mut s = HoverState::new();
        s.on_hover(1, "tip a");
        let a = s.on_hover(1, "tip b");
        assert!(matches!(a, TipAction::Show { .. }));
    }

    #[test]
    fn empty_text_hides() {
        let mut s = HoverState::new();
        s.on_hover(1, "tip");
        assert_eq!(s.on_hover(1, ""), TipAction::Hide);
        assert!(!s.is_visible());
        assert_eq!(s.target(), None);
    }

    #[test]
    fn leave_only_dismisses_for_current_target() {
        let mut s = HoverState::new();
        s.on_hover(1, "tip");
        // A child's Leave (different owner) doesn't kill the tip.
        assert_eq!(s.on_leave(2), TipAction::NoOp);
        assert!(s.is_visible());
        // The owning element's Leave dismisses.
        assert_eq!(s.on_leave(1), TipAction::Hide);
        assert!(!s.is_visible());
    }

    #[test]
    fn mouse_press_dismisses_but_keeps_target() {
        let mut s = HoverState::new();
        s.on_hover(1, "tip");
        assert_eq!(s.on_mouse_press(), TipAction::Hide);
        assert!(!s.is_visible());
        // Target retained (the C++ keeps it so a re-show can happen).
        assert_eq!(s.target(), Some(1));
    }

    #[test]
    fn window_deactivate_clears_and_hides() {
        let mut s = HoverState::new();
        s.on_hover(1, "tip");
        assert_eq!(s.on_window_deactivate(), TipAction::Hide);
        assert!(!s.is_visible());
        assert_eq!(s.target(), None);
    }

    #[test]
    fn healthy_stream_one_show_zero_hide() {
        // test_tooltip_flicker: a stationary mouse-move stream over the same row
        // → 1 show / 0 hide (count the actions).
        let mut s = HoverState::new();
        let mut shows = 0;
        let mut hides = 0;
        for i in 0..20 {
            match s.on_hover(7, "RTTI · Comment") {
                TipAction::Show { .. } => shows += 1,
                TipAction::Hide => hides += 1,
                TipAction::NoOp => {}
            }
            let _ = i;
        }
        assert_eq!(shows, 1);
        assert_eq!(hides, 0);
    }

    // ── tooltip content (PIC5 help / PIC1 history) ──

    #[test]
    fn help_content_carries_title_and_table() {
        let c = TooltipContent::help(
            "Base Address",
            vec![
                ("0x7FF61234ABCD".into(), "hex address".into()),
                ("<app.exe>".into(), "module base".into()),
            ],
        );
        assert_eq!(c.title.as_deref(), Some("Base Address"));
        match c.body {
            TooltipBody::HelpTable(rows) => {
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[1].0, "<app.exe>");
                assert_eq!(rows[1].1, "module base");
            }
            _ => panic!("expected a help table"),
        }
    }

    #[test]
    fn history_content_carries_value_time_rows() {
        let c = TooltipContent::history(
            "Previous Values",
            vec![("0x19825945810".into(), "9s ago".into())],
        );
        assert_eq!(c.title.as_deref(), Some("Previous Values"));
        match c.body {
            TooltipBody::History(rows) => {
                assert_eq!(rows[0].0, "0x19825945810");
                assert_eq!(rows[0].1, "9s ago");
            }
            _ => panic!("expected a history list"),
        }
    }

    #[test]
    fn plain_text_content_has_no_title() {
        let c = TooltipContent::text("a hint");
        assert_eq!(c.title, None);
        assert_eq!(c.body, TooltipBody::Text("a hint".into()));
    }
}
