---
Status: Current
Created: 2026-06-24
Last edited: 2026-08-15
---

# File list

The file navigator: a directory tree that opens a file in the read pane. It lists the scope's changed files in `Changes` and the whole worktree in `All files`.

## Overview

The list groups files into a collapsible tree. A file row shows a change marker, an optional semantic code, its name, and its add/remove stats.

```
 src/
   M app.rs                    +562 −16
   A diff_view.rs              +210
   M ui.rs                     +437 −9
 specs/
   A diff-view.md              +96
   M …/2026-06-23-changes/plan +4 −2
 M  Cargo.toml                 +11 −1
 ?  herdr-plugin.toml          +25
```

In Git review, `All files` lists the whole worktree: tracked, untracked, and ignored alike. Ignored rows render dimmed. `.git` is the one exclusion. A file the active scope changed keeps its marker and stats. The rest show name only.

In Files-only, the navigator lists the direct root entries before its first frame. It lists a readable real directory's direct children only after the reviewer expands it. A refresh asks the root and visible expanded loaded directories. It retains a loaded subtree when a later directory read fails, and an unknown failed directory remains retryable. The launch root is accepted only when it opens as a real non-symlink directory capability. Each requested relative path is parsed into components and opened one real directory at a time relative to that retained descriptor with no-follow semantics: root, prefix, parent, `.git`, and symlink components are rejected. The directory stream is read through its verified descriptor; before enumeration, a replaced target is rejected rather than listed. Direct child symlinks and `.git` entries are omitted. A selected file is reopened at click time through the same capability with no-follow before its metadata or bytes are read, so a post-listing symlink replacement is rejected rather than read. Files-only therefore excludes `.git` and its descendants at every depth, does not follow symlinks, and skips unreadable or disappearing branches. Every row is unannotated because Files-only has no Git changeset.

```
 src/
   M app.rs                    +562 −16
     diff.rs
   M ui.rs                     +437 −9
 specs/
     overview.md
 target/                       (ignored — dimmed, one collapsed row)
 Cargo.toml
```

### Node

The list is a flat sequence of visible rows over the tree.

| field       | type     | meaning                                                             |
| ----------- | -------- | -------------------------------------------------------------------- |
| `kind`      | enum     | `dir` or `file`                                                       |
| `name`      | string   | the segment shown: a directory name, or a file's basename             |
| `depth`     | integer  | nesting level, for indentation                                        |
| `change`    | enum?    | `added`/`modified`/`deleted`/`renamed`/`untracked`, absent on a `dir` and on an unchanged file |
| `additions` | integer? | lines added in the scope, absent on unchanged rows                    |
| `deletions` | integer? | lines removed in the scope, absent on unchanged rows                  |
| `ignored`   | bool?    | in `All files`, whether git ignores this row (rendered dimmed), absent on tracked and untracked rows |
| `expanded`  | bool?    | on a `dir`, whether its children are shown                            |

## Behavior

### Tree

- Files group by directory. Directories sort first, then files, both alphabetical.
- A directory with a single child collapses into its child's row, so vertical space goes to names, not scaffolding.
- `Changes` directories open expanded. A changeset is small enough to show whole.
- `All files` and Files-only directories open collapsed. The worktree is not.
- Files-only materializes only root entries and children reachable through expanded loaded ancestors. Expansion queues one-level filesystem work after the frame and never traverses descendants on the input loop.
- A wholly-ignored directory is one collapsed row. Its contents enumerate only on expand, so a large ignored tree costs nothing until opened.

### Selection

- The cursor selects a row. `j`/`k` and the arrows move it, skipping collapsed subtrees. The list scrolls to keep it visible.
- The hunk steps and the file skips move the cursor onto the file they open, from either pane (`input.md`).
- Moving onto a file opens it in the read pane: its diff in `Changes`, its content in `All files`.
- The wheel scrolls the viewport only. The selection and the open file stay put, so browsing never reloads a diff. When the tree exceeds its viewport and the inner navigator has at least two columns, a right-edge scrollbar shows that independent viewport position; its track is reserved before row layout, so it never overwrites a marker, filename, or stats. Zero- and one-column interiors show no track. It is visual feedback, not a second selection control.
- Clicking anywhere on a directory row selects it and toggles it. A file-row click selects and opens only that file. A click while a line range is selected preserves the range and reports `clear selection before opening a file` rather than silently doing nothing.
- `→` expands a closed directory, then moves to its first visible child when it is open. `←` collapses an open directory, then moves to its visible parent when it is closed. `enter` toggles a directory or opens its selected file.
- A directory's full painted row is its mouse target at every navigator position. Polls preserve its cursor path, expansion, scroll, and reveal state by path (→ Continuity).
- `tab` moves focus to the read pane, to navigate and comment.
- A poll preserves the selection and expansions by path. A selected file that left the changeset falls back to the open file, then the first file. A Files-only completion for a collapsed path is discarded rather than painting hidden children.
- In `All files` a poll adds and removes rows as the worktree changes, preserving cursor, scroll, and expansions by path.
- Switching scope re-marks the `All files` tree in place. Only the markers and stats change.

### Presentation

- A directory row is `<indent><disclosure> <name>/`. Disclosure plus the trailing slash is the complete directory grammar. A file row is `<marker> <kind?> <name> <stats>`. The kind is navigator-only and never appears in Search; the colored Git marker remains a separate review-state field.
- Kind resolution is pure lexical lookup on the canonical entry path. Its precedence is user exact basename, user longest dot-separated compound suffix, user final extension, bundled exact basename, bundled compound suffix, bundled final extension, then filename-prefix/category/generic fallback. A user rule always dominates bundled associations, including `.env.rs` and `Dockerfile.rs`. It never reads MIME data, Git, metadata, or the filesystem. User associations are the closed built-in identities documented in `config.md`, never arbitrary glyphs or external themes.
- `plain` is the default, one font-safe ASCII code plus a separator: `s` source, `c` config/build, `d` document, `j` structured data, `t` style/template, `i` image, `m` media, `p` archive/package, `b` executable/binary, and `.` generic. `emoji` is an explicit standard-Unicode emoji presentation. `nerd` is an explicit retained compatibility presentation requiring a compatible Nerd Font. `none` omits the kind. `unicode` is a legacy alias normalized to `plain` (`config.md`).
- Plain and Nerd reserve their one-cell glyph plus separator. Emoji reserve `unicode_width`'s measured string width plus separator, so a narrow row drops the whole optional slot before harming filename, Git marker, or stats. Actual terminal emoji/font advance width and missing Nerd glyph coverage cannot be detected. Directories consume no kind slot and the row never exceeds its navigator geometry.
- Stats read `+added −removed`: additions green, deletions red, a zero side dropped. A change with no countable lines (a binary file) shows no stats.
- Ignored icons dim with ignored names. Selection uses the existing secondary palette treatment so the icon stays legible.
- An ignored row dims whole, distinct from the marker colors. `All files` is the one place an ignored path is readable. An ignored file never carries a change marker, since every scope respects `.gitignore` (`review-model.md`).
- A too-narrow path shortens with a middle ellipsis (`…/2026-06-23-changes/plan`), keeping the basename and stats visible.

## Non-goals

- No reviewed-file state. Marking a file reviewed and greying it is roadmap.
- No file content rendered here. The read pane renders the diff or content (`diff-view.md`).

## Related specs

- [review-model](./review-model.md)
- [input](./input.md)
- [tui](./tui.md)
- [search](./search.md)
