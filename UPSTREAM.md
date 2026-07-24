# Upstream provenance

`reclass-rs` is a faithful, 1:1 Rust + [GPUI](https://www.gpui.rs/) port of the
C++/Qt application **Reclass** by IChooseYou.

## The fork this port is based on

- **Upstream project:** [`IChooseYou/Reclass`](https://github.com/IChooseYou/Reclass)
  (MIT). A structured binary editor for reverse engineering — *"built from scratch
  as a modern replacement for ReClass.NET and ReClassEx"* (C++17, Qt 6,
  QScintilla; the whole editor surface is rendered as formatted plain text).
- **Lineage:** ReClass / ReClassEx / ReClass.NET → `IChooseYou/Reclass` (a
  from-scratch C++ successor — *not* a GitHub fork of those earlier tools) →
  `reclass-rs` (this Rust + GPUI port).
- Upstream is standalone on GitHub (it has no `parent` repo), so "the fork" this
  Rust project tracks is `IChooseYou/Reclass` itself.

## Base revision

The Rust port was made from the local C++ checkout at `/home/loke/reclass-cpp`,
which matches upstream:

| | |
|---|---|
| **Tag** | `snapshot-29-05-2026` |
| **Commit** | `f9171af` |
| **Subject** | `feat: IBM Plex Mono is the default font + tighten chip hover hit zone` |
| **Date** | 2026-05-29 |

The local C++ tree carries two small local modifications on top of that commit
(`src/controller.cpp`, `src/editor.cpp`); every other source file matches
`f9171af` exactly.

## Upstream tracking

The `f9171af..9f39e36` delta — **23 upstream commits** — has been audited and
ported into the Rust version. The tracked upstream point is therefore
**`9f39e36a7b867a90b4da64ff27440d768d016fb6`** (2026-06-16), even though the
original local C++ snapshot at `/home/loke/reclass-cpp` remains at `f9171af`.

### Sync from `b7baced` to `9f39e36` (2026-07-24)

The exact newly-audited range contained three commits:

| Commit | Date | Disposition |
|---|---|---|
| `be9c2b682daa891765960c2612a0678ee7c1abd2` | 2026-06-10 | All newly introduced application behavior relevant to the Rust/GPUI port was ported. |
| `61aaf0a91ceaa96156963fc16d5075850302a033` | 2026-06-16 | All editor fixes and applicable UI polish were ported. |
| `9f39e36a7b867a90b4da64ff27440d768d016fb6` | 2026-06-16 | No Rust analogue: this only adds Qt's `Q_OBJECT` marker to `ResizeEdge` so Qt 6.8 `findChildren` can discover it. GPUI's resize-edge views are statically typed Rust values and need no meta-object registration. |

The relevant behavior from those commits is covered as follows:

- **Document/view lifecycle:** a window may now have zero document tabs; closing
  the last tab reveals the hatched “No document open” surface; tab-close targets
  are resolved at click time; active-document/source/bookmark state follows the
  actual focused tab; and rendered panes refresh in Code and Both modes.
- **Both and Code views:** the primary Both surface is one live Reclass/Code
  split with a draggable 67/33 divider and shared south controls. Every pane owns
  its zoom state, continuous slider/readout, Ctrl-wheel synchronization, and Both
  divider. Generated C++, Rust, defines, C#, and Python each use format-aware
  highlighting, and the Code root follows the selected field's owning class
  before falling back to the current view root. Extra split panes retain the
  Rust port's pre-existing read-only Tree/Code projections: upstream already had
  distinct live editors per secondary pane before this delta, while the current
  GPUI editor owns its controller and cannot be mounted twice. Closing that older
  architectural gap requires separating the shared controller/document model
  from pane-local focus, scroll, hover, edit, and zoom state.
- **Selection correctness:** byte bands are reapplied after refresh, mirrored to
  row selection, and cleared by structural delete/shrink operations. Stale
  deferred clicks cannot select the row that shifted into a deleted row's old
  position. Type/hex pickers use a one-shot, selection-scoped reopen guard.
- **Break into Class:** byte selections, contiguous multi-row selections, and a
  directly clicked field all share the exact-region extraction path. Fully
  contained structs/arrays are preserved; straddling container boundaries and
  nested coordinate frames fail closed instead of moving the wrong bytes.
- **Context menus and hex toolbar:** selection IDs have one canonical decoder;
  copy/cut/address batches are sorted structurally; Copy Address emits all
  selected addresses; “Bookmark this address…” records the displayed formula;
  and multi-selection Join validates the exact same-kind contiguous run again
  when activated.
- **Compose and hints:** unreadable live values are marked and struck through;
  strings probe their first code unit; null RTTI naming is offered only for live
  targets; automatic RTTI defaults off; enum/RTTI hints live under Visual hints;
  and expensive type-hint decisions are memoized for one compose pass.
- **Source management:** the chooser is ordered as Connected, Add Source,
  providers, and Clear All; saved rows have mouse/keyboard deletion without
  closing the popup; removing active/inactive sources preserves the correct
  index and detach semantics; file switches discard stale snapshots/history;
  and copied source lists notify the document.
- **Target and symbol state:** the source chip distinguishes Live, Static,
  Stale, and Disconnected from actual read results. Symbol lookup tries the PDB
  store first, then the provider fallback.
- **Refresh performance:** page diffs skip equal machine words while preserving
  byte-identical changed ranges; executable-region sweeps are cached for 64
  refresh ticks; module snapshots persist for the provider lifetime and are
  reused by compose, RTTI, hover previews, kernel, remote, WinDbg, snapshot, and
  ReClass.NET-backed providers.
- **Plugin safety:** Rust's stable plugin ABI remains stronger than upstream's
  new `sizeof(Provider)` token, and rejected/incompatible plugin errors remain
  visible in Manage Plugins rather than failing silently.
- **Workspace and docks:** empty versus filtered-empty states are distinct;
  `ALL TYPES` is absent when empty; rename is truly inline and preserves tree
  expansion; the Project header drops noisy counts; its close control works;
  and a collapsed Project rail restores the hidden dock. Bookmarks content is
  built lazily and shares the two-line empty-state treatment.
- **Chrome and dialogs:** editor-paper and menu-strip colors are shared theme
  roles; tab/dock seams and the status-bar resize grip use single hairlines;
  document tabs use the 31 px upstream height; frameless edge/corner resize
  remains available; the app opens centered at 1080×720; tooltips/popups use
  square corners; and live value-history hover contents no longer flicker.
- **Process/source pickers:** every process row gets an icon fallback; Refresh,
  Attach, and Cancel use the square outline-button treatment; saved-source rows
  reserve their delete-button gutter and retain keyboard navigation.
- **Windows console:** the UI binary uses the Windows GUI subsystem (no launch
  flash), and View ▸ Show Console allocates/persists an on-demand console,
  protects the application from the console close button, preserves redirected
  stdout/stderr handles, and only frees a console it allocated itself.
- **Profiler:** a deterministic stderr report sorted by total time is available
  for headless capture.

Upstream-only CMake registration, `.gitignore` entries, Qt widget render tools,
and Qt meta-object/test scaffolding were not copied. The Rust tree exercises the
ported behavior through native unit/integration tests and its real GPUI capture
harness (`scripts/ui.sh`); it already used JetBrains Mono as its default and did
not bundle the IBM Plex resource removed by the second upstream commit.

The next delta is measured from `9f39e36`. To recompute it against a fresh
upstream (from any clone of `IChooseYou/Reclass` — e.g.
`/home/loke/Documents/Reclass`):

```sh
git -C <clone> fetch origin
git -C <clone> log --oneline --reverse 9f39e36..origin/main
```

When the port catches up again, bump this tracking point to the new HEAD.
