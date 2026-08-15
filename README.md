# Herdr Preview

Herdr Preview is a diff-first right rail for [Herdr](https://herdr.dev). It shows an agent's
changes beside the current pane, lets you comment on line ranges, and sends the complete review
back to an agent through Herdr.

This fork uses [persiyanov/herdr-reviewr](https://github.com/persiyanov/herdr-reviewr) as its
review engine. The internal Rust crate remains `herdr-reviewr` to minimize divergence. The
installed `herdr-preview` executable keeps Preview panes distinct from upstream `herdr-reviewr`
panes. The plugin, actions, pane labels, config, and documentation use **Herdr Preview** and
`pi-dal.herdr-preview`.

![demo](assets/demo.gif)

## What it provides

- **Changes**: a structured, syntax-highlighted diff and changed-file tree.
- **Files**: the whole repository tree and a read-only content viewer.
- **PR**: pull request or merge request state, checks, description, and comments.
- **Scopes**: uncommitted changes by default, branch changes, or the last observed agent turn.
- **Files-only**: from any readable non-Git directory, a safe tree and read-only content browser rooted exactly there.
- **Review comments**: select lines, write notes, then send all notes to a Herdr agent or copy
  them to the clipboard.

The default experience opens on **Changes**, scope **uncommitted**, with the navigator on the
right. Herdr Preview never edits, stages, reverts, or commits repository files. Its only Git
writes are private review-engine refs under `refs/reviewr/`.

## Requirements

- Herdr 0.8.0 or newer
- Git
- Rust 1.97 for installation from source
- macOS or Linux with a truecolor terminal
- `gh`, `glab`, or `az` only when using the PR tab for its matching forge

## Install

Install the plugin directly from GitHub. Managed installation builds this fork's checked-out
source and never downloads an upstream release binary.

```bash
herdr plugin install pi-dal/herdr-preview
```

Open it in the current workspace:

```bash
herdr plugin action invoke open --plugin pi-dal.herdr-preview
```

The `worktree.created` event auto-opens a non-focusing Preview split by default. Set
`auto_open = false` in the plugin config to disable it.

### Local development link

`herdr plugin link` skips build steps, so build and install the local binary first:

```bash
git clone https://github.com/pi-dal/herdr-preview
cd herdr-preview
mise exec rust@1.97.1 -- cargo build --release
mkdir -p bin
./scripts/swap-binary.sh target/release/herdr-preview bin/herdr-preview
herdr plugin link .
```

Rebuild and replace `bin/herdr-preview` before reopening a linked Preview pane. An already open
pane keeps running its old binary image.

## Shortcuts

Preview has two intentional shortcut layers:

- **Herdr-global `Option` shortcuts** work while an agent pane has focus. Herdr routes them to
  Preview; it opens a stable right-side split when Preview is absent.
- **In-Preview bare keys** are for focused reading and editing. They are terminal-independent and
  remain the fastest workflow once the pane is open. Press `?` for only the keys valid in the
  current state.

### Herdr-global layer

`alt+` is macOS **Option** in Herdr config. This local profile reserves `alt+d` for Herdr's native
vertical split, so **Changes is `alt+shift+d`**. The other view, review, and navigation bindings
are mnemonic and are routed through Herdr even if Ghostty never emits a physical Alt sequence.

| group | keys | action |
| --- | --- | --- |
| Open | `alt+p` | toggle Preview (opens focused) |
| Views | `alt+shift+d` · `alt+f` · `alt+r` | Changes · Files · PR |
| Review | `alt+c` · `alt+l` · `alt+s` | comment · comment list · send |
| Display | `alt+u` · `alt+shift+r` | fold unchanged · refresh |
| Navigate changes | `alt+up` / `alt+down` | previous / next changed line |
| Navigate groups | `alt+left` / `alt+right` | previous / next change group |
| Navigate files | `alt+shift+b` / `alt+shift+n` | previous / next changed file |

Each entry uses Herdr's plugin-action shape:

```toml
[[keys.command]]
key = "alt+shift+d"
type = "plugin_action"
command = "pi-dal.herdr-preview.changes"
```

The tab, traversal, hide-unchanged, and refresh routes preserve the invoking agent focus.
`comment` and `comments` focus Preview before forwarding so the next keystroke edits Preview. `send`
adds no host focus change, then follows Preview's normal send behavior.
When Preview is absent, a route first opens its stable right-side non-focusing split, then forwards.
Human `open` and `toggle` still use `toggle_placement` and `toggle_direction` and take focus.
`close` sweeps every live Preview review-UI pane in the focused workspace.

When the focused pane is outside Git, manual actions open a **Files-only** Preview rooted exactly at
the focused directory. They never inspect unrelated panes, substitute another repository, or recursively
discover repositories. The same rule applies when a global shortcut opens Preview before forwarding its key.

## Agent interface: `peek`

Agents, including Pi, Codex, and Claude, invoke the stable action through Herdr:

```bash
herdr plugin action invoke peek --plugin pi-dal.herdr-preview
```

`peek` is idempotent. It opens exactly one right-side split beside the current pane with
`--no-focus`, regardless of human placement settings. If a Preview review UI is already open in
the workspace, it does nothing and does not focus or close that pane. Invalid config, missing
Herdr workspace context, or no unambiguous Git repository produces one bounded refusal.

Herdr Preview requires **no Pi extension and no Pi changes**. Agents control it through Herdr
plugin actions. Review comments return through Herdr pane and agent operations. In Files-only mode,
Git review, comments, and agent send are intentionally unavailable; file browsing, markdown preview,
wrapping, find, and refresh remain useful.

## Review flow

1. In **Changes**, choose a changed file and read its diff.
2. Press `Tab` to focus the diff, then `v` and movement keys to select lines.
3. Press `c` for a fast one-line note, or select with `v` and inspect the anchor strip before pressing `c`. Write the draft and press `Enter` to save.
4. Review exact inline cards with `n`/`N` or `l`. Press `s`, choose the target on the confirmation sheet, then press `Enter`; Preview adds the text to agent input and does not submit it.

Use `1`, `2`, and `3` for **Changes**, **Files**, and **PR**. Use `u`, `b`, and `t` for
**uncommitted**, **branch**, and **last turn** scopes. Press `?` for the complete context-aware
key list.

### Alt layout

The in-pane mnemonic uses `alt+d` Changes, `alt+f` Files, `alt+r` PR, `alt+c` comment,
`alt+l` list, `alt+s` send, `alt+shift+r` refresh, and `alt+h` help. `alt+up`/`alt+down` step
individual changed lines, `alt+left`/`alt+right` step change runs, and `alt+shift+up`/
`alt+shift+down` step files. `alt+u` hides unchanged context in Changes as expandable folds.
These are in-pane shortcuts. The global actions above are the route for Option chords that Herdr
intercepts before Preview sees terminal input.

`alt+` is the config spelling for macOS's **Option** key. Terminals only deliver these shortcuts
when Option is configured to send Alt/Meta, so retain the existing bare keys or rebind them if
Option produces characters instead. In Changes, `c` makes a one-line selection and opens the
composer; `v` or a content-row click/drag selects a range, then the inline `Comment` control or
`c` opens it. A fold expands instead of selecting, and clicking a saved comment card edits it.

The plugin config is:

```text
~/.config/herdr/plugins/config/pi-dal.herdr-preview/config.toml
```

Configuration, keybindings, pane placement, forge hosts, search, markdown preview, and detailed
behavior remain documented in the living [`specs/`](specs/) and contributor documentation.

### Navigator tree and semantic codes

Click anywhere on a directory row to expand or collapse it. In the focused navigator, `→` expands
then enters its first child, `←` collapses then returns to its parent, and `Enter` toggles a
directory or opens a file. These controls work in Files-only and Git Files at every navigator
position.

Directory rows use only their disclosure arrow and trailing slash. File rows reserve a compact
file-kind slot separate from the colored Git marker: `plain` uses `s/c/d/j/t/i/m/p/b/.` for source,
config, document, data, template, image, media, package, binary, and other. Resolution is lexical
only, with user exact filename, bundled exact filename, longest compound suffix, extension, then a
category fallback; it never alters Search results or inspects MIME data. `file_icons = "plain"` is
the font-safe default; `"emoji"` is an opt-in standard-Unicode emoji mode and `"nerd"` remains an
explicit compatibility mode for a compatible Nerd Font. Preview cannot detect terminal fonts or
promise emoji/Nerd glyph advance widths, so choose `plain` or `none` when a font-safe presentation
is required. Legacy `"unicode"` normalizes to `"plain"`.

Small lexical overrides select safe built-in identities, not glyph strings or theme files:
```toml
file_icons = "emoji"
[file_icon_overrides.names]
"Containerfile" = "docker"
[file_icon_overrides.extensions]
"d.mts" = "typescript"
astro = "vue"
```
Names are basenames and extensions are dot-separated suffixes without a leading dot. The complete
closed ID list and whole-file validation rules are in [`specs/config.md`](specs/config.md). Press
`?` in a file tab for the active mode's concise legend. The optional slot yields before the
filename, Git marker, or change stats at narrow widths; emoji reserve their measured width safely.

## Build and test

Use the pinned Rust toolchain through mise:

```bash
mise exec rust@1.97.1 -- cargo fmt --all --check
mise exec rust@1.97.1 -- cargo clippy --all-targets --all-features -- -D warnings
mise exec rust@1.97.1 -- cargo test --all-features
mise exec rust@1.97.1 -- cargo build --release
```

`just install` is the local-link convenience command. It release-builds and replaces
`bin/herdr-preview` through a fresh inode. See [CONTRIBUTING.md](CONTRIBUTING.md) and
[docs/qa-install.md](docs/qa-install.md) for the development workflow.

## Limitations

- Comments live in memory. Send or copy them before closing the pane.
- Last-turn tracking polls Herdr, so a turn shorter than the polling interval can be missed.
- The PR tab is read-only and requires the authenticated CLI for GitHub, GitLab, or Azure DevOps.
- Files over the review engine's size budget and binary files do not receive line comments. Selected
  PNG, JPEG, WebP, and GIF files get a bounded, static truecolor halfblock preview; SVG currently
  reports that its preview is unavailable rather than parsing untrusted XML. Image previews are
  read-only and use no terminal graphics escape protocol or external viewer.
- Truecolor and Unicode box drawing are required. Windows is not supported.
- The UI does not edit files, stage changes, call Pi RPC, or write to a forge.

## Upstream and license

Herdr Preview is a fork of
[`persiyanov/herdr-reviewr`](https://github.com/persiyanov/herdr-reviewr), created by Dmitry
Persiyanov. The upstream architecture, review engine, specifications, and history remain
explicitly attributed and are intentionally preserved.

Licensed under the upstream [MIT License](LICENSE). The original copyright notice remains
unchanged. Bundled theme files retain their own licenses as documented with the assets.
