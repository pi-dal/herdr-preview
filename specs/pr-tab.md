---
Status: Current
Created: 2026-07-17
Last edited: 2026-08-08
---

# PR tab

A read-only mirror of the pull request in reviewr's frame: identity in the header, checks and comments in the navigator, the selected body in the read pane.

## Overview

The navigator shows checks and selects the description or a comment. The read pane shows that selection. The header carries the PR's identity and state. The tab reads the repository's forge through `forge-host.md`. It keeps review activity read-only by default, while deliberately retaining local comment `send`, `list`, and `copy` workflows. Its browser actions open only URLs that pass the shared safe-link policy. The only forge mutation reachable here is an explicitly confirmed `S submit review` for this pane's own cached GitHub pending-review binding; no action automatically submits, approves, requests changes, resolves, or posts to the forge.

The tab is labeled `PR` on every forge. Body text, the chip, the read pane's title, and the footer use the resolved forge's vocabulary (`forge-providers.md`). A repository that resolves to no forge takes the default vocabulary. A `finding` from a forge that returns no code context shows its body alone.

```
 1 Changes  2 Files  3 PR    Deep research: GPT-5.5/5.4-mini upgrade…  deep-research  merged #226 ↗
╭─ @codex · manager.py:115 ──────────────────────────╮╭─ Checks & comments ──────────╮
│ -    if primary_result.status == PERM_FAILURE:        ││ description                  │
│ -        return primary_result                        ││                              │
│                                                       ││ checks  ✗ 1 failing          │
│ Avoid falling back after target permanent failures.   ││  ✓ build-main-image          │
│ This now attempts a fallback for every non-success…   ││  ✗ tests                     │
│                                                       ││                              │
│                                                       ││ comments · 5                 │
│                                                       ││ @you    comment          5m  │
│                                                       ││▍@codex  manager.py:115   2h  │
│                                                       ││ @claude review           2h  │
│                                                       ││ @claude manager.py:39    2h  │
│                                                       ││ @claude parse.py:187 outdated│
╰───────────────────────────────────────────────────────╯╰─────────────────────────────╯
 ⚠ conflicts with main · ⇡ 2 unpushed · ✗ 1 failing · 5 comments   o open ↗                            ?
```

## Behavior

### Header and footer

- The header right-anchors a clickable `status #226 ↗` chip, status colored by lifecycle: `open` green, `draft` yellow, `merged` mauve, `closed` red. The `draft` status shows only while the PR is open. The PR title sits to its left, truncated to fit.
- Between title and chip sits the resolved head branch (`head_ref`, `forge-host.md`), dim, prefixed `⑂ ` when the head lives in a fork. On a narrow bar the branch drops first.
- The footer leads with merge, sync, checks, and comment counts, then `o open ↗` and the `?` when the PR URL passes the shared trimmed HTTP(S), nonblank, no-control/no-bidi browser policy (`markdown.md`). An unsafe forge URL leaves `o open` out of the footer; pressing `o` or clicking the chip reports a safe failure without launching it. Merge and sync show only while the PR is open. A capped surface appends a `+more ↗` link naming the forge (`forge-host.md`).
- The `?` expands to the `go` band and a `move` band of down, up, and the page keys. The `PR` tab has no hunk or file steps (`input.md`).
- The ordinary no-PR body says only `No pull request yet. Ready to ship?` A detached HEAD says `No pull request found — HEAD is detached.` Both use the forge's noun.

### Navigator and read pane

- The navigator, titled `Checks & comments`, shows a status-only checks section above the comments list. The cursor walks the description row and the comments.
- Comments list newest first, each row `@author anchor age`, with `outdated` or `resolved` markers where the forge receded the thread.
- A non-empty PR description pins a `description` row at the top of the navigator, above the checks. An emptied description vanishes like a comment: the cursor clamps, the read pane resets.
- The read pane shows the selected comment: a finding shows its `snippet` then the body, a review or plain comment shows its prose, the description row shows the PR description.
- Bodies render as markdown (`markdown.md`). A finding's `snippet` stays plain `+`/`−`-colored lines.
- A human author is emphasized over the bots.
- `j`/`k` or a click selects a description or comment and reveals it in the navigator viewport. Checks are not selectable.
- The wheel over the navigator scrolls its viewport without changing the selection. The wheel over the read pane scrolls the read pane. `PageUp`/`PageDown` scroll the focused pane. Both panes stop with their last line at the bottom edge.
- `o` or the chip opens the PR in the browser only after its URL passes the shared trimmed HTTP(S), nonblank, no-control/no-bidi browser policy. An unsafe URL does not launch the browser and reports the safe failure in the status line.
- A body taller than the read pane shows a scrollbar on the pane's right border. One that fits shows none.
- A retry notice for a preserved snapshot stays fixed above the read body, so it remains visible without resetting the reader's scroll.
- File-anchor authoring keys (`c`, `v`, `d`, `e`) do nothing here. Existing local comments deliberately remain available: `s` sends them to the selected Herdr agent, `l` lists them, and `y` copies them. These local workflows never mutate the forge.
- `S` is available only while this pane's cached, session-owned GitHub pending-review binding still matches the current PR identity. It opens the explicit review-event selection and confirmation flow; only its final bare `enter` submits. `esc` cancels at every step. It never automatically submits, approves, requests changes, resolves, posts, or otherwise mutates GitHub.
- A merged or closed PR shows the same mirror, read-only.
- No usable forge CLI shows the matching failure state from `forge-host.md`, naming the command that unblocks it.

### Refresh

- The tab fetches on open, on entering the tab, on `r`, and on the worktree's turn-end on any tab (`herdr-host.md`, HH-TURN-PER-WORKTREE), with a slow fallback timer while active. One fetch per turn keeps the tab fresh before it is entered. An ambient trigger rides a fetch already in flight: the ridden result still paints, and one trailing fetch follows it. `r` cancels the in-flight fetch and starts fresh. A fetch stuck past a minute is abandoned and replaced instead of joined. Its cadence is separate from the worktree poll (`tui.md`).
- A refetch keeps your place: the cursor follows the selected comment by identity, and both pane scroll positions hold. A vanished comment clamps the cursor and resets the read pane.

## Non-goals

- No jump from a PR comment's anchor to the code tabs.

## Related specs

- [forge-host](./forge-host.md)
- [forge-providers](./forge-providers.md)
- [tui](./tui.md)
- [input](./input.md)
- [markdown](./markdown.md)
