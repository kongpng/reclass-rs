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

As of this writing, upstream `origin/main` was at **`b7baced`** (2026-06-06),
**20 commits ahead** of the `f9171af` base. That delta (`f9171af..b7baced`) is
the set of upstream changes being ported into the Rust version.

To recompute the delta against a fresh upstream (from any clone of
`IChooseYou/Reclass` — e.g. `/home/loke/Documents/Reclass`):

```sh
git -C <clone> fetch origin
git -C <clone> log --oneline --reverse f9171af..origin/main
```

When the port catches up to a newer upstream commit, bump the **Base revision**
above to that commit so the next delta is measured from the right point.
