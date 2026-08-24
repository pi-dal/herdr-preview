---
Status: Current
Created: 2026-06-23
Last edited: 2026-08-15
---

# Herdr host

How Herdr Preview runs inside Herdr: the Preview pane, the actions that manage it, sending comments to an agent, and turn tracking.

## Overview

Herdr Preview uses the `herdr-preview` terminal binary, and every launcher produces the same review pane (→ HH-LAUNCHER-BLIND): the actions, the `worktree.created` event, a layout plugin's pane command, and a hand-typed command. The plugin packaging (`herdr-plugin.toml`) builds the binary, binds the actions, and hooks the event.

The binary paints its empty frame before the first git scan, so the pane never shows herdr's blank grid. A failing scan shows the error in the status line. A hung `git` leaves a frozen but visible pane.

## Invariants

| code                            | Always true                                                             |
| ------------------------------- | ----------------------------------------------------------------------- |
| `HH-HUMAN-PLACEMENT-CONFIGURED` | Human `open` and `toggle` use the placement named by `toggle_placement`. |
| `HH-PEEK-RIGHT-NONFOCUSING`     | Agent `peek` opens only a right split and never changes focus.           |
| `HH-TURN-PER-WORKTREE`          | A turn belongs to the worktree, never to one agent.                      |
| `HH-LAUNCHER-BLIND`             | A Preview pane behaves the same however it was created.                  |

## Pane identity

A Preview pane is any pane running `herdr-preview` in its foreground process group, read live from Herdr at each action and event. A wrapped launch like `cargo run` counts through its child. A flag run like `--resolve-plugin-config` is not the review UI and never counts. A pane running `herdr-reviewr` belongs to the upstream plugin and never counts as Preview.

The review run labels its pane `Preview` when the pane has no label, and a normal exit clears only a `Preview` label, so a name the user gave the pane survives both ends. The label is display only: a failed write or a stale label changes nothing an action or the event reads.

## Install paths

The plugin keeps the binary linked at a stable path, `~/.local/state/herdr/plugins/pi-dal.herdr-preview/bin/herdr-preview`, and at `~/.local/bin/herdr-preview` when that directory exists. The install step creates both, and every action and event re-points them at the live plugin root. Both paths are links into the installed plugin, never copies, and neither ever replaces anything but a symlink. A launch after an uninstall fails rather than running a stale build.

## Pane actions

Actions bind to keys and to script invocations alike.

| action          | pane absent                      | pane present                  |
| --------------- | -------------------------------- | ----------------------------- |
| `open`          | opens one with human config      | does nothing                  |
| `close`         | does nothing                     | closes them all               |
| `toggle`        | opens one with human config      | closes them all               |
| `peek`          | opens one right split            | does nothing                  |
| `forward <key>` | opens one right split then sends | sends to the first live pane  |

With valid plugin config:

| question                | answer                                                                      |
| ----------------------- | --------------------------------------------------------------------------- |
| run it twice?           | it converges and exits 0, nothing stacks and nothing errors                 |
| does `auto_open` gate?  | no, explicit actions ignore `auto_open`                                     |
| focus?                  | human opens take focus. `peek` never changes focus                            |
| on refusal, on success? | exit 1 with one stderr line, exit 0 with at most one stdout line             |
| what counts as open?    | any Preview pane in the workspace (Pane identity)                            |
| which workspace?        | the focused one, wherever the action is invoked from                        |

`peek` is the stable agent interface. It always passes `split`, `right`, and `--no-focus`, regardless of `toggle_placement` and `toggle_direction` (→ HH-PEEK-RIGHT-NONFOCUSING). It attaches beside the invoking pane, or the focused pane when the action has workspace context. It never focuses or closes an existing Preview pane.

The manifest's `changes`, `files`, `review`, `comment`, `comments`, `send`, `refresh`, `hide-unchanged`, `previous-file`, and `next-file` actions call `forward` with the input table's default canonical key. Herdr sends the canonical key through `pane send-keys`. It does not need the terminal to deliver a physical Option key. Unshifted Option-left/right/up/down are intentionally not forwarded host actions, so they reach the active terminal or Preview text field without plugin interception. The identity sweep chooses only a foreground `herdr-preview` process. It never sends to `herdr-reviewr` or a pane title.

A host key is a user profile decision, not a plugin contract. It must not shadow a Herdr-native
command. In the supplied macOS profile, `alt+d` stays Herdr's vertical split and
`alt+shift+d` invokes Preview Changes; the manifest still forwards the canonical `alt+d` input
inside Preview. This keeps one in-pane action keymap while allowing a conflict-free global layer
(`input.md`).

A forwarding open uses `peek` placement and focus. `changes`, `files`, `review`, `refresh`, `hide-unchanged`, `previous-file`, and `next-file` preserve the invoking focus. `comment` and `comments` focus Preview before sending because the next input belongs there. `send` adds no host focus change, then Preview applies its normal send behavior. A focus or send failure refuses before a key reaches a pane.

Every action validates plugin config before inspecting the workspace (`config.md`). An action refuses without workspace context. Refusals land in `herdr plugin log list`.

## Pane placement

Placement settings come from the plugin config (`config.md`). Each action and event reads one snapshot.

```toml
toggle_placement = "overlay"   # split | overlay | zoomed | tab   (default: split)
toggle_direction = "down"      # right | down, split only         (default: right)
auto_open = false              # auto-open on worktree.created    (default: true)
```

A human open gives focus to Preview (→ HH-HUMAN-PLACEMENT-CONFIGURED). The event auto-opens configured `split` and `tab` placements only, never takes focus, and does nothing with `auto_open = false`. Like `open`, the event opens nothing when the workspace already holds a Preview pane. The event acts on the workspace its payload names, never the focused one. A missing key uses its default. Invalid plugin config follows `config.md`.

| placement | direction         | covers the pane |
| --------- | ----------------- | --------------- |
| `split`   | `right` or `down` | no              |
| `tab`     | none              | no              |
| `overlay` | none              | yes             |
| `zoomed`  | none              | yes             |

A `split` or `zoomed` open attaches to the focused pane, or to the workspace's first pane when the context has none.

A `tab` open names the fresh tab `Preview`, using the `tab_id` the pane-open result reports. Herdr otherwise labels a new tab with a bare number. The rename is cosmetic. When it fails, or an older Herdr omits `tab_id`, the open still succeeds and the tab keeps its numeric label.

**HH-EVENT-BESIDE-LAYOUT: a layout plugin handles the same event**

1. The user sets `auto_open = false`. A layout plugin also handles `worktree.created`.
2. A worktree is created. herdr runs both handlers in any order.
3. Preview opens nothing either way. The layout builds undisturbed.
4. The user toggles later. Preview opens over the finished layout (→ HH-HUMAN-PLACEMENT-CONFIGURED).

## Launch directory

The binary classifies its own launch directory once. A Git worktree normalizes to its top level and opens Git review. Every other readable directory opens Files-only rooted exactly at that directory.

A manual action passes the focused pane's live foreground directory unchanged. When Herdr has no live directory, it passes the action context's focused launch directory unchanged. It never inspects other panes for a repository, chooses a repository, walks child directories, or refuses only because the directory is outside Git. A known Preview rooted elsewhere does not satisfy an open or forward for this directory.

`peek` and `forward` use the same focused directory rule before their right-split open. A global shortcut can therefore create or reuse a Files-only Preview without changing the invoking focus. The worktree-created event continues to use its payload directory directly.

## Sending to the agent

`Send` freezes every written comment for an explicit confirmation sheet. It asks Herdr directly and never routes text through Pi or another intermediary.

| herdr reports          | `Send` does                                                                  |
| ---------------------- | ---------------------------------------------------------------------------- |
| one agent              | opens a confirmation sheet with that agent preselected                       |
| several agents         | opens the same sheet with a retained selection                               |
| no agent               | opens a retained-drafts sheet naming clipboard copy                          |
| no answer              | opens a retained-drafts sheet naming Herdr as unavailable and clipboard copy |

A candidate is any other pane in the same herdr workspace carrying an `agent` field. Placement never narrows the set, and no scope inside the workspace wins over another.

The pane write wraps the batch in bracketed paste markers, never raw bytes. The write removes every paste terminator inside the batch. The clipboard export carries the batch untouched.

The sheet states count, stale count, selected target, and `Text is added to agent input and is not submitted.` Bare `enter` sends directly through the existing bracketed-paste pane path. Its no-agent and unavailable states keep drafts and show the resolved `copy` binding; they never advertise an inert `enter` send. A successful write clears exactly that frozen batch and says `Added N comments to <agent>, not submitted`. A focus failure after the write remains success. A send that does not land says so in one short sentence and keeps every comment. It never shows herdr's own wording.

### Agent picker

Each row leads with the agent's name, with its state and tab trail dim behind. The highlight is a row fill (`tui.md`).

```
┌ Send 3 comments to ─────────────────────────────────────────┐
│ 1  claude        idle · Grip Outreach · last used           │  ← highlighted, filled row
│ 2  release-bot   idle · Grip Outreach Campaign              │
│ 3  codex         working · 3                                │
└─────────────────────────────────────────────────────────────┘
```

Every part comes from herdr, and reviewr synthesizes none of it:

| part  | herdr source                                                          |
| ----- | --------------------------------------------------------------------- |
| name  | the agent's `name`, else its `display_agent`, else its kind           |
| state | the agent's `state_labels` entry for its state, else the state itself |
| tab   | the tab's label                                                       |

The highlight opens on the agent this session last sent to, else on the first row. Only a successful send sets the last-sent agent. The last-sent row carries a dim `last used` tag.

- Rows keep herdr's own order.
- Only the first nine rows carry a number.
- The row set and its order freeze when the picker opens (`overview.md`).

The send addresses the pane on the chosen row. A pane that closed while the picker was open fails the send, and every comment stays. A successful send focuses the chosen agent and names it.

A configuration error closes the picker and drops its frozen rows. Every comment survives, and so does the last-sent agent that arms the next picker (`config.md`).

## Per-comment agent assignment

The `a` action opens the same frozen workspace-agent picker for one stable comment ID. A bare Enter writes a structured task envelope through the existing bracketed-paste pane path and never submits it. Success or failure is retained on that comment; it is never consumed.

## Turn tracking

Two agents editing one worktree produce one turn (→ HH-TURN-PER-WORKTREE), so a second agent starting mid-turn never re-baselines the first one's work out of the diff.

An agent is in the worktree when its working directory resolves to the reviewr pane's git top level. Only another pane carrying an `agent` field counts. A second worktree of the same repository resolves elsewhere and is not in it. herdr workspaces and tabs never enter this rule, so placement never changes which turns the pane sees, and a plain clone tracks like a herdr worktree.

reviewr polls the agents in the worktree on every worktree refresh:

| the worktree | when                                                    |
| ------------ | ------------------------------------------------------- |
| rests        | every agent in it reports `idle` or `done`              |
| works        | at least one agent in it reports `working`              |
| neither      | an agent reports `blocked` or `unknown`, and none works |

A worktree holding no agents rests, so the first agent to arrive and work starts a turn.

A turn starts on the edge from rest to work. It starts only from rest, so a `blocked` permission prompt mid-turn resumes the same turn rather than starting another.

A turn ends when the worktree next rests, however many neither samples sit between. The two edges are deliberately asymmetric: a missed start only widens the diff, which is allowed below, while a missed end would strand the `PR` tab's refetch.

A poll that cannot observe the whole worktree changes nothing, and the last known membership stands. Two failures reach it: herdr is unreachable, or git cannot be run to resolve an agent's directory. Either holds the poll, so a member left unresolved under load never reads as an empty worktree. Before any poll succeeds membership is unobserved, which the empty state reports as waiting rather than as an empty worktree (`tui.md`).

On a turn start, reviewr snapshots the worktree as a candidate baseline. The candidate becomes the live baseline on the first poll where that turn changed a file, so a turn that changes nothing never moves it. The live baseline is the old side of every `last-turn` diff until the next change-producing turn replaces it.

The snapshot never touches the index, the worktree, or any branch, and it respects `.gitignore`. The baseline lives in a private ref under `refs/reviewr/turn-base/`, keyed by worktree path and outside `refs/heads`. The ref persists across reviewr pane restarts.

## Failure semantics

Actions:

- Two concurrent opens can both open a pane. The next action heals it: `open` no-ops and `close` sweeps both.
- An open can race a binary still starting in another pane. Both land, and the next action heals it.
- With `auto_open` on, the event and a layout can each open a pane for one worktree. The next action heals it, and `auto_open = false` is the layout recipe (HH-EVENT-BESIDE-LAYOUT).
- A `close` racing an in-flight open exits 0, and the open still lands.
- An action that observed a pane may act after that pane exited. It still exits 0, and the next action converges.
- A forward whose target exits, cannot be focused, or cannot receive a key refuses. It never retries another pane after that failure.
- A pane the process read reports gone has exited and does not count. A read that fails any other way refuses the whole action, so a failing herdr never reads as an absent pane.
- A close herdr refuses because the pane is gone converges and exits 0. A close failing any other way finishes the sweep and then refuses, so a failing herdr never reports a running pane closed.
- A crashed binary leaves its pane a plain pane. The actions no longer count it, so `open` opens a fresh Preview pane and `close` never touches the old one.
- A `close` sweeps by the live process read, so it reaches a pane herdr's plugin registry forgot after a restart.
- An open never opens into the pane that invoked it.
- An action acts on the focused workspace. herdr offers no workspace selector on invoke, so a scripted open lands wherever the user is looking, not where the script meant.
- After a close, focus falls wherever herdr leaves it.

Send and tracking:

- Browsing and the clipboard export work without the herdr CLI. Without it `last-turn` stays empty, `uncommitted` and `branch` are unaffected, and the plugin config goes unread unless `HERDR_PLUGIN_CONFIG_DIR` names it (`config.md`).
- A failed clipboard utility or `herdr pane send-text` reports the error. The comments stay in the list.
- A turn shorter than one poll interval, or one whose start is masked by a `blocked` or `unknown` report, is missed. `last-turn` then shows the changes since the last observed turn start, which is more than that turn wrote and never less.
- An agent left `blocked` or `unknown` holds the worktree at neither for as long as it stays there, so no other agent in that worktree can start or end a turn meanwhile. Their work accumulates into the open turn's diff rather than being lost. Answering the prompt releases it: the next poll rests, ends the held turn, and the turn after that re-anchors the baseline. Distinguishing a bystander from the agent that was working needs per-agent turn identity, which `HH-TURN-PER-WORKTREE` trades away on purpose.
- A crash mid-snapshot costs at most one failed refresh. Ref updates are atomic, and leftover locks clear before the next snapshot and on every exit path.
- Two reviewr panes on one worktree agree on turn boundaries, since both read the same worktree. Each still snapshots on its own poll clock, so their baselines can differ by the edits made between the two samples, and each shows its own. They write one shared ref, last writer winning, which only seeds the next reviewr pane to open.

## Non-goals

- No clipboard over SSH. The export targets the machine where the binary runs.
- No herdr socket subscription. Turn tracking polls.
- No per-agent attribution in `last-turn`. herdr reports no per-file authorship.

## Related specs

- [configuration](./config.md)
- [input](./input.md)
- [overview](./overview.md)
- [review-model](./review-model.md)
- [theme](./theme.md)
