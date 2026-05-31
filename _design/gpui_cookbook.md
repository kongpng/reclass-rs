# GPUI Cookbook for the Reclass Rust Port

Authoritative, copy-pasteable reference for building Reclass's UI on **GPUI** (Zed's
GPU-accelerated UI framework). Every pattern here is traceable to a file:line under
`/tmp/zed_src/crates/gpui` (the Zed monorepo). **Do not trust web docs or memory — the API
changes fast.** Re-clone Zed and re-read the cited files if anything looks off.

- **Source of truth:** `/tmp/zed_src/crates/gpui` (+ `crates/gpui_platform`, `crates/editor`,
  `crates/theme`, `crates/workspace`).
- **Pinned commit (read + built here):** `09165c15dc5d1fea93604231eaf30ca4c25f1cd6`
- **gpui crate version at this commit:** `0.2.2`
- **Probe build result:** SUCCESS on Linux, nightly `cargo 1.97 (2026-04)`, in ~1m38s
  (cold). Binary: `/tmp/gpui_probe/target/debug/gpui_probe` (~331 MB debug).

> CRITICAL ARCHITECTURE NOTE (changed recently): at this commit, GPUI is **split into a
> renderer-agnostic `gpui` crate plus per-OS backend crates** (`gpui_linux`, `gpui_macos`,
> `gpui_windows`, `gpui_web`) tied together by a thin facade crate **`gpui_platform`**. The
> application entry point `application()` now lives in **`gpui_platform`, not `gpui`.** Older
> tutorials that call `gpui::App::new().run(...)` are stale. See
> `/tmp/zed_src/crates/gpui_platform/src/gpui_platform.rs:13`.

---

## 1. PROBE BUILD — proven working dependency

### 1.1 Cargo.toml (verified to compile)

```toml
[package]
name = "reclass-rs"           # or gpui_probe for the throwaway
version = "0.0.0"
edition = "2024"              # gpui's workspace uses edition 2024; 2021 also works for consumers
publish = false

[dependencies]
gpui          = { git = "https://github.com/zed-industries/zed.git", rev = "09165c15dc5d1fea93604231eaf30ca4c25f1cd6" }
gpui_platform = { git = "https://github.com/zed-industries/zed.git", rev = "09165c15dc5d1fea93604231eaf30ca4c25f1cd6" }
```

Both crates MUST be pinned to the **same rev** (they are members of the same workspace and
share path-internal deps like `collections`, `scheduler`, `http_client`). Mismatched revs
will fail to resolve. `gpui_platform` re-exports nothing you need from `gpui` except
`application()` / `headless()` / `current_platform()` — keep importing the bulk of types
from `gpui` directly.

Why a git dep and not crates.io: although `gpui` sets `publish = true`
(`crates/gpui/Cargo.toml`), the published crate lags hard and the split-platform layout is
git-only right now. Use the git dep pinned to a known-good commit.

### 1.2 Minimal working `main.rs` (verified, this exact file built)

```rust
use gpui::{
    App, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    rgb, size,
};
use gpui_platform::application;

struct HelloWorld {
    text: SharedString,
}

impl Render for HelloWorld {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex().flex_col().gap_3()
            .bg(rgb(0x505050))
            .size(px(500.0))
            .justify_center().items_center()
            .text_xl().text_color(rgb(0xffffff))
            .child(format!("Hello, {}!", &self.text))
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(500.), px(500.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| HelloWorld { text: "World".into() }),
        )
        .unwrap();
        cx.activate(true);
    });
}
```

This is line-for-line `crates/gpui/examples/hello_world.rs` minus the wasm cfg shims. The
canonical example keeps the wasm `#[cfg]` arms; copy them only if you target the web.

### 1.3 Toolchain

- Zed's `rust-toolchain.toml` pins **stable `1.95.0`** (`profile = minimal`, components
  rustfmt/clippy/rust-analyzer/rust-src; extra targets wasm32 + linux-musl for extensions
  and the remote server). See `/tmp/zed_src/rust-toolchain.toml`.
- **Our nightly `cargo 1.97 (2026-04)` built it fine** — no override needed. gpui compiles
  on both stable 1.95 and our nightly. If you hit an edition-2024 or feature-gate error,
  add a `rust-toolchain.toml` pinning `channel = "1.95.0"` to match Zed exactly. We did
  NOT need to.
- gpui consumers do NOT need the wasm/musl targets unless building for web/remote.

### 1.4 Rendering backends per OS (important for the port)

GPUI no longer has a single "Blade" backend; rendering is per-platform:

| OS | Backend crate | Renderer | Notes |
|----|---------------|----------|-------|
| Linux/FreeBSD | `gpui_linux` | **gpui_wgpu** (Vulkan via wgpu) + X11 and/or Wayland | `crates/gpui_linux/Cargo.toml:17-45` |
| macOS | `gpui_macos` | **Metal** (native, `metal` crate) | `crates/gpui_macos/Cargo.toml:51` |
| Windows | `gpui_windows` | **Native DirectX 11** (`directx_atlas.rs`, `directx_devices.rs`) — NOT wgpu | `crates/gpui_windows/src/directx_atlas.rs:5`, `directx_devices.rs:7` |
| Web | `gpui_web` | gpui_wgpu (WebGPU) | out of scope for Reclass |

`gpui_platform::current_platform()` picks the backend via `#[cfg(target_os=...)]` and is the
only place OS gating lives for app startup
(`/tmp/zed_src/crates/gpui_platform/src/gpui_platform.rs:38-62`).

### 1.5 Build prerequisites

- **Linux (compile-verified here):** Vulkan loader+headers, X11/XCB/XKB
  (`xkbcommon`, `x11rb`), Wayland (`wayland-client`, `wayland-protocols`, `xkbcommon`),
  fontconfig/freetype, plus `ashpd`/zbus for portals & a11y (AccessKit/atspi pulled in
  automatically). All were already installed; the cold build pulled ~600 crates incl. `wgpu`,
  `naga`, `resvg`, `swash`, `cosmic-text`, `accesskit_unix`. No system-lib errors.
- **Windows (PRIMARY target — cannot compile here):** uses the `windows`/`windows-core`
  crates (v0.61) + DirectX 11/DXGI/Direct3D11 + DirectWrite/Direct2D for text, AccessKit
  (`accesskit_windows`). No Vulkan/wgpu needed on Windows. Build deps:
  `windows-registry` build-dep, MSVC toolchain (`x86_64-pc-windows-msvc`). The
  `windows-manifest` feature (default-on via `gpui_platform`'s windows dep) embeds an app
  manifest using `embed-resource` (`crates/gpui/Cargo.toml` `[features] windows-manifest`,
  `[build-dependencies] embed-resource`). Keep all `gpui_windows`-touching code behind
  `#[cfg(windows)]`; for Reclass you almost never touch it directly — `gpui_platform`
  abstracts it.
- **macOS:** Metal + Cocoa; build-deps `bindgen`/`cbindgen`; font-kit fork
  (`zed-font-kit`). Behind `#[cfg(target_os = "macos")]`.

**Takeaway for the port:** Reclass app code is OS-agnostic. Only `current_platform()` (inside
`gpui_platform`) is `#[cfg]`-gated, and we never edit it. This satisfies the "OS-specific code
stays behind `#[cfg(...)]` and keeps compiling" constraint for free.

---

## 2. The GPUI mental model

### 2.1 App / Application / Window / Context

- **`Application`** (`crates/gpui/src/app.rs:140`) is the pre-launch builder. `application()`
  (the `gpui_platform` helper) constructs it with the right platform:
  `gpui::Application::with_platform(current_platform(false))`
  (`gpui_platform.rs:13`). Builder methods: `.with_assets(...)`,
  `.with_http_client(...)`, `.with_quit_mode(...)` (`app.rs:169-191`).
- **`Application::run(|cx: &mut App| { ... })`** (`app.rs:196`) starts the event loop; the
  closure runs once after launch. Inside it you open windows, bind keys, set menus, register
  global actions.
- **`App`** is the global application context (`&mut App`). It owns all entities/globals and
  is what you get inside `run`, action handlers, and observers.
- **`Context<T>`** (`crates/gpui/src/app/context.rs`) is the *view-scoped* context handed to
  `Render::render` and to entity update closures. It derefs to `App` but also knows "I am
  updating entity `T`", enabling `cx.notify()`, `cx.listener(...)`, `cx.processor(...)`,
  `cx.entity()`, `cx.focus_handle()`, `cx.new(...)`.
- **`Window`** (`crates/gpui/src/window.rs`) is the per-window paint/layout/input surface.
  Almost every render/element method takes `(&mut Window, &mut App)` or
  `(&mut Window, &mut Context<T>)`.

Opening a window:

```rust
cx.open_window(
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(900.), px(700.)), cx))),
        titlebar: Some(TitlebarOptions { title: Some("Reclass".into()), ..Default::default() }),
        focus: true,
        ..Default::default()
    },
    |window, cx| cx.new(|cx| RootView::new(window, cx)),
)?;
cx.activate(true);   // bring app to foreground
```
(`hello_world.rs:96-108`, `text.rs` for `TitlebarOptions`, `list_example.rs:163` for
`focus: true`.)

### 2.2 Entities & Views

- An **`Entity<T>`** is a reference-counted, app-owned handle to state of type `T`
  (`crates/gpui/src/app/context.rs`, `view.rs`). Create with `cx.new(|cx| T { ... })`.
- A **View** is just an `Entity<T>` where `T: Render`. There is no separate `View` type to
  implement — implement `Render` and the entity becomes renderable.
- Read state: `entity.read(cx)`. Mutate: `entity.update(cx, |state, cx| { ...; cx.notify() })`.
  `cx.notify()` marks the view dirty so it re-renders next frame.
- Weak handles: `entity.downgrade()` / `cx.weak_entity()` to avoid cycles in callbacks.
- `cx.entity()` inside a `Context<T>` returns the strong `Entity<T>` of the current view
  (used to embed `Entity<Child>` views as children — see `input.rs:632`,
  `cx.entity()` passed into `TextElement`).

### 2.3 Render / IntoElement / Element / RenderOnce

Three layers, pick the lowest you need:

1. **`Render`** (high level, stateful view): `fn render(&mut self, window, cx) -> impl IntoElement`.
   Build a tree of `div()`s. This is 90% of UI. (`hello_world.rs:13`).
2. **`RenderOnce` + `#[derive(IntoElement)]`** (stateless component): `fn render(self, window,
   cx) -> impl IntoElement`. Use for reusable widgets (a row, a cell, a button). See
   `text.rs` `Specimen`/`CharacterGrid` and `data_table.rs` `TableRow`
   (`#[derive(IntoElement)]` + `impl RenderOnce`).
3. **`Element`** (low level, full layout/paint/hit-test control): implement `request_layout`,
   `prepaint`, `paint`. This is what the Reclass editor surface needs. Trait at
   `crates/gpui/src/element.rs:50`; the three phases are documented inline
   (`element.rs:73-102`).

Element lifecycle each frame (`element.rs:8-30`):
`render()` builds the element tree → Taffy lays it out (`request_layout`) → `prepaint`
commits bounds + registers hitboxes/focus → `paint` draws + registers mouse/input handlers.
**The entire tree and all its callbacks are dropped and rebuilt every frame.** Persist state
in your `Entity<T>`, not in elements. Per-frame element scratch state goes in the element's
`RequestLayoutState`/`PrepaintState` associated types or via
`window.with_optional_element_state` (`elements/text.rs:1068`).

### 2.4 Styling (Tailwind-like flexbox via Taffy)

`div()` implements `Styled` + `InteractiveElement` + `ParentElement`. Layout is flexbox via
Taffy (`taffy = "=0.10.1"`). Helpers (all on `div.rs`/`styled.rs`):

- Flex: `.flex()`, `.flex_col()`, `.flex_row()`, `.flex_1()`, `.flex_none()`,
  `.flex_shrink_0()`, `.justify_center()`, `.justify_between()`, `.items_center()`, `.gap_2()`.
- Size: `.size_full()`, `.w_full()`, `.h_full()`, `.size(px(..))`, `.w(px(..))`, `.h(px(..))`,
  `.size_8()`, `.min_w(px(..))`, `.flex_1()`.
- Box: `.bg(rgb(0x..))` / `.bg(hsla(..))`, `.border_1()`, `.border_color(..)`,
  `.rounded_md()`, `.shadow_lg()`, `.p_4()`, `.px_2()`, `.py_1()`, `.mt_2()`,
  `.absolute()`/`.relative()`, `.top(px(..))`, `.overflow_hidden()`, `.overflow_y_scroll()`.
- Text: `.text_xl()`, `.text_sm()`, `.text_size(px(..))`, `.text_color(..)`,
  `.font_family("...")`, `.line_height(px(..))` or `relative(1.3)`, `.text_center()`,
  `.text_right()`, `.whitespace_nowrap()`, `.truncate()`, `.line_through()`,
  `.text_decoration_1()`.
- Grid: `.grid().grid_cols(3).gap_6()` (`anchor.rs:118`).
- State styles: `.hover(|s| s.bg(..))`, `.active(|s| s.opacity(0.8))`,
  `.focus(|s| s.border_3())` (`tab_stop.rs:64`).
- Conditionals (FluentBuilder, in prelude): `.when(cond, |this| ...)`,
  `.when_some(opt, |this, v| ...)`, `.map(|this| ...)` (`tab_stop.rs:101`,
  `anchor.rs:140`).

Colors: `rgb(0xRRGGBB)`, `rgba(0xRRGGBBAA)`, `hsla(h,s,l,a)`, named `gpui::red()` etc.,
`.opacity(0.5)`, `.blend(other)` (`color.rs`, used throughout examples).

---

## 3. THE CRITICAL PART — large virtualized list of styled rows with inline-editable fields

This is the Reclass editor surface (original is a QScintilla text grid). The faithful
GPUI approach combines **two proven gpui patterns**:

- **Virtualization** via `uniform_list` (fixed-height rows) — `crates/gpui/src/elements/uniform_list.rs`.
- **Custom row painting + inline editing** via a custom `Element` that shapes text with
  `TextRun`s, paints per-span colors, registers hitboxes, and installs an
  `EntityInputHandler` for the editing field — modeled exactly on `examples/input.rs` and
  Zed's real `crates/editor/src/element.rs`.

### 3.1 Virtualized rows: `uniform_list`

Signature (`uniform_list.rs:22`):
```rust
pub fn uniform_list<R: IntoElement>(
    id: impl Into<ElementId>,
    item_count: usize,
    f: impl 'static + Fn(Range<usize>, &mut Window, &mut App) -> Vec<R>,
) -> UniformList
```
It measures the FIRST item, then lays out only the visible slice at that height — O(visible),
not O(n). It works only for **uniform-height rows** (the Reclass grid is uniform line-height,
so this fits). Module doc: `uniform_list.rs:1-5`.

Canonical usage (`examples/uniform_list.rs:13-44`):
```rust
div().size_full().child(
    uniform_list(
        "reclass-rows",
        self.node_count,
        cx.processor(|this, range: Range<usize>, window, cx| {
            range.map(|ix| this.render_row(ix, window, cx)).collect::<Vec<_>>()
        }),
    )
    .h_full()
    .track_scroll(self.scroll_handle.clone()),  // see below
)
```
`cx.processor(...)` (`app/context.rs:264`) wraps a `Fn(&mut T, E, &mut Window, &mut Context<T>)`
into the `Fn(E, &mut Window, &mut App)` closure `uniform_list` wants, giving the render
callback `&mut Self` access. **Use `cx.processor` for the row builder** — it's why the example
can reach view state.

Scroll control: store a `UniformListScrollHandle` (`uniform_list.rs:84`) in your view, pass it
via `.track_scroll(handle.clone())`, and read/set offset through
`handle.0.borrow().base_handle` (`data_table.rs:259,269,279,310,367`). Use
`ScrollStrategy::{Top,Center,Bottom,Nearest}` (`uniform_list.rs:88-105`) for "scroll to node".

For **variable-height** rows (only if some Reclass nodes need it), use `list(ListState, ...)`
instead (`elements/list.rs`, `examples/list_example.rs:13`): `ListState::new(count, alignment,
overdraw_px)`, render callback `|index, window, cx| AnyElement`. `uniform_list` is preferred
for the grid because rows are uniform and it's much cheaper.

### 3.2 Per-span styled text within a row

Two options depending on whether spans are clickable text or click-to-edit fields:

**(a) Static colored spans — `StyledText::with_highlights`** (`elements/text.rs:391-446`,
`examples/text_layout.rs:130`):
```rust
use gpui::{StyledText, HighlightStyle};
StyledText::new(full_row_text)
    .with_highlights([
        (0..8,   HighlightStyle { color: Some(addr_color),  ..Default::default() }),
        (8..16,  HighlightStyle { color: Some(type_color),  ..Default::default() }),
        (16..40, HighlightStyle { color: Some(name_color),  ..Default::default() }),
    ])
```

**(b) Clickable spans with hit-testing — `InteractiveText`** (`elements/text.rs:953-1027`):
```rust
use gpui::InteractiveText;
InteractiveText::new("row-3", styled_text)
    .on_click(vec![name_range.clone(), type_range.clone()],
        cx.listener(|this, range_ix, window, cx| { /* enter edit mode for span range_ix */ }))
    .tooltip(|char_ix, window, cx| Some(make_tooltip(char_ix)))
```
`on_click` takes the byte-`Range`s that are clickable and reports which range index was
clicked, with correct hit-testing done by gpui against the shaped glyph runs
(`text.rs:994-1010`). This is the cheapest way to get "click this field" behavior on
otherwise-static rows.

### 3.3 Inline click-to-edit fields — the custom Element + EntityInputHandler pattern

When a field becomes editable (text caret, selection, IME), drop in a real editable text
element. The complete, idiomatic blueprint is **`examples/input.rs`** — study it as THE
reference; below is the structure mapped to Reclass.

**State entity** holds content + selection + cached layout (`input.rs:30-48`):
```rust
struct FieldInput {
    focus_handle: FocusHandle,
    content: SharedString,
    selected_range: Range<usize>,   // utf-8 byte offsets
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,  // IME composition
    last_layout: Option<ShapedLine>,     // cached for hit-testing/cursor
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
}
impl Focusable for FieldInput {
    fn focus_handle(&self, _: &App) -> FocusHandle { self.focus_handle.clone() }
}
```

**Editing actions** are plain methods bound via `.on_action(cx.listener(Self::left))` etc.
(`input.rs:50-160`): `left/right/select_left/select_right/select_all/home/end/backspace/
delete/paste/copy/cut/show_character_palette`. Grapheme-aware boundaries via
`unicode-segmentation` (`input.rs:243-256`). Mouse: `on_mouse_down/up/move` map x→index via
`ShapedLine::closest_index_for_x` (`input.rs:113-200`).

**IME / OS text input**: implement `EntityInputHandler` for the state
(`input.rs:282-410`): `text_for_range`, `selected_text_range`, `marked_text_range`,
`unmark_text`, `replace_text_in_range`, `replace_and_mark_text_in_range`, `bounds_for_range`,
`character_index_for_point`. UTF-16 ⇄ UTF-8 helpers included (`input.rs:208-241`). This is
required for correct IME, dead keys, and macOS character palette — **do not skip it** for a
faithful editor.

**Custom `Element` that shapes + paints + installs the handler** (`input.rs:412-560`):
```rust
struct FieldElement { input: Entity<FieldInput> }
struct PrepaintState { line: Option<ShapedLine>, cursor: Option<PaintQuad>, selection: Option<PaintQuad> }

impl Element for FieldElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;
    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static Location<'static>> { None }

    fn request_layout(&mut self, _, _, window, cx) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width  = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _, _, bounds, _, window, cx) -> PrepaintState {
        let input = self.input.read(cx);
        let style = window.text_style();
        // Build per-span TextRuns (color/underline/bg) here — THIS is where Reclass
        // applies per-field coloring within the editable line:
        let runs = vec![ TextRun { len: content.len(), font: style.font(),
            color: text_color, background_color: None, underline: None, strikethrough: None } ];
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(display_text, font_size, &runs, None);
        // cursor + selection PaintQuads via line.x_for_index(...)
        PrepaintState { line: Some(line), cursor, selection }
    }

    fn paint(&mut self, _, _, bounds, _, prepaint, window, cx) {
        let focus = self.input.read(cx).focus_handle.clone();
        // REGISTER OS INPUT HANDLER for the focused field:
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.input.clone()), cx);
        if let Some(sel) = prepaint.selection.take() { window.paint_quad(sel); }
        let line = prepaint.line.take().unwrap();
        line.paint(bounds.origin, window.line_height(), gpui::TextAlign::Left, None, window, cx).unwrap();
        if focus.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() { window.paint_quad(cursor); }
        }
        // cache layout for hit-testing next frame:
        self.input.update(cx, |input, _| { input.last_layout = Some(line); input.last_bounds = Some(bounds); });
    }
}
```

**Wiring the field into a row** (`input.rs:563-607`): the state's `Render` wraps the custom
element with `.track_focus(&self.focus_handle(cx))`, `.key_context("FieldInput")`,
`.cursor(CursorStyle::IBeam)`, all the `.on_action(cx.listener(...))`, and mouse handlers,
then `.child(FieldElement { input: cx.entity() })`.

**Key shaping/hit-test APIs you'll use** (text_system):
- `window.text_system().shape_line(text, font_size, &[TextRun], force_width) -> ShapedLine`
  (`text_system.rs:397`). `TextRun { len, font, color, background_color, underline,
  strikethrough }` (`text_system.rs:987`) — one run per colored span; consecutive identical
  runs auto-merge.
- `ShapedLine::x_for_index(byte_idx) -> Pixels` (`text_system/line_layout.rs:105`),
  `closest_index_for_x(x)` (`:75`), `index_for_x(x) -> Option` (`:58`), `width()`/`len()`
  (`text_system/line.rs:54-66`).
- `ShapedLine::paint(origin, line_height, align, align_width, window, cx)`
  (`text_system/line.rs:82`) and `paint_background(...)` (`:106`) for span background fills.
- `window.paint_quad(fill(bounds, color))` for cursor/selection rectangles
  (`window.rs:3655`, `fill` helper used in `input.rs:475`).

### 3.4 Faithful grid alternative: ONE custom Element for the whole viewport

`uniform_list` + per-row sub-elements is the recommended, lowest-risk path and gives free
virtualization. But for pixel-exact parity with the QScintilla grid (continuous selection
across rows, column rulers, a single caret that moves between fields), Zed itself renders its
*entire editor* as one custom `Element` (`crates/editor/src/element.rs`): it computes the
visible row range, shapes each visible line with `window.text_system().shape_line(...)`
(`element.rs:3027`, `1036`), paints line backgrounds then glyphs
(`paint_lines`/`paint_lines_background`, `element.rs:5548,5581`), and does its own hit-testing
against a `position_map`. **Recommendation for Reclass:** start with `uniform_list` + custom
field elements (3.1–3.3). If selection-across-rows / single-caret semantics demand it,
graduate the whole surface to one custom `Element` modeled on `editor/src/element.rs`. Both
use the same `shape_line` + `paint_quad` + hitbox primitives, so the migration is mechanical.

### 3.5 Hit-testing & mouse, low level (for the whole-surface element)

- `window.insert_hitbox(bounds, HitboxBehavior) -> Hitbox` during **prepaint**
  (`window.rs:4161`). Hitboxes are z-ordered; later (front) hitboxes occlude earlier ones.
- `window.on_mouse_event::<MouseDownEvent>(|ev, phase, window, cx| { ... })` during **paint**
  (`window.rs:4278`). Check `hitbox.is_hovered(window)` and `phase == DispatchPhase::Bubble`
  to act only when the cursor is over your area. Convert `ev.position` → row/col via your
  cached line layouts + `closest_index_for_x`.
- `window.set_cursor_style(CursorStyle::IBeam, &hitbox)` (`window.rs:3110`) for the text cursor
  over editable regions.
- `window.with_content_mask(Some(ContentMask{bounds}), |w| ...)` (`window.rs:3146`) to clip
  glyph painting to the viewport; `window.with_element_offset(scroll_offset, |w| ...)`
  (`window.rs:3163`) to implement scrolling inside a custom element.

---

## 4. Focus & keyboard tab-cycling between fields

Reclass needs Tab/Shift-Tab to cycle through the inline-editable fields in a row (and
across rows). GPUI has first-class tab-stop support — see `examples/tab_stop.rs`.

- Each focusable field owns a `FocusHandle` (`cx.focus_handle()`), configured with a tab
  order: `cx.focus_handle().tab_index(2).tab_stop(true)` (`tab_stop.rs:25-29`). `tab_stop(false)`
  removes it from the cycle (`tab_stop.rs:28`).
- Bind keys to actions and drive focus: `window.focus_next(cx)` / `window.focus_prev(cx)`
  (`tab_stop.rs:41-49`, `window.rs:1901,1912`). Bind `tab`→`Tab`, `shift-tab`→`TabPrev`
  (`tab_stop.rs:172-175`).
- Attach a handle to an element with `.track_focus(&handle)` (`tab_stop.rs:80`,
  `div.rs:696`). Style the focused state with `.focus(|s| ...)` or
  `.when(handle.is_focused(window), style_fn)` (`tab_stop.rs:64,100`).
- **Tab groups**: `div().tab_index(6).tab_group().tab_stop(false)` makes a container whose
  children have their own nested tab order `[6,1]`, `[6,2]`… (`tab_stop.rs:139-160`) — perfect
  for "tab within a row, then jump to next row".
- Explicit focus: `window.focus(&handle, cx)` (`window.rs:1862`), or
  `cx.focus_view(&entity, window)` (`context.rs`). Set initial focus in `WindowOptions{focus:
  true}` + `window.focus(...)` in the open closure (`input.rs:660-664`).
- `track_focus` + `.key_context("FieldInput")` + `.on_action(cx.listener(...))` is how a
  focused field receives editing actions (`input.rs:530-560`).

---

## 5. Actions & key bindings

- Declare actions with the `actions!` macro (`action.rs:24`):
  `actions!(reclass, [AddBytes, ChangeType, DeleteNode, ToggleHex, ...]);` — each becomes a
  unit struct `#[derive(Action)]` in namespace `reclass`. Complex actions (with fields) use
  `#[derive(Action)] #[action(namespace = reclass)]` on a struct (`action.rs:48-67`).
- Bind keys globally in `run`: `cx.bind_keys([KeyBinding::new("ctrl-shift-a", AddBytes,
  Some("ReclassEditor")), ...])` (`input.rs:643-657`, `KeyBinding::new` at
  `keymap/binding.rs:33`). The 3rd arg is the **key context** (a CSS-ish scope string);
  `None` = global.
- Handle actions on elements: `.on_action(cx.listener(Self::add_bytes))`
  (`input.rs:530`, `div.rs:411`). The element must be within a focused subtree whose
  `key_context` matches the binding (set via `.key_context("ReclassEditor")`,
  `div.rs:738`; `window.set_key_context` at `window.rs:4185`).
- App-level (menu/global) actions: `cx.on_action(|_: &Quit, cx| cx.quit())` (`input.rs:666`,
  `app.rs:2020`). Menus: `cx.set_menus(vec![Menu { name, items, .. }])` (`text.rs`,
  `app.rs:2186`); see `examples/set_menus.rs`.
- Observe raw keystrokes (e.g. status bar): `cx.observe_keystrokes(|ev, window, cx| ...)`
  (`input.rs:667`, `app.rs:1952`). `Keystroke::unparse()` for display (`input.rs:608`).

---

## 6. Theming (runtime-switchable)

GPUI has no built-in theme; you supply one via a **Global** and an accessor trait — exactly
how Zed's `theme` crate does it (`crates/theme/src/theme.rs`). Pattern to mirror in Reclass:

```rust
// 1. A struct + Global wrapper for the active theme.
#[derive(Clone)] pub struct ReclassTheme { pub bg: Hsla, pub addr_fg: Hsla, pub type_fg: Hsla, /* ... */ }
struct GlobalTheme(Arc<ReclassTheme>);
impl gpui::Global for GlobalTheme {}

// 2. An accessor trait on App (Zed: ActiveTheme, theme.rs:119-127).
pub trait ActiveTheme { fn theme(&self) -> &Arc<ReclassTheme>; }
impl ActiveTheme for App {
    fn theme(&self) -> &Arc<ReclassTheme> { &self.global::<GlobalTheme>().0 }
}

// 3. Install at startup: cx.set_global(GlobalTheme(Arc::new(light_theme())));
// 4. Read in render: let t = cx.theme(); div().bg(t.bg).text_color(t.addr_fg)
// 5. SWITCH AT RUNTIME: cx.update_global::<GlobalTheme, _>(|g, _| g.0 = Arc::new(dark_theme()));
//    then cx.refresh() / cx.notify() the root so all views re-render.
```
Cited: Global trait + `cx.set_global`/`cx.global::<T>()`/`cx.update_global` pattern is exactly
`theme.rs:115` (`cx.set_global(GlobalTheme{...})`), `theme.rs:124` (`impl ActiveTheme for App`),
`theme.rs:307` (`cx.update_global::<Self,_>(|this,_| this.theme = theme)`). The `text.rs`
example shows the same Global pattern with `GlobalTextContext`
(`examples/text.rs:39-66`, `cx.set_global(...)`, `cx.global::<...>()`).

Because every element tree is rebuilt each frame and reads `cx.theme()` fresh, swapping the
global + notifying = instant runtime theme switch. No widget needs to subscribe.

---

## 7. Multi-pane / split views, docks + tabs

Reclass shows several panes over one document (struct view, hex view, etc.). GPUI ships the
primitives; Zed builds the full workspace on top in `crates/workspace`:

- **Splits over one document:** the document is one `Entity<ReclassDocument>`. Each pane is a
  separate view `Entity<PaneView>` that holds a clone of the same document entity and renders
  its own projection. Lay panes out with flex: a vertical split is
  `div().flex().flex_row().child(pane_a).child(divider).child(pane_b)`; nest for grids.
  Because all panes share the same `Entity`, `entity.update(cx, ...) + cx.notify()` from any
  pane updates them all. (Entity-sharing model: `view.rs`, `app/context.rs`.)
- **Reference implementation:** Zed's `crates/workspace/src/pane_group.rs` (recursive split
  tree of panes with draggable dividers) and `pane.rs` (a pane = tab bar + active item). Read
  these for faithful split/resize behavior. `dock.rs` implements left/right/bottom dockable
  panels. These are app-level constructs built entirely from `div`, flex, drag handlers, and
  entities — there is no special "dock widget" in `gpui` itself.
- **Resizable dividers:** a thin `div().id("divider").on_drag_move(...)` that mutates a stored
  pane-width fraction in the view, then `cx.notify()`. (`on_drag_move` at `div.rs:315`;
  `data_table.rs` uses canvas+drag for its scrollbar thumb as a worked example of
  mouse-drag→state→repaint, `data_table.rs:323-367`.)
- **Tabs:** a tab bar is `div().flex().flex_row()` of clickable `div().id(tab_id)
  .on_click(...)`; the body renders the active item. Model state (`active_tab: usize`) in the
  pane view. (Pattern = `tab_stop.rs` buttons + `uniform_list` click handling.)

For the port, implement a minimal `PaneGroup`/`Pane`/`Dock` of our own modeled on
`crates/workspace` rather than depending on the `workspace` crate (it pulls in huge Zed-specific
deps). The building blocks above are all in `gpui` proper.

---

## 8. Popups, overlays, tooltips, context menus, modals

All "floating above content" UI uses **`deferred(anchored()…)`** — see `examples/popover.rs`
and `examples/anchor.rs`.

- **`anchored()`** (`elements/anchored.rs`) positions a child relative to an anchor corner:
  `.anchor(Anchor::TopLeft)`, `.position(point)`, `.position_mode(AnchoredPositionMode::Local)`,
  `.snap_to_window_with_margin(px(8.))` to keep it on-screen (`popover.rs:84-90`,
  `anchor.rs:140-155`).
- **`deferred(elem).priority(n)`** (`elements/deferred.rs`) paints the child in a later
  (higher) layer so it floats above siblings; nested `deferred` is supported
  (`popover.rs:79-110`). Higher `priority` = more on top.
- **Popover open/close**: store `open: bool` in the view; `.when(self.open, |this|
  this.child(deferred(anchored()...)))`; dismiss with `.on_mouse_down_out(cx.listener(|this,_,_,cx|
  { this.open=false; cx.notify(); }))` (`popover.rs:91-110`, `on_mouse_down_out` at
  `div.rs:259`).
- **Tooltips**: `.tooltip(|window, cx| cx.new(|_| MyTooltip{...}).into())` on any interactive
  element (`div.rs:614`), or `InteractiveText::tooltip(|char_ix, window, cx| ...)` for
  per-character tooltips in text (`text.rs:1021`). Hoverable tooltips: `.hoverable_tooltip(...)`
  (`div.rs:631`).
- **Context menus (right-click)**: register `.on_mouse_down(MouseButton::Right, ...)` or
  `.on_aux_click(...)` (`div.rs:122,559`), set `open_at: Option<Point<Pixels>>` in the view,
  and render a `deferred(anchored().position(pt))` menu list. Menu items = clickable
  `div().id(...).on_click(...)`. (Same deferred/anchored mechanism as popover.)
- **Modal dialogs / pickers**: a modal is just a full-window overlay:
  `deferred(div().absolute().inset_0().bg(black().opacity(0.4)).child(centered_panel)).priority(high)`.
  Capture clicks on the backdrop to dismiss. Inside, a **fuzzy picker** = a `FieldInput`
  (Section 3.3) for the query + a `uniform_list` of filtered results. Do the fuzzy filtering in
  the view's state on each keystroke (`replace_text_in_range`→recompute matches→`cx.notify()`).
  Zed's `crates/picker` + `fuzzy` crate are the reference for ranking; for the port,
  filter in your own state and render with `uniform_list` (Section 3.1). gpui has no
  dedicated picker element — it's composed from input + list + deferred overlay.

---

## 9. Text input widgets (single-line and multi-line)

The single-line editable field is fully covered by Section 3.3 (`examples/input.rs`). Notes:

- For a **simple, non-IME text box** you can skip the custom Element and use a `div` with
  `.on_key_down`/`.on_action` editing of a `SharedString`, but you LOSE IME, dead keys, system
  cursor, and the OS character palette. For faithful parity (esp. on Windows/macOS), use the
  `EntityInputHandler` + custom element pattern. `window.handle_input(&focus,
  ElementInputHandler::new(bounds, entity), cx)` is what bridges to the OS
  (`window.rs:4257`, `input.rs:512`).
- Multi-line / code-style editing: reuse the per-line shaping loop from
  `editor/src/element.rs` (Section 3.4). gpui does not ship a ready-made multi-line editor in
  the `gpui` crate; the full editor lives in `crates/editor` (Zed-specific, heavy). Port only
  the parts you need.
- Placeholder text, selection rendering, clipboard (`cx.read_from_clipboard()` /
  `cx.write_to_clipboard(ClipboardItem::new_string(...))`) all in `input.rs:139-175`.

---

## 10. Reclass-specific recipes (cheat sheet)

- **Root window**: `application().run(|cx| { install_theme(cx); bind_keys(cx);
  cx.open_window(opts, |w,cx| cx.new(|cx| Workspace::new(w,cx)))?; cx.activate(true); })`.
- **The struct grid**: `uniform_list("nodes", n, cx.processor(|this,range,w,cx|
  this.render_rows(range,w,cx)))` inside `div().size_full().overflow_hidden()`, with
  `.track_scroll(self.scroll)`. Each row = a `RenderOnce` component
  (`#[derive(IntoElement)]`) that shapes per-field-colored text and, when a field is being
  edited, embeds the `FieldElement` (Section 3.3).
- **Per-field colors** (address / offset / type / name / value): build `Vec<TextRun>` with the
  theme colors (`cx.theme()`), one run per field; pass to `shape_line` or use
  `StyledText::with_highlights` for non-editing rows.
- **Click a field to edit**: `InteractiveText::on_click(ranges, ...)` flips that row's field
  into edit mode (swap a `Render` branch to emit `FieldElement`), then
  `window.focus(&field.focus_handle, cx)`.
- **Tab between fields**: give each field's `FocusHandle` a `tab_index`; bind `tab`/`shift-tab`
  to `window.focus_next/prev` (Section 4); wrap a row's fields in `.tab_group()`.
- **Context menu on a node**: right-click handler stores position; render
  `deferred(anchored().position(pos).child(menu))`.
- **Runtime theme toggle (light/dark/custom)**: `cx.update_global::<GlobalTheme,_>(...)` then
  refresh root (Section 6).
- **Multiple views of one document**: share `Entity<ReclassDocument>` across pane views; any
  `update` + `notify` refreshes all (Section 7).

---

## 11. Gotchas / pitfalls discovered while reading the source

1. **`application()` is in `gpui_platform`, not `gpui`.** Add BOTH crates, same rev. This is
   the #1 thing stale tutorials get wrong. (`gpui_platform.rs:13`.)
2. **Element trees are rebuilt and dropped every frame.** Never store persistent state in an
   element — store it in your `Entity<T>` and read it in `prepaint`/`paint`. Per-frame caches
   go in `RequestLayoutState`/`PrepaintState` or `with_optional_element_state`. (`element.rs:8-30`.)
3. **`uniform_list` requires uniform row height** (it measures item 0 and extrapolates). Use
   `list`+`ListState` for variable heights. (`uniform_list.rs:1-5`.)
4. **Phase ordering is strict and asserted.** `insert_hitbox`/`set_focus_handle` only in
   **prepaint**; `paint_quad`/`on_mouse_event`/`handle_input`/`set_key_context` only in
   **paint** (debug-asserted, e.g. `window.rs:4257` `debug_assert_paint`). Calling in the wrong
   phase panics in debug.
5. **Text offsets are UTF-8 bytes**, but OS input handlers speak UTF-16. You MUST implement the
   utf8⇄utf16 conversions (`input.rs:208-241`) or IME breaks on non-ASCII. Use grapheme
   boundaries (`unicode-segmentation`) for cursor movement, not byte stepping.
6. **`handle_input` only registers when the focus handle is actually focused** (it checks
   `is_focused` internally, `window.rs:4264`). Make sure the field's handle is focused before
   expecting keystrokes/IME.
7. **Floating UI needs `deferred` AND a higher `priority`**, else it paints under siblings.
   `anchored()` alone does not raise the layer. (`popover.rs:79`, nested deferred at `:81`.)
8. **`cx.notify()` is required** after mutating view state or nothing re-renders. Mouse/action
   handlers that change state must call it (every example does).
9. **Editions**: gpui's workspace is edition 2024 and uses `let ... else`, `if let` chains
   (`&&`), etc. Our nightly 1.97 is fine; if you pin stable, use ≥1.95 (Zed's pin). Lower
   stable will fail on 2024 syntax.
10. **Windows is native DirectX 11, not wgpu/Blade.** Don't add wgpu deps for Windows; the
    `windows`/`windows-core` crates + MSVC handle it. The default `windows-manifest` feature
    embeds an app manifest via `embed-resource` (a build-dep). (`gpui_windows/src/directx_*.rs`,
    `gpui/Cargo.toml` features.)
11. **Heavy first build.** Cold build pulls ~600 crates (wgpu, naga, resvg, swash, cosmic-text,
    accesskit, zbus). ~1m38s here; budget more on weaker machines. Incremental rebuilds of app
    code are fast.
12. **Don't depend on `crates/workspace`/`editor`/`picker` directly** — they drag in large
    Zed-specific deps. Re-implement the small slices you need (panes, docks, pickers) from the
    `gpui` primitives, using those crates only as a reading reference.

---

## 12. File:line index of every cited pattern

- App entry / platform split: `gpui_platform/src/gpui_platform.rs:13,38-62`;
  `gpui/src/app.rs:140,146,196`.
- Hello window / styling: `gpui/examples/hello_world.rs:13,96-108`.
- Render/IntoElement/RenderOnce: `gpui/src/element.rs:50,73-102`; `gpui/examples/text.rs`
  (RenderOnce `Specimen`); `gpui/examples/data_table.rs` (`#[derive(IntoElement)]` `TableRow`).
- `uniform_list`: `gpui/src/elements/uniform_list.rs:1-5,22,84,88-105`;
  `gpui/examples/uniform_list.rs:13-44`; scroll handle usage `gpui/examples/data_table.rs:259-367`.
- `list`/`ListState`: `gpui/src/elements/list.rs`; `gpui/examples/list_example.rs:13-163`.
- `cx.processor`: `gpui/src/app/context.rs:264`.
- Styled text spans: `gpui/src/elements/text.rs:391-446,953-1027`;
  `gpui/examples/text_layout.rs:130`.
- Inline editor (custom Element + EntityInputHandler): `gpui/examples/input.rs` (entire file;
  key lines 30-48, 50-256, 282-410, 412-560, 530-607, 643-666).
- Text shaping: `gpui/src/text_system.rs:397,509,987`;
  `gpui/src/text_system/line.rs:43-130`; `gpui/src/text_system/line_layout.rs:58,75,105`.
- Custom paint primitives: `gpui/src/window.rs:3110,3146,3163,3655,3676,3763,4161,4257,4278`;
  `gpui/src/elements/canvas.rs:9-90`.
- Focus / tab cycling: `gpui/examples/tab_stop.rs:25-29,41-49,64,80,100,139-175`;
  `gpui/src/window.rs:1862,1901,1912`.
- Actions / keymap: `gpui/src/action.rs:24,48-67`; `gpui/src/keymap/binding.rs:33`;
  `gpui/src/app.rs:1952,2001,2020,2186`; `gpui/examples/input.rs:643-666`.
- Interactive element methods: `gpui/src/elements/div.rs:122,259,276,315,411,546,559,601,614,
  631,687,696,738,750`.
- Theming: `gpui/src/theme... ->` `crates/theme/src/theme.rs:115,119-127,307`;
  `gpui/examples/text.rs:39-66`.
- Panes/docks: `crates/workspace/src/pane_group.rs`, `pane.rs`, `dock.rs`.
- Popups/overlays/tooltips: `gpui/examples/popover.rs:79-110`; `gpui/examples/anchor.rs:118-155`;
  `gpui/src/elements/{anchored,deferred}.rs`.
- Editor-as-one-Element reference: `crates/editor/src/element.rs:1036,3027,5548,5581`.
