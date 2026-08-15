---
Status: Current
Created: 2026-06-23
Last edited: 2026-08-15
---

# Herdr Preview

A terminal review pane for Herdr: browse a coding agent's changes, comment on line ranges, and send the comments back to the agent.

## Overview

Herdr Preview is the `pi-dal.herdr-preview` plugin. It retains the `herdr-reviewr` Rust crate and review engine from its upstream foundation. It installs the review UI as `herdr-preview`, so its panes remain distinct from upstream `herdr-reviewr` panes. One binary runs in a Herdr pane, rooted at its launch directory. A Git worktree opens Git review. Any other readable directory opens Files-only. It renders in the real terminal, so fonts and colors are whatever the user already runs.

Humans open the pane with configurable placement. Agents use the stable `peek` action to open a non-focusing right split (`herdr-host.md`).

The reviewer's loop:

```
open the pane → pick a changed file → read its diff → comment on a range
→ send the comments to the agent → add a line and hit enter
```

Three tabs:

| tab         | shows                                                                  |
| ----------- | ---------------------------------------------------------------------- |
| `Changes`   | the active scope's changed files, with a syntax-highlighted diff viewer |
| `All files` | the whole repo tree, with a read-and-comment content viewer             |
| `PR`        | a read-only mirror of the pull request: state, checks, comments         |

Files-only shows only the `Files` browser. It roots the tree at the launch directory and shows read-only content. The launch root is retained as a real-directory capability, and Files-only resolves listings and content reads descriptor-relative with no-follow semantics rather than treating display paths as filesystem authority. It lists the root directly, then lists one real directory at a time only after expansion. It never selects another pane or repository. Git review tabs, scopes, comments, agent send, and Git refresh are unavailable there. `.git` remains excluded from the Files-only tree.

## Voice

reviewr is lightly empowering. Its copy leaves the reviewer feeling capable, in control, and ready
to move the work forward.

- Lead with the state. Keep expected states short and calm.
- Offer one useful next step only when the user needs one.
- In low-stakes moments, a restrained question or nudge may add warmth.
- In failures, drop the personality. Say what happened and how to recover.
- Never scold, hype, narrate the implementation, or turn an empty state into documentation.

## Scope

- The `Changes` view: a changed-files list per scope plus the diff viewer (`diff-view.md`).
- The `All files` tab: a repo tree and content viewer, annotated with the active scope's changes (`file-list.md`, `diff-view.md`).
- The `PR` tab: pull-request identity, state, checks, and comments, read from the repository's forge, with external links only (`forge-host.md`, `pr-tab.md`).
- Three scopes: `uncommitted`, `branch`, `last-turn` (`review-model.md`).
- Comments anchored to `path:start-end`, held in memory for the review pass.
- Export of all comments to the agent input or the clipboard.
- Poll-based refresh plus a manual refresh key.
- Keyboard and mouse input (`input.md`).
- Full-screen search over the worktree: fuzzy file names and literal code with a live preview, ranking owned by the engine (`search.md`).
- In-file find in the read pane: literal match highlighting and match-to-match stepping (`find-in-file.md`).

## Roadmap

Named so the architecture stays open to them. None is part of this design.

- Reviewed-file state: marking a file reviewed and greying it in the list.
- Hopping between the agent's changed files while browsing `All files`.
- A side-by-side split diff view for wide panes.
- Search on `Changes`, scoped to the changeset.
- Live theme switching.

## Continuity

The agent edits the worktree while the reviewer reads it. These rules govern how every surface
absorbs that motion — a poll, a refresh, a returning fetch, an agent commit.

State divides into three kinds:

- Authored state is what the reviewer wrote: comments and the draft being typed.
- Place state is where the reviewer's attention is: the active tab and scope, the open file, every
  cursor and scroll, folds, a selection, the layout, the footer's shortcut expansion.
- Derived state is everything recomputed from git or the forge: changesets, trees, diffs, the PR
  snapshot.

Authored state survives every world event. Only the reviewer removes it (Invariants).

Place state moves only under the reviewer's own input. A world event may only reconcile it, in
order: match the same target by identity (a path, a comment's author and anchor — never a row
index), fall back to the nearest surviving target, clamp as the last resort. While the reviewer is
mid-gesture — composing, dragging a divider, holding a selection — their place is frozen.

Derived state on screen may be stale, never wrong. A view blanks only when its identity changed —
a different repository, pull request, or file — never because the same thing gained newer content.
Newer content paints over the old in place, reconciling the reviewer's place as above.

## Invariants

| Always true                                                                                                                                                 |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| reviewr never commits, stages, or mutates the worktree, the index, or any branch. Its only git writes are private refs under `refs/reviewr/`: the turn baseline and the base pick.  |
| reviewr never writes to a forge. It reads the pull request through the forge's official CLI and opens links in the browser, nothing more.                   |
| A comment, saved or being typed, is never lost to a refresh or the agent's edits. Only the reviewer removes it, and only an explicit export takes it out.   |

## Related specs

- [review-model](./review-model.md)
- [diff-view](./diff-view.md)
- [theme](./theme.md)
- [file-list](./file-list.md)
- [input](./input.md)
- [search](./search.md)
- [find-in-file](./find-in-file.md)
- [tui](./tui.md)
- [pr-tab](./pr-tab.md)
- [herdr-host](./herdr-host.md)
- [forge-host](./forge-host.md)
