# ReClass-RS — C++ → Rust Parity Report (wf18, updated wf19)

Authoritative C++ original: `/home/loke/Documents/Reclass`
Rust port: `/home/loke/reclass-rs`
HEAD at report time: `bb977eb` (4 wf19 follow-up commits on top of the wf18 HEAD `582f25b`).

This report scores **1:1 behavioral parity OUTSIDE the deliberate stubs**. The
deliberately-excluded stubs (live-memory providers, the managed .NET CLR host,
native runtime DLL/SO loading, the subprocess loader, and permission sandboxing)
are NOT scored as gaps — they are listed at the end.

---

## Parity gate (verified GREEN at HEAD `bb977eb`, tree clean)

All 7 checks of the universal parity gate pass:

| # | Check | Result |
|---|-------|--------|
| 1 | default/ui build | Finished |
| 2 | headless build (`disasm,symbols,imports,mcp`) | Finished |
| 3 | plugins build (`--features plugins`) | Finished |
| 4 | full tests (`disasm,symbols,imports,mcp`) | **1102 pass / 0 fail** |
| 5 | ui tests (`--lib ui::`) | **559 pass / 0 fail** |
| 6 | plugins tests (`--features plugins --lib`) | **1562 pass / 0 fail** |
| 7 | bare tests (`--no-default-features`) | **822 pass / 0 fail** |

`git status --short` is clean. Test counts grew vs the wf18 HEAD
(full 1083→1102, ui 551→559, plugins 1536→1562, bare 822→822); the
+19/+8/+26/+0 deltas are the new tests added by the wf19 A1/A2/M1a/M1b
follow-up batches (the MCP/app-shell tests are not in the bare feature set,
hence bare is unchanged).

---

## Per-subsystem parity

| Subsystem | Parity % | Key remaining in-scope gaps |
|---|---:|---|
| Address Expression Parser (`src/addr.rs`) | 99 | UTF-16-vs-char `errorPos` indexing and exotic-Unicode `skip_spaces` set (ASCII inputs unaffected); no dedicated vtop/cr3/phys success-path test. |
| Generator (`src/generator.rs` + UI wiring) | 99 | Live code-view **render cache** (perf only, byte-identical output) and scroll-restoration not ported. |
| Disassembly (`src/disasm.rs`) | 98 | `hex_dump` uses checked `+` on `base_addr` (debug panic only near `u64::MAX`). |
| Controller / Document / sources (`src/controller.rs`) | 98 | Two File-load paths reset the snapshot inconsistently (each individually defensible); `attach_provider` base-adopt also triggers on `base==0` (wider than literal C++); split-editor panes not modeled; macro per-command obsolete + overlap-pair logging are faithfulness tweaks. |
| RTTI + symbols (`src/rtti/`) | 97 | Browser uses MSVC→Itanium fallback where C++ Tools-menu was MSVC-only (additive, spec-intended); `NameRegistry` not aggregated by any panel (not present in this C++ checkout either). |
| Compose + undo (`src/compose.rs`) | 96 | Inline enum annotation has one extra space (`0x6  (RGBA)`) inside the newer chip model; no byte-exact rendered-text test for inline annotations. |
| Editor surface (`src/ui/editor/`) | 96 | Hover-preview popup host + 4 previews and presentation-mode animations are gpui-overlay/animation surfaces verified by contract tests, not pixel parity; minor extra nav-target coverage vs `hitTestTarget`. |
| Type-selector popup (`src/ui/typeselectorpopup.rs`) | 95 | Cosmetic-only: 220px detail pane, normal/compact density toggle, `SortAlign` mode, loading skeleton (all spec-dropped for GPUI); composite-icon enum-vs-struct glyph collapsed to one. |
| Core model (`src/core/`) | 94 | `typeinfer` `strengthFromScore` threshold (85 vs current C++ 75), 3 extra `countPtrFeatures64` sentinels, an extra `pruneAndRank` dominance gate, and 2 extra `ViewState` fields — all vs a *newer* C++ rev; verify against intended revision. |
| Format / render (`src/format.rs` + compose render dup) | 93 | `compose.rs` render duplicate rounds half-to-even (not Qt half-away) and mishandles `%g` carry; `-0.0` sign dropped; two divergent copies of the render layer is a maintenance hazard. |
| Scanner + panel (`src/scanner.rs`, `src/ui/scannerpanel.rs`) | 93 | Mode switch does not apply C++ smart-filter defaults; most keyboard shortcuts unported; inline value-write parse path differs; rescan `readSize` for extended types; cosmetic status/label strings. |
| Imports/exports (PDB / ReClass-XML / source / PE) | 95 | Largely at parity; residual edge cases in dialog flow and format sniffing. |
| Providers + plugin system (`src/provider/`, `src/plugin/`) | 95 | Plugin **system** (contract/registry/manager/dialog/discovery) at/over parity and verified; remaining gaps are stub-adjacent (runtime load has no binary on disk here). |
| Widgets + dialogs (`src/ui/*`) | 90 | OptionsDialog search omits some keywords; MessageBox detail-list + width clamp differ; ProcessPicker path-column/`lastAttachedProcess`; SourceChooser pid==0; confirm-dialog destructive default-focus not applied (gpui limit); DialogButton chrome intentionally Zed-aesthetic. |
| Themes + theme editor (`src/theme/`) | 88 | Heat-color derivation + `from_json` marker fallback use stale spec values; 6/8 default `selection` hexes diverge from shipped C++ JSON; tests lock the divergent values; a 9th built-in theme added; pretty-print indent 2 vs Qt 4. |
| App shell (window/menus/docks/tabs/start page) | 94 | Window close (X/Alt+F4/titlebar), replace-all `open_project`, XML byte-sniff, consumed `show_icon`, and real recents `age_days` all ported (wf19 A1/A2). Residual: recents bucket edges use elapsed-seconds whole-day deltas vs the C++ calendar `QDate::daysTo`, and ThisMonth approximates same-month+year as `<31` days (minor documented approximation, functionally-correct buckets for typical files); titlebar show-icon 32→34px height bump not reproduced (gpui-component `TitleBar` owns its fixed height — accepted cosmetic divergence; only the label↔icon swap is ported). By design (not scored): bespoke dock-drag overlay + status-bar shimmer + CLI `--profile/--screenshot` (gpui/Zed substitutions). |
| MCP bridge (`src/mcp/`) | 95 | Six `evidence.*` tools + `tree.export_header` added; the 5 phantom `tools/list` entries removed (`tools/list` now the authoritative 37-tool C++ set); `initialize` evidence paragraph, `project.state` evidence summary, `tree.apply` `change_comment`, and `McpBridge` `notify_evidence_changed` + `URI_EVIDENCE` all ported (wf19 M1a/M1b). Residual: `tree.export_header` sorts `selected_ids` before iterating (`tools.rs:2624`) vs C++ iterating an unordered `QSet` (`mcp_bridge.cpp:3473`) — a defensible determinization, only observable with multiple selected struct roots, not a gap against any deterministic C++ contract. Live-memory provider plugins + permission sandboxing behind the tools remain deliberate stubs (excluded). |

---

## Overall parity % (with method)

**Overall weighted parity OUTSIDE STUBS ≈ 95%** (wf18: 93% → wf19: 95%).

**Weighting method.** Each subsystem is weighted by the **size/importance of the
in-scope C++ surface it ports**, proxied by authoritative-C++ source LOC
(`wc -l` of the mapped `src/*.{cpp,h}` files), with two honest, explicitly-stated
discounts applied to avoid over-weighting non-load-bearing surface:

- **App shell** raw C++ is ~11.7k LOC, but a large fraction is bespoke
  custom-paint Qt UI (dock-drag overlay, VS2022 start page, status-bar shimmer)
  that the port **substitutes by design** with gpui-component / Zed equivalents.
  Counting that raw LOC would over-weight intentionally-substituted surface, so
  app-shell is weighted at ~6.0k (the load-bearing logic + wiring).
- **MCP** raw C++ is ~4.4k LOC, but a large fraction is repetitive per-tool
  schema/dispatch boilerplate. Weighted at ~1.5k to reflect distinct behavior.

Approximate weights used (in-scope C++ LOC): controller 5959, editor 5077,
imports 4019, widgets+dialogs 3632, scanner 3605, core 2528, typeselector 1979,
generator 1747, compose 1639, MCP 1500, providers+plugin 1200, format 950,
RTTI+symbols 839, themes 672, addr 540, disasm 76, app-shell 6000.
Total weight ≈ 41,962.

`Σ(parity × weight) / Σ(weight)` with the SAME weights (total ≈ 41,962, unchanged
— only the MCP and app-shell parity scores moved):
- **wf18:** `= 93.13% → 93%` (MCP 78, app-shell 86).
- **wf19:** `= 94.88% → 95%` (MCP 78→95, app-shell 86→94; all 15 other rows unchanged).

(Unweighted mean across the 17 subsystems = 95.0% post-wf19, up from 93.5%, i.e.
the weighting still does not flatter the result — the two formerly-lowest
subsystems were both real-weight rows, so closing them moves the number honestly.
The new lowest in-scope rows are themes 88 and widgets+dialogs 90.)

---

## What this run fixed (wf18 — commit hashes)

| ID | Subsystem | Fix | Commit |
|----|-----------|-----|--------|
| B1 | Controller / Provider | Interior-mutable `write(&self)` + real write-through (no more `Arc::get_mut` failure after a snapshot exists; `SnapshotProvider::write` performs the real write-through). | `e748bac` |
| B2 | Controller | Attach-data-file now clears undo + resets snapshot (matches `loadData`). | `c658961` |
| B3 | Controller | Obsolete-drop adjusts the clean index on undo/redo. | `1f2d607` |
| B4 | Controller | Ported `navigate_to_formula` (non-undoable bookmark goto) + File-branch source-switch parity. | `82e0682` |
| B5 | Type-selector | Correct fuzzy scorer (`source_score`, branch-cap-4) for the filtered list, matching `typeselectorpopup.cpp:149`. | `ac925e9` |
| B6 | Type-selector | Within-group sort + same-size-first ordering + model-side category-chip filtering. | `6066ff7` |
| B7 | Type-selector | Composite `TypeEntry` reports the **computed** struct size (`tree.struct_span`) not the dyn placeholder. | `d566a85` |
| B8 | Editor surface | Tab-cycle onto Type / PointerTarget / ArrayElementType opens the **picker** (mirrors the mouse path). | `1765f03` |
| B9 | Editor surface | hit-test bails on `ArrayElementSeparator` rows + redirects an array element's Type/Name click to the parent array header. | `6234350` |
| B10 | RTTI + symbols | Wired the Tools ▸ RTTI Browser handler (`tools.rtti` → `open_rtti_browser`) to the existing dialog + field gate. | `54af577` |
| B11 | Generator | Live code view + export honor CodeFormat / CodeScope / type aliases / asserts (format+scope selector wired in the tab). | `582f25b` |

No batches were reverted; the gate stayed green throughout.

---

## wf19 follow-up

Landed the two biggest remaining levers from the wf18 report's "How to reach
100%" list (batches 1–2): the MCP tool-surface + behavior parity batch and the
app-shell correctness + behavior batch. All 4 batches landed; none reverted; the
7-step gate stayed GREEN throughout.

| ID | Subsystem | Fix | Commit |
|----|-----------|-----|--------|
| A1 | App shell | Window close (OS/WM X, Alt+F4, titlebar X) now routes through the unsaved-changes prompt instead of bypassing it — closes the data-loss-class correctness gap. | `ee1eec6` |
| A2 | App shell | `open_project` does replace-all into a fresh tab; XML detected by byte-sniff (first bytes) not file extension; persisted `show_icon` consumed (label↔icon swap); recent-file list shows real `age_days` buckets instead of collapsing to "Today". | `9cb09e4` |
| M1a | MCP bridge | `tools/list` realigned to the authoritative 37-tool C++ set: added the six `evidence.*` tools + `tree.export_header`, removed the 5 phantom entries; parity tests updated to the real list. | `3845db2` |
| M1b | MCP bridge | `initialize` instructions gained the evidence paragraph; `project.state` emits the evidence summary; `tree.apply` gained the `change_comment` op; `McpBridge` gained `notify_evidence_changed` + `URI_EVIDENCE`. | `bb977eb` |

**Re-audited subsystem parity:** MCP bridge **78 → 95**; App shell **86 → 94**.
All 15 other subsystems unchanged.

**Overall weighted parity (same weighting method, total weight ≈ 41,962 unchanged):
93% → 95%** (`93.13% → 94.88%`).

**Test counts (7-step gate, GREEN at HEAD `bb977eb`, tree clean):**
full 1083→**1102**, ui 551→**559**, plugins 1536→**1562**, bare 822→**822**
(+19/+8/+26/+0; the MCP/app-shell tests are outside the bare feature set, so bare
is unchanged).

**Still open after wf19** (now documented approximations / design substitutions,
not data-loss-class gaps):
- *App shell:* recents bucket edges use elapsed-seconds whole-day deltas vs the
  C++ calendar `QDate::daysTo`, and ThisMonth approximates same-month+year as
  `<31` days — minor documented approximation (functionally-correct buckets for
  typical files); titlebar show-icon 32→34px height bump not reproduced
  (gpui-component `TitleBar` owns its fixed height — accepted cosmetic
  divergence; only the label↔icon swap is ported); bespoke dock-drag overlay +
  status-bar shimmer + CLI `--profile/--screenshot` remain gpui/Zed substitutions
  by design.
- *MCP bridge:* `tree.export_header` sorts `selected_ids` before iterating
  (`tools.rs:2624`) whereas C++ iterates an unordered `QSet`
  (`mcp_bridge.cpp:3473`) — a defensible determinization, only observable with
  multiple selected struct roots, not a behavioral gap against any deterministic
  C++ contract. Live-memory provider plugins + permission sandboxing behind the
  MCP tools remain deliberate stubs (excluded, not scored).

---

## Remaining in-scope gaps (prioritized — what + why-not-yet)

> wf19 closed the former High items #1 (MCP tool surface) and #2 (app-shell
> window-close guard) plus the former Medium items #3 (MCP behavior) and #4
> (app-shell behavior). See "## wf19 follow-up" above. The list below is renumbered.

### High
*(none — the two former High items were closed by wf19.)*

### Medium
1. **Format — `compose.rs` render duplicate rounds half-to-even and mishandles `%g` carry; `-0.0` sign dropped.** *Why not yet:* the fix is to delete the duplicate and route the live editor through the verified `format.rs`; a careful refactor (two callers, golden tests) deferred to avoid churn near the report.
2. **Themes — heat-color derivation + `from_json` fallbacks use stale spec values; 6/8 default `selection` hexes diverge from shipped C++ JSON, and tests lock the divergent values.** *Why not yet:* requires re-deriving from the authoritative shipped JSON and rewriting the fidelity tests that currently assert the wrong values — moderate, low user-visible impact.
3. **Scanner — mode switch does not apply C++ smart-filter defaults; most keyboard shortcuts unported.** *Why not yet:* engine-level behavior is correct and tested; this is UI-flow polish.
4. **Core model — `typeinfer` threshold/sentinel/dominance differences vs a *newer* C++ rev.** *Why not yet:* these diverge only against a C++ revision newer than this checkout; confirm intended revision before changing (changing now could regress against the on-disk source).

### Low / cosmetic (representative)
- Compose inline enum-annotation extra space; addr/generator UTF-16-vs-byte column indexing (ASCII unaffected); type-selector detail pane / density toggle / loading skeleton (spec-dropped for GPUI); widget chrome (DialogButton, MessageBox width clamp, ProcessPicker column widths) intentionally Zed-aesthetic; bespoke dock-drag overlay + status-bar shimmer + CLI `--profile/--screenshot` (gpui substitutions by design); generator render cache (perf only). These are documented non-load-bearing or design-substitution items.

---

## Excluded stubs (deliberate — NOT scored as parity gaps)

- **Live-memory provider plugins** — C++ `plugins/{ProcessMemory, RemoteProcessMemory, WinDbgMemory, KernelMemory}` that read live OS process memory. The Rust port keeps a logical-source stub pending the security program. (B1 fixed the in-scope write model so these will be correct when implemented.)
- **`RcNetPluginCompatLayer` managed .NET CLR host** — Windows-only P5 scaffold.
- **Native plugin runtime DLL/SO loading** — infra at parity, but no binary on disk to load in this environment.
- **P7 subprocess loader.**
- **Permission sandboxing** — disclosure-only by design.

---

## How to reach 100% (outside stubs)

1. ~~**MCP parity batch (biggest lever, +~3–4 pts overall):** add the six `evidence.*` tools and `tree.export_header`; remove the 5 phantom tools from `tools/list`; add the `change_comment` op; restore `notify_evidence_changed` + `URI_EVIDENCE`; add the evidence paragraph to `initialize` and the evidence summary to `project.state`; update the `tools/list` parity tests to the real list. Brings MCP ~78 → ~95.~~ **DONE in wf19 (M1a `3845db2`, M1b `bb977eb`); MCP 78 → 95.**
2. ~~**App-shell correctness + behavior batch:** intercept window-close through the unsaved-changes guard; make `open_project` replace-all into a fresh tab; sniff the first 64 bytes for XML; consume `show_icon`; compute real recent `age_days`. Brings app-shell ~86 → ~94.~~ **DONE in wf19 (A1 `ee1eec6`, A2 `9cb09e4`); app-shell 86 → 94.**
3. **Format de-duplication:** delete the `compose.rs` render duplicate and route the live editor through the verified `format.rs` (fixing half-away rounding, `%g` carry, and `-0.0` sign in one move). Format ~93 → ~98 and removes the standing maintenance hazard.
4. **Themes authoritative-default sync:** re-derive heat colors + the 8 default `selection` hexes from the shipped C++ JSON and rewrite the fidelity tests to guard the authoritative values. Themes ~88 → ~96.
5. **Scanner UI-flow batch:** apply smart-filter defaults on mode switch + port the remaining keyboard shortcuts. Scanner ~93 → ~97.
6. **Core `typeinfer` reconciliation** — only after confirming the target C++ revision (this checkout vs the newer rev the understand-docs reference); align threshold/sentinels/dominance to whichever is authoritative.
7. **Residual cosmetics** — close the remaining byte-exact/test-coverage items per subsystem (inline-annotation tests, caret-clamp `testAddrEdit*` suite, winsdk roundtrip test) only if strict snapshot parity is mandated; most are explicitly accepted as gpui/Zed substitutions.

Batches 1–2 landed in wf19 (overall **93% → 95%**). Landing the remaining
batches 3–5 (format de-dup, themes default sync, scanner UI-flow) would move the
weighted overall from **95% to ~97–98%**; the remaining delta to 100% is the
documented cosmetic / design-substitution surface, which is intentional and
accepted by the specs.
