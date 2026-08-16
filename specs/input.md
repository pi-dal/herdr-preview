---
Status: Current
Created: 2026-07-17
Last edited: 2026-08-15
---

# Input

Driving the review: the keymap, the changeset traversal, the live footer, and the comment editor.

## Overview

Every action has a key. The mouse-relevant ones also work by click or drag.

The keymap is rebindable per action through `[keybindings]` in the plugin config (`config.md`):

- **In-Preview keys** in the table are the terminal-facing defaults. Bare keys (`c`, `v`, `s`,
  `?`) are the primary focused-pane workflow.
- **Herdr-global Option keys** are host bindings, not a second keymap. A host plugin action routes
  its canonical input through the active Preview pane (`herdr-host.md`). The host may bind that
  action to a non-conflicting chord: for example, a profile whose Herdr vertical split is `alt+d`
  binds Preview Changes to `alt+shift+d`.
- The `action` column names the action for `[keybindings]`.
- The keys shown are defaults: a bare character, or a `ctrl+`/`alt+` chord (`config.md`).
- The arrows, `tab`, `esc`, `enter`, and the page keys are structural. They are fixed and never rebind.
- A key hint in the header or the footer shows its action's first bound key.
- The comments list acts through the same bindings and closes on `esc` and the `comments` binding.
- The agent picker acts through the `down` / `up` bindings and closes on `esc`.
- The base picker filters through a text field with the comment editor's controls, moves through the arrows, and closes on `esc`.
- Prose and mockups elsewhere show the default keys.

| action                                                   | does                                        | keys                                        | mouse                         |
| -------------------------------------------------------- | ------------------------------------------- | ------------------------------------------- | ----------------------------- |
| `down` / `up`                                            | move the cursor in the focused pane         | `j` / `k` / `↓` / `↑`                       | click a row                   |
| `next-hunk` / `prev-hunk`                                | jump to the next / previous change run      | `alt+right` / `alt+left`, `]` / `[`         | —                             |
| `next-change` / `prev-change`                            | jump to the next / previous changed line    | `alt+down` / `alt+up`                       | —                             |
| `next-file` / `prev-file`                                | jump to the next / previous file            | `alt+shift+down` / `alt+shift+up`, `f` / `F` | —                             |
| —                                                        | navigate a directory tree                   | `←` / `→` / `enter`                         | click anywhere on a directory row |
| —                                                        | switch focus between list and diff          | `tab`                                       | click a pane                  |
| —                                                        | move a page                                 | `PageUp` / `PageDown` / `ctrl+u` / `ctrl+d` | —                             |
| —                                                        | scroll the viewport, selection put          | —                                           | wheel over the pane           |
| —                                                        | scroll the diff horizontally (wrap off)     | `←` / `→`                                   | —                             |
| `scope-uncommitted` / `scope-branch` / `scope-last-turn` | switch scope                                | `u` / `b` / `t`                             | click the scope chip to cycle |
| `base-pick`                                              | open the base picker                        | `B`                                         | click the base name           |
| `tab-changes` / `tab-all-files` / `tab-pr`               | switch tab                                  | `alt+d` / `alt+f` / `alt+r`, `1` / `2` / `3` | click a tab name              |
| `hide-unchanged`                                         | fold all unchanged context in Changes       | `alt+u`                                     | —                             |
| —                                                        | expand the fold under the cursor            | `→`                                         | click the `⋯` row             |
| —                                                        | open a link in rendered markdown            | —                                           | click the link                |
| `wrap`                                                   | toggle line wrap                            | `w`                                         | —                             |
| `preview`                                                | toggle the markdown preview                 | `m`                                         | —                             |
| `navigator-position`                                     | move the navigator clockwise                | `p`                                         | —                             |
| `navigator-hide`                                         | hide / show the navigator                   | `z`                                         | —                             |
| `navigator-grow` / `navigator-shrink`                    | grow / shrink the navigator                 | `<` / `>`                                   | drag the divider              |
| `select`                                                 | select a line range, removed lines included | `v` then move                               | click-drag in the diff        |
| —                                                        | clear the selection                         | `esc`                                       | —                             |
| `comment`                                                | comment on the selection                    | `alt+c`, `c`, type, `enter`                 | inline `Comment` control      |
| `edit`                                                   | edit the comment under the cursor           | `e`                                         | —                             |
| `delete`                                                 | delete the comment under the cursor         | `d`                                         | —                             |
| `next-comment` / `prev-comment`                          | jump to next / previous comment             | `n` / `N`                                   | —                             |
| `comments`                                               | list and manage all comments                | `alt+l`, `l`                                | —                             |
| `search`                                                 | open the search screen (`search.md`)        | `/`                                         | —                             |
| `find`                                                   | open in-file find (`find-in-file.md`)       | `ctrl+f`                                    | —                             |
| `keys`                                                   | toggle the footer's full shortcut list      | `alt+h`, `?`                                | —                             |
| `send`                                                   | send all comments to the agent              | `alt+s`, `s`                                | —                             |
| `submit-review`                                          | submit this pane's pending GitHub review     | `S`                                         | —                             |
| `assign-remote`                                          | assign selected GitHub inline finding        | `A`                                         | —                             |
| `copy`                                                   | copy all comments to the clipboard          | `y` / `Y`                                   | —                             |
| `open-pr`                                                | open the PR in the browser (`pr-tab.md`)    | `o`                                         | click the status chip         |
| `refresh`                                                | refresh now                                 | `alt+shift+r`, `r`                          | —                             |
| `quit`                                                   | quit                                        | `q`                                         | —                             |

Herdr's global Preview actions send the table's canonical default `alt+` key through the pane API (`herdr-host.md`). The key enters this keymap like an in-pane chord. It does not depend on a terminal delivering a physical Option key, and it creates no second terminal binding. A `[keybindings]` override still controls the key's resulting action.

In Files-only, the `Files` tab, navigator controls, wrapping, markdown preview, find, search when available, and refresh remain active. Directory rows use the same whole-row targets and fixed tree keys as Git Files. Git scopes, Changes and PR tabs, change and hunk traversal, hide unchanged, comment actions, and send or copy are inert and identify Files-only mode.

`navigator-position` cycles `right` → `bottom` → `left` → `top` → `right`.

In a focused navigator, `→` expands a closed directory or moves to its first visible child. `←` collapses an open directory or moves to its visible parent. `enter` toggles a directory and opens a file. A directory's entire painted row toggles it by mouse. File rows keep their select-and-open behavior and never toggle a directory.

`navigator-grow` and `navigator-shrink` change the active share by four percentage points. The allowed range clamps every change.

`navigator-hide` hides the navigator and shows it again in place (`tui.md`). On the file tabs it is never inert in `Normal` mode, so the way back is always the key that hid it. On `PR` it is inert and stays out of the footer (`tui.md`). While the navigator is hidden, `navigator-position`, `navigator-grow`, and `navigator-shrink` are inert. In `Normal` mode, `tab` then shows the navigator and focuses it. Every other mode keeps its own `tab` meaning (`search.md`).

Outside the hidden-state rules above, these four navigator actions work from either main pane on every tab. While the comment editor is open, their printable characters are text. In the comments list, the agent picker, and the base picker they are inert. Those local modes omit the navigator actions from the footer.

A divider drag belongs to the navigator position and split axis at mouse-down. A keypress, terminal resize, or config-driven layout change cancels it. A cancelled drag keeps its last painted share, and the cancelling keypress still performs its own action. After cancellation, drag events are consumed until mouse-up rather than becoming a selection in the read pane.

Writing a comment is explicit: select, inspect the anchor strip, draft, then review or send. A navigator click while a selection is live preserves that selection and reports `clear selection before opening a file`, so a mouse action never silently discards an anchor. `c` on a content row remains the fast singleton draft. `v`, mouse-down, and drag select content rows. A fold expands instead of selecting. While a selection is live, file, tab, scope, and traversal changes are inert. A dedicated row after the selected endpoint names its range, count, and old/new/mixed side, and keeps `c comment` plus `esc clear` visible. Its `Comment` button is the exact mouse target and never paints over source. The composer title names New or Edit, the elided file location, and the line count. Its footer keeps `enter save`, `shift+enter newline`, and `esc cancel`, and distinguishes saved comments from the draft. Saved cards expose their ordinal, range, current or `STALE` state, and `click/e edit`. `n`/`N` cycles exact cards, including cards sharing a line. `d` opens `enter delete · esc cancel`. A successful send says `Added N comments to <agent>, not submitted`. A successful copy reports that they were copied.

Remote GitHub finding assignment is explicit: on the PR tab, `A` is available only for the selected inline GitHub finding with a direct thread URL. It opens a frozen agent picker and then a separate `Enter assign · Esc cancel` confirmation. Successful delivery bracketed-pastes a task envelope containing the thread URL, author, body, file/location, and returned snippet when available. It never submits agent input and never posts, replies, resolves, approves, requests changes, or otherwise writes to GitHub. A delivered or failed receipt stays in this Preview session only; a PR refresh may replace remote activity but never changes GitHub thread state.

GitHub pending-review publishing is explicit: `p` opens a confirmation only for a resolved, current-side, single-line diff comment. Bare `enter` publishes it into a Preview-owned pending review; `esc` cancels, and no path submits, approves, or requests changes. Before the mutation Preview rebuilds the comment's exact anchor and derives GitHub's one-based unified-diff `position` inside its current hunk. When that position cannot be reconstructed, publishing is disabled rather than guessing an API anchor. Preview rechecks that the provider still reports the same open PR and head immediately before the mutation. A failed publish keeps the local comment and its failure receipt.

## Behavior

### Changeset traversal

`next-hunk` / `prev-hunk` keep their canonical config IDs and step between **change runs**, not hunks in user-facing copy. `next-change` / `prev-change` step each added or removed row and cross directly to the nearest changed row in the adjacent file. They are inert outside Changes source, in previews and notices, and while selecting or composing.

`next-hunk` / `prev-hunk` step the diff cursor between change runs, from either pane. A step lands on the first row of a run of changed rows. A context line or a fold ends a run, so two edits three lines apart are two stops.

- Each press jumps to the nearest run past the cursor: `next-hunk` below, `prev-hunk` above.
- With no run left that way, the first press arms a crossing and holds the cursor still. The next press the same way opens the adjacent file on its nearest run. A notice diff, which has no runs at all, arms on the first press like any other file.
- The armed crossing leads the footer, keyed to the step that armed it. It is the one movement key the footer names.
- Any other input drops the arm and still does its own work. A background poll keeps it, unless it changes the open file.
- A crossing arms only when a file to cross to exists. At the changeset's ends nothing is offered and nothing moves.
- A file with no changed rows is crossed over, notice diffs (`binary`, `too_large`) included.
- The steps are inert in `All files` and in the markdown preview, which paint no changed rows.

`next-file` / `prev-file` skip a file per press, from either pane, and never arm:

- In the diff, each press opens the next or previous file, cursor on its first row. Focus stays on the diff.
- In the file list, each press moves the cursor to the nearest file row, skipping directories.
- The skips land on every file, notice diffs included. From a preview, the opened file starts in source (`diff-view.md`).

The steps and the skips share the rest:

- Adjacency is the list's visible order, so a collapsed subtree is skipped.
- Opening a file this way moves the list selection onto it.
- With no target in the pressed direction, a press does nothing.
- Both are inert while a line selection is live and while the comments list, the agent picker, or the base picker is open.
- The `PR` tab has neither.

### Footer

The footer is one row: the primary next step, the cursor's own actions, `send` once a comment
exists, and `S submit review` when this Preview pane's cached current GitHub PR identity has its own
pending-review binding, closing with a `?`. The footer predicate is an in-memory PR/identity-refresh
cache; rendering never runs Git or forge commands. Pressing `?` expands it to every shortcut that
works here, and it stays until `?` or `esc`. It never lists a key that would not work in the current
state.

```
 e edit · d delete · n/N jump · s send 2                                      ?
```

Opening it turns the one-row action bar into a labeled grid. Row 1 becomes the `do` band under a
dim `do` label: the primary and the cursor's actions. `send` and `submit` bands follow when their
actions are available, then `go` (the global actions) and `move` (cursor movement). The distinct
`submit` band ensures `S` remains discoverable when ordinary agent send is also available. Every
band's content aligns in one column. The `?` stays at the right of the `do` row.

```
 do    e edit · d delete · n/N jump · s send 2                                ?
 go    u/b/t scope · / search · ctrl+f find · w wrap · l list · y copy · r refresh · 1·2·3 tabs
       tab files · p position · z hide · q quit
 move  j k · ] [ hunk · f F file · PageUp PageDown
```

A band wraps to as many rows as its keys need. The label sits on the first row, and continuation
rows indent under the keys. A cursor action that does not fit row 1 continues under the `do` label
on its own indented row.

Row 1 is always shown:

| slot    | content                                                                           |
| ------- | --------------------------------------------------------------------------------- |
| primary | the most likely next step, in a bright accent, never dropped                      |
| send    | `s send N`, present once any comment is written, after the primary, never dropped |
| status  | the transient message, after `send`, truncated to fit                             |
| actions | the cursor's other actions, in normal text, trimmed to fit                        |
| more    | a `?` at the right, muted but legible — always present, and expands the rest      |

A narrow row drops trailing actions to fit. The status outranks them, since the `?` panel repeats
every action and nothing repeats the status. A 40-column reviewr pane shows `no agent here …` rather than
nothing. The status drops only below a legible width. The primary, `send`, and the `?` never drop. On
a pane too narrow even for those, the primary truncates before the `?` does.

The `?` expansion:

- It lists every shortcut applicable in the current context that is not already on row 1, wrapped
  below row 1 in three labeled bands, each a dim label then its keys. `do`: the cursor's actions.
  `go`: the global actions, each shown only where it works — scope, the base picker, search, find,
  wrap, the comments list, copy, refresh, the tabs, the pane toggle, the navigator position and hide
  keys, quit. `move`:
  down and up, the hunk and file steps, the page keys. An empty band is dropped, and a key that would
  not work in the current state never appears. The hunk step shows only where it works, the `Changes`
  diff and never a preview (see Changeset traversal). `PR` has no hunk or file steps. The hide key's
  hint reads `hide` while the navigator shows and `show` while hidden.
- Row 1 wears the `do` label and aligns into the grid only while the panel is open. Collapsed, it is
  the flush action bar with no label. The `?` sits at its right in both states.
- It takes body rows down to the read pane's minimum (`tui.md`). A context that needs more rows than
  that shows only what fits, and row 1 always survives.
- Its open state is place state (`overview.md`). `?` and `esc` move it. A world event only re-derives
  its content in place, reconciled by identity, never the toggle. It opens collapsed, is never saved,
  and config recovery preserves it.
- `?` toggles it. `esc` closes it, one step behind a live selection and an armed crossing — each `esc`
  consumes exactly one.

Row 1's primary and actions follow the cursor:

| cursor on                                | primary                        | also                              |
| ---------------------------------------- | ------------------------------ | --------------------------------- |
| an armed crossing                        | `] next file` / `[ prev file`  | the cursor's own actions, demoted |
| a diff line                              | `c comment`                    | `v select`                        |
| a line of a markdown file that previews  | `c comment`                    | `v select · m preview`            |
| a live selection                         | `c comment`                    | `esc clear`                       |
| a commented line                         | `e edit`                       | `d delete · n/N jump`             |
| a fold                                   | `→ expand fold`                | —                                 |
| an open markdown preview                 | `m source`                     | —                                 |
| a file (file list)                       | `tab diff`                     | `z hide`                          |
| a collapsed directory                    | `→ expand`                     | `z hide`                          |
| an expanded directory                    | `← collapse`                   | `z hide`                          |
| nothing to review (awaiting turn)        | `u/b/t scope`                  | `r refresh`                       |
| the `branch` scope with no base          | `B pick base`                  | `u/t scope · r refresh`           |
| an empty read pane, navigator hidden     | `z show`                       | `tab files`                       |

- An armed crossing outranks the cursor's own action and leads row 1, since only the footer says the next press leaves the file. It is the one movement key on row 1 (see Changeset traversal). While it is armed, the `move` band drops the hunk step, whose key row 1 now shows.
- While the navigator is hidden, `z show` joins row 1's actions, so the collapsed footer always names the way back. Visible, `z hide` joins them only while the file list holds focus, whose row 1 has the room. Elsewhere it waits in the `go` band.
- When the awaiting-turn state and the hidden empty read pane match at once, the awaiting-turn row wins, and `z show` still joins its actions.
- `scope`, `search`, and `find` are global, not cursor actions, so the `go` band carries them, never row 1 — `search` in every context, `find` wherever the read pane has content (`search.md`, `find-in-file.md`). `scope` leads row 1 only where nothing else does, an empty or notice diff.
- Movement keys never sit on row 1. The `move` band shows them.
- The comment editor, the comments list, the agent picker, the base picker, the search screen, and the find band show their own one-row footer, without `?`. The expansion's open state is kept and restored when they close.
- `?` (the `keys` action) toggles the expansion in `Normal` mode only. It is text in the comment editor, the search and find inputs, and the base picker's filter, and inert in the comments list and the agent picker.
- The changed-file count and line totals live in the header. The footer carries only the comment count, inside `s send N`.
- On `PR` row 1 carries the PR state line and `o open ↗` per `pr-tab.md`, and `?` expands to the rest.

### Comment editor

A plain-text field that edits at the caret, not only at the end. The search input and the base picker's filter share these controls, without the newline inserts (`search.md`). An empty box shows a dim `Leave a comment…` placeholder. `e` preloads the exact selected card's text with the caret at the end. An unresolved anchor stays in the list as detached text and never opens an inline composer below replacement source.

```
┌ comment · llm_registry.py:41 ───────────┐
│ this import path looks wrong█            │
│ and breaks on 3.12                       │
└──────────────────────────────────────────┘
```

| key                                             | does                                             |
| ----------------------------------------------- | ------------------------------------------------ |
| `←` / `→`                                       | move the caret one character                     |
| `↑` / `↓`                                       | move the caret one wrapped row, keeping column   |
| `Home` / `End`, `Ctrl+A` / `Ctrl+E`             | move to the start / end of the logical line      |
| `Alt+b` / `Alt+f`, `Alt`/`Ctrl` + `←` / `→`     | move by a word                                   |
| `Backspace` / `Delete`                          | delete before / after the caret                  |
| `Ctrl+W`                                        | delete the word before the caret                 |
| `Ctrl+U` / `Ctrl+K`                             | delete to the start / end of the logical line    |
| `Alt+Enter` / `Shift+Enter` / `Ctrl+J`          | insert a newline                                 |
| `Enter` / `Esc`                                 | save / cancel, cancel discards the draft         |

- A paste arrives whole via bracketed paste. A multi-line paste keeps its newlines. `\r\n` and `\r` normalize to `\n`.
- A paste outside the comment editor and the search input is ignored. It never starts or mutates a comment.
- Movement, insertion, and deletion are character-wise. Multi-byte and wide characters count as whole characters.
- The terminal cursor follows every field's caret in display cells, anchoring an IME candidate
  window after wide characters.
- The painted block caret covers the character under the insertion point. With no character under
  it, at the end of a logical line or of the input, the terminal cursor alone marks the insertion
  point.
- `↑`/`↓` move by wrapped rows. `Home`/`End` and the kill keys act on the logical line, the run of text between explicit newlines.
- A box too short for its wrapped rows scrolls to keep the caret row visible.
- A caret past an exactly-full row sits on the next row's first cell, where the next character
  lands. Input that ends by exactly filling its last row keeps an empty continuation row for it.
- `Alt+b`/`Alt+f` always survive as ESC-prefixed sequences. The modified arrows work where the terminal delivers them. The character arrows, `Home`/`End`, and `Ctrl+A`/`Ctrl+E` always work.

### Agent picker

`Send` always opens a confirmation sheet (`herdr-host.md`), including with one or zero agents. The sheet states the comment and stale counts, selected target, and that text is added to agent input and is not submitted. Every key below acts, and every other key is inert.

| key                     | does                                        |
| ----------------------- | ------------------------------------------- |
| `j` / `k` / `↓` / `↑`   | move the highlight                          |
| `1` – `9`               | move the highlight to that row              |
| `enter`                 | send every comment to the highlighted agent |
| `y`                     | copy when the sheet names no agent or unavailable Herdr |
| `esc`                   | cancel, keeping every comment               |

Only the unmodified `enter` sends. `Alt+Enter` and `Shift+Enter` insert a newline in the comment editor, so carrying that chord into the picker sends nothing rather than handing the whole review to the armed agent. The no-target `y` path uses the ordinary consume-on-success clipboard export.

- A click moves the highlight to the clicked row. A click on the highlighted row sends. The highlight is armed when the picker opens, so a first click on the armed row sends immediately. Every other gesture is inert, and none reaches the view behind.
- The digits are literal here, whatever `tab-changes` and its siblings are bound to. A modified digit moves nothing.
- `esc` cancels with or without a modifier, so no keystroke traps the reviewer in the picker.
- A picker taller than the pane scrolls with the highlight.
- `esc` returns to the view the send was issued from, the comments list and the find band included.

### Base picker

`base-pick` opens the picker over the body, like the comments list (`tui.md`). It works on the file tabs while the `branch` scope is active and no `--base` flag was passed. Elsewhere it is inert and stays out of the footer. While the comment editor is open, the base-name click is inert like the key.

The list holds one row per branch name, remote-tracking and local names merged. The checked-out branch is not listed, unless it is the default branch, whose row must stay reachable to clear a pick. Rows sort by most recent commit. Two rows outrank that order:

- The open PR's target sorts first, starred (`forge-host.md`).
- The default branch sorts next, marked `default`. Choosing it clears the pick (`review-model.md`).

The highlight opens on the current base, else the first row. The highlight is place state (`overview.md`).

| key                 | does                                            |
| ------------------- | ----------------------------------------------- |
| typed character     | narrow the list, matching anywhere in the name  |
| `↓` / `↑`           | move the highlight                              |
| `ctrl+n` / `ctrl+p` | move the highlight                              |
| `enter`             | pick the highlighted branch                     |
| `esc`               | cancel                                          |

The filter is a text field with the comment editor's controls, above. `↑` and `↓` move the highlight, so the single-line filter keeps `←` and `→` for its caret. A pasted newline drops, so a branch name copied with its line ending filters as the bare name.

- A click moves the highlight. A click on the highlighted row picks.
- A filter matching no branch shows `no branches match`, and `enter` does nothing.
- A pick applies immediately: the changeset rebuilds and the header renames (`review-model.md`).
- A picker taller than the pane scrolls with the highlight.

## Non-goals

- No text selection, cut/copy, undo/redo, markdown rendering, or click-to-place-caret in the comment editor.
- No named-key or multi-key sequence bindings. A binding is one key, alone or with a `ctrl+`/`alt+` prefix.
- No `down` / `up` crossing at a file's edges. The line cursor clamps there.
- The `?` expansion omits the navigator-resize keys and the horizontal-diff-scroll keys. Resizing is a divider drag first, and horizontal scroll is one of the `←` / `→` keys' several meanings.

## Related specs

- [tui](./tui.md)
- [config](./config.md)
- [diff-view](./diff-view.md)
- [review-model](./review-model.md)
- [pr-tab](./pr-tab.md)
- [search](./search.md)
- [find-in-file](./find-in-file.md)


### Assign a comment to an agent

`a` targets the focused comment (or selected comments-list card) and opens an agent picker. The picker accepts `↑`/`↓` or `j`/`k`, `1`–`9`, bare `Enter` to deliver, and `Esc` to cancel. Delivery uses bracketed paste into the chosen Herdr agent tab and never submits it. The comment remains in the review and records its latest delivery receipt.

### Publish a comment to GitHub pending review

`p` targets the focused current-side, single-line Diff comment (or selected comments-list card) and opens a confirmation sheet. Bare `Enter` creates or appends exactly that one comment to Preview's session-owned GitHub pending review; `Esc` cancels. It never submits, approves, requests changes, or removes the local comment. Publish is disabled outside Git review, in Files-only/image view, for stale, old-side, All-files, or multi-line anchors, and when no open GitHub PR with a head OID is available.

The pending review binding is keyed by exact `{host, owner, repository, PR number, head SHA}`. Preview reuses only a binding it created in this pane session with that complete key; it never discovers or adopts a pending review created elsewhere. A remote failure leaves the local comment in place with a retryable failed receipt. Successful cards show that they are pending on GitHub and, when returned, the remote URL.
