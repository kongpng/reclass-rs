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

The `f9171af..b7baced` delta — **20 upstream commits** (`b7baced` was
`origin/main` HEAD, 2026-06-06) — **has been ported into the Rust version**, each
landed as its own commit matched to its upstream source(s); pure build/CI/Qt-only
commits were skipped as having no Rust analogue. So the Rust port now tracks
**`b7baced`**, even though the local C++ snapshot at `/home/loke/reclass-cpp`
remains at `f9171af`.

The next delta is therefore measured from `b7baced`. To recompute it against a
fresh upstream (from any clone of `IChooseYou/Reclass` — e.g.
`/home/loke/Documents/Reclass`):

```sh
git -C <clone> fetch origin
git -C <clone> log --oneline --reverse b7baced..origin/main
```

When the port catches up again, bump this tracking point to the new HEAD.
