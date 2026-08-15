#!/usr/bin/env bash
# Herdr Preview pane actions and event hook (specs/herdr-host.md).
#
#   pane.sh toggle      open a Preview pane, or close every one if any is open
#   pane.sh open        open a Preview pane, no-op if one is open
#   pane.sh close       close every Preview pane, no-op if none
#   pane.sh peek        agent interface: idempotent, right split, never takes focus
#   pane.sh forward KEY host shortcut: route a canonical Herdr key to Preview
#   pane.sh auto-open   worktree.created hook: open, gated by auto_open and placement
#
# A Preview pane is any pane running the review UI in its foreground process group, read
# live per pane (specs/herdr-host.md, Pane identity). The `Preview` label is display only
# and never read. There is no state file. Actions refuse loudly (exit 1, one stderr line)
# and report successes on stdout; a refused event reports its config error through stderr
# for herdr's plugin log.
set -uo pipefail

# herdr runs plugin commands with a minimal PATH; ensure jq/git resolve on common installs.
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:${PATH:-}"

mode="${1:-toggle}"
H="${HERDR_BIN_PATH:-herdr}"

# Validate the whole plugin config before reading workspace state or taking any action. The Rust
# binary owns TOML parsing and defaults, so every plugin entry point shares exactly one contract.
if [ -n "${HERDR_PREVIEW_BIN:-}" ]; then
  PREVIEW="$HERDR_PREVIEW_BIN"
elif [ -n "${HERDR_PLUGIN_ROOT:-}" ]; then
  PREVIEW="$HERDR_PLUGIN_ROOT/bin/herdr-preview"
else
  PREVIEW="herdr-preview"
fi
config_json=$("$PREVIEW" --resolve-plugin-config 2>&1)
config_status=$?
if [ "$config_status" -ne 0 ]; then
  [ -n "$config_json" ] || config_json="Herdr Preview: configuration validation failed"
  printf '%s\n' "$config_json" >&2
  exit 1
fi
# One normalized-config field. The has() form reads a present `false` as a value, where a
# bare `jq -e` would read it as failure — one template serves strings and booleans alike.
cfg_field() {
  printf '%s' "$config_json" | jq -r "if has(\"$1\") then .$1 else error(\"missing $1\") end" 2>/dev/null
}
unreadable_config() {
  printf 'Herdr Preview: normalized configuration is unreadable\n' >&2
  exit 1
}
placement=$(cfg_field toggle_placement) || unreadable_config
direction=$(cfg_field toggle_direction) || unreadable_config
auto_open=$(cfg_field auto_open) || unreadable_config

# The stable launch paths track the live plugin root from here, not from the install step:
# the build step runs in a staging checkout that herdr renames afterwards, so only a runtime
# invocation knows the real root (specs/herdr-host.md, Install paths). Best effort — never
# fails an action, and never replaces anything but a symlink.
if [ -n "${HERDR_PLUGIN_ROOT:-}" ] && [ -x "$HERDR_PLUGIN_ROOT/bin/herdr-preview" ]; then
  for link_dir in "$HOME/.local/state/herdr/plugins/pi-dal.herdr-preview/bin" "$HOME/.local/bin"; do
    if [ "$link_dir" = "$HOME/.local/bin" ] && [ ! -d "$link_dir" ]; then
      continue
    fi
    mkdir -p "$link_dir" 2>/dev/null || continue
    if [ -L "$link_dir/herdr-preview" ] || [ ! -e "$link_dir/herdr-preview" ]; then
      ln -sfn "$HERDR_PLUGIN_ROOT/bin/herdr-preview" "$link_dir/herdr-preview" 2>/dev/null || :
    fi
  done
fi

# Event policy gates the event alone: explicit actions ignore it. This is after validation but
# before workspace or pane inspection, so a disabled event performs no normal work.
if [ "$mode" = auto-open ]; then
  [ "$auto_open" = "false" ] && exit 0
  if [ "$placement" != "split" ] && [ "$placement" != "tab" ]; then
    exit 0
  fi
fi

refuse() {
  [ "$mode" = auto-open ] && exit 0
  printf 'Herdr Preview: %s\n' "$1" >&2
  exit 1
}

# Forward only canonical keys that the manifest owns. This keeps a malformed action invocation
# from becoming an arbitrary pane input write. `comment` and `comments` focus Preview because
# their next input is interactive; every other routed action preserves the invoking pane's focus
# (specs/herdr-host.md, Pane actions).
forward_key=""
forward_focus=false
if [ "$mode" = forward ]; then
  forward_key="${2:-}"
  case "$forward_key" in
  alt+d | alt+f | alt+r | alt+s | alt+shift+r | alt+u | alt+up | alt+down | alt+shift+up | alt+shift+down | alt+left | alt+right) ;;
  alt+c | alt+l) forward_focus=true ;;
  *) refuse "unknown forward key '$forward_key'" ;;
  esac
fi

ws="${HERDR_WORKSPACE_ID:-}"
pane="${HERDR_PANE_ID:-}"
cwd=""
[ -n "${HERDR_PLUGIN_CONTEXT_JSON:-}" ] &&
  cwd=$(printf '%s' "$HERDR_PLUGIN_CONTEXT_JSON" | jq -r '.focused_pane_cwd // .workspace_cwd // empty' 2>/dev/null)

# The event fires without a focused pane; target the fresh workspace from its payload
# (worktree.created shape: .data.workspace.workspace_id, .data.workspace.worktree.checkout_path).
if [ "$mode" = auto-open ] && [ -n "${HERDR_PLUGIN_EVENT_JSON:-}" ]; then
  ev="$HERDR_PLUGIN_EVENT_JSON"
  ws=$(printf '%s' "$ev" | jq -r '.data.workspace.workspace_id // .data.worktree.open_workspace_id // empty' 2>/dev/null)
  cwd=$(printf '%s' "$ev" | jq -r '.data.workspace.worktree.checkout_path // .data.worktree.path // empty' 2>/dev/null)
  pane=""
fi

[ -n "$ws" ] || refuse "no workspace context (invoke from inside herdr)"

# One pane-list snapshot serves the whole run. A failed or unreadable listing must not read
# as "no Preview pane" — that would stack a duplicate on toggle and false-succeed a close.
panes_json=$("$H" pane list --workspace "$ws" 2>/dev/null) && [ -n "$panes_json" ] &&
  printf '%s' "$panes_json" | jq -e '.result.panes' >/dev/null 2>&1 ||
  refuse "herdr pane list failed for $ws"

# A Preview pane runs the review UI in its foreground process group (specs/herdr-host.md,
# Pane identity). A wrapped launch (`cargo run`) counts through its child; a flag run
# (`--resolve-plugin-config`) never counts. The executable name in `argv0`/`argv[0]`
# decides, never `name`: that field is a rewritable process title (docs/herdr-api-notes.md).
# The flag exclusion mirrors the dispatch in `src/main.rs`, which recognizes the flag
# anywhere in argv — a future non-UI flag must land in both halves.
# Takes the pane id and a scratch file for the call's stderr, which stays out of the JSON
# handed to jq — a successful call with an advisory on stderr must not read as unreadable.
# Returns 0 for Preview, 1 for an ordinary pane, 3 for upstream reviewr, and 4 for a pane
# that exited between the list and this read. Return 2 is an unreadable process identity and
# refuses the whole action. Discovery may use only ordinary panes: a Preview is already open,
# an upstream reviewr belongs to another plugin, and a gone pane has no live identity.
is_preview_pane() {
  if ! info=$("$H" pane process-info --pane "$1" 2>"$2"); then
    case "$(cat "$2" 2>/dev/null)" in
    *pane_not_found*) return 4 ;;
    *) return 2 ;;
    esac
  fi
  # No `?` after the key path: an envelope missing `foreground_processes` is a shape
  # failure and must refuse, never read as "no Preview pane" — only a present-but-empty
  # process list may count zero.
  kind=$(printf '%s' "$info" | jq -r '
    def base: split("/") | last;
    [.result.process_info.foreground_processes[]
      | { executable: ((.argv0 // .argv[0] // "") | base),
          config: ((.argv // []) | index("--resolve-plugin-config") != null) }]
    | if any(.[]; .executable == "herdr-preview" and (.config | not)) then "preview"
      elif any(.[]; .executable == "herdr-reviewr") then "upstream"
      else "ordinary"
      end' 2>/dev/null) || return 2
  case "$kind" in
  preview) return 0 ;;
  ordinary) return 1 ;;
  upstream) return 3 ;;
  *) return 2 ;;
  esac
}

# The workspace's Preview panes: any tab, any placement, however they were launched
# (HH-LAUNCHER-BLIND) — the actions and a layout's own pane converge on one set. The
# per-pane reads run concurrently, so the sweep costs one process-info round-trip of
# wall clock, not one per pane in the workspace. Each probe reports through a marker
# file keyed by its position in the list, so nothing rests on a pane id's spelling.
pane_list=$(printf '%s' "$panes_json" | jq -r '.result.panes[].pane_id // empty' 2>/dev/null)
probe_dir=$(mktemp -d) || refuse "cannot create a temp dir"
trap 'rm -rf "$probe_dir"' EXIT
i=0
while IFS= read -r p; do
  [ -n "$p" ] || continue
  {
    is_preview_pane "$p" "$probe_dir/$i.err"
    case $? in
    0) : >"$probe_dir/$i.preview" ;;
    2) : >"$probe_dir/$i.unreadable" ;;
    3) : >"$probe_dir/$i.upstream" ;;
    4) : >"$probe_dir/$i.gone" ;;
    esac
    # Written last, so its presence means the probe settled. A probe that never reports —
    # a failed fork or marker write — must refuse, never read as "not a Preview pane".
    : >"$probe_dir/$i.done"
  } &
  i=$((i + 1))
done <<EOF
$pane_list
EOF
wait

existing=""
unreadable=0
i=0
while IFS= read -r p; do
  [ -n "$p" ] || continue
  [ -e "$probe_dir/$i.done" ] || unreadable=1
  [ -e "$probe_dir/$i.unreadable" ] && unreadable=1
  [ -e "$probe_dir/$i.preview" ] && existing="$existing$p"$'\n'
  i=$((i + 1))
done <<EOF
$pane_list
EOF
[ "$unreadable" -eq 0 ] || refuse "herdr pane process-info failed in $ws"

# Manual opens and forwards reuse a known Preview only when it is rooted at the focused
# directory. A Preview whose live directory is absent keeps the existing compatibility
# behavior, but a known different root can never steal a Files-only action from its focus.
action_existing="$existing"
if [ "$mode" != auto-open ] && [ "$mode" != close ]; then
  fp=$(printf '%s' "${HERDR_PLUGIN_CONTEXT_JSON:-}" | jq -r '.focused_pane_id // empty' 2>/dev/null)
  focused_live=""
  [ -z "$fp" ] || focused_live=$(printf '%s' "$panes_json" |
    jq -r --arg p "$fp" 'first(.result.panes[] | select(.pane_id == $p) | .foreground_cwd // empty)' 2>/dev/null)
  cwd="${focused_live:-$cwd}"
  if [ -n "$cwd" ]; then
    compatible=""
    while IFS= read -r candidate; do
      [ -n "$candidate" ] || continue
      candidate_cwd=$(printf '%s' "$panes_json" | jq -r --arg p "$candidate" \
        'first(.result.panes[] | select(.pane_id == $p) | .foreground_cwd // empty)' 2>/dev/null)
      if [ -z "$candidate_cwd" ] || [ "$candidate_cwd" = "$cwd" ]; then
        compatible="$compatible$candidate"$'\n'
      fi
    done <<EOF
$existing
EOF
    action_existing="$compatible"
  elif [ -z "$existing" ]; then
    refuse "no focused pane directory"
  fi
fi

# Plain `pane close`, not `plugin pane close`: the live process read reaches a pane the
# plugin-pane registry forgot after a herdr restart, and a layout-launched pane was never
# in that registry at all (specs/herdr-host.md). A close refused because the pane is gone
# lost a benign race — the pane exited between the read and the close, the same end
# state — so the sweep still converges and exits 0. A close failing any other way names a
# pane that may still be running, so the sweep finishes the rest and then refuses rather
# than reporting that pane closed (specs/herdr-host.md, Failure semantics).
close_all() {
  closed=""
  failed=""
  while IFS= read -r p; do
    [ -n "$p" ] || continue
    if err=$("$H" pane close "$p" 2>&1 >/dev/null); then
      closed="$closed $p"
    else
      case "$err" in
      *pane_not_found*) closed="$closed $p" ;;
      *) failed="$failed $p" ;;
      esac
    fi
  done <<EOF
$existing
EOF
  [ -z "$failed" ] || refuse "herdr pane close failed for$failed in $ws"
  printf 'Herdr Preview: closed%s in %s\n' "$closed" "$ws"
}

# A target comes only from the live foreground-process identity sweep, or from the fresh
# Preview pane-open result below. Focus before sending only when the routed action starts an
# interaction. A focus or send failure refuses before any later input write.
forward_to_preview() {
  target="$1"
  if [ "$forward_focus" = true ]; then
    "$H" plugin pane focus "$target" >/dev/null 2>&1 ||
      refuse "herdr plugin pane focus failed for $target in $ws"
  fi
  "$H" pane send-keys "$target" "$forward_key" >/dev/null 2>&1 ||
    refuse "herdr pane send-keys failed for $target in $ws"
  printf 'Herdr Preview: forwarded %s to %s in %s\n' "$forward_key" "$target" "$ws"
}

case "$mode" in
close)
  [ -n "$existing" ] || { printf 'Herdr Preview: close: nothing open in %s\n' "$ws"; exit 0; }
  close_all
  exit 0
  ;;
toggle)
  if [ -n "$action_existing" ]; then
    close_all
    exit 0
  fi
  ;;
open | peek | auto-open)
  if [ -n "$action_existing" ]; then
    if [ "$mode" = open ] || [ "$mode" = peek ]; then
      printf '%s: Preview already open (%s) in %s\n' "$mode" "$(printf '%s' "$action_existing" | tr '\n' ' ' | sed 's/ $//')" "$ws"
    fi
    exit 0
  fi
  ;;
forward)
  if [ -n "$action_existing" ]; then
    forward_to_preview "$(printf '%s' "$action_existing" | sed -n '1p')"
    exit 0
  fi
  ;;
*)
  refuse "unknown mode '$mode' (toggle | open | close | peek | forward | auto-open)"
  ;;
esac

# The focused directory was resolved above before action reuse. The event uses its payload
# directory directly and does not participate in manual pane matching.
if [ "$mode" = auto-open ]; then
  [ -n "$cwd" ] || refuse "no event directory"
fi

# Human opens take focus. The event and stable agent `peek` interface never do
# (HH-PEEK-RIGHT-NONFOCUSING).
focus=--no-focus
[ "$mode" != auto-open ] && [ "$mode" != peek ] && [ "$mode" != forward ] && focus=--focus

# Peek and host forwarding ignore human placement configuration and always open a right split
# beside the invoking or focused pane without taking focus (HH-PEEK-RIGHT-NONFOCUSING).
if [ "$mode" = peek ] || [ "$mode" = forward ]; then
  placement=split
  direction=right
  fp=$(printf '%s' "${HERDR_PLUGIN_CONTEXT_JSON:-}" | jq -r '.focused_pane_id // empty' 2>/dev/null)
  [ -z "$fp" ] || pane="$fp"
fi

# Placement decides the pane-open shape (spec: Pane placement). A split or zoomed
# open attaches to the focused pane, else the workspace's first pane.
case "$placement" in
split | zoomed)
  if [ -z "$pane" ]; then
    pane=$(printf '%s' "$panes_json" | jq -r '.result.panes[0].pane_id // empty' 2>/dev/null)
  fi
  [ -n "$pane" ] || refuse "no pane to attach to in $ws"
  set -- --placement "$placement" --target-pane "$pane"
  [ "$placement" = "split" ] && set -- "$@" --direction "$direction"
  ;;
tab)
  set -- --placement tab --workspace "$ws"
  ;;
overlay)
  set -- --placement overlay
  ;;
*)
  refuse "unreachable placement '$placement'" # guard against a future value leaking $@
  ;;
esac

open_json=$("$H" plugin pane open --plugin "${HERDR_PLUGIN_ID:-pi-dal.herdr-preview}" --entrypoint pane \
  "$@" --cwd "$cwd" "$focus" 2>/dev/null)
new=$(printf '%s' "$open_json" | jq -r '.result.plugin_pane.pane.pane_id // empty' 2>/dev/null)
[ -n "$new" ] || refuse "herdr plugin pane open failed"

# A tab open lands in a fresh tab that herdr labels with a bare index; name it
# after the plugin so the tab bar reads "Preview" (specs/herdr-host.md). Cosmetic: a
# failed rename never fails an open that already succeeded.
if [ "$placement" = tab ]; then
  tab=$(printf '%s' "$open_json" | jq -r '.result.plugin_pane.pane.tab_id // empty' 2>/dev/null)
  [ -z "$tab" ] || "$H" tab rename "$tab" Preview >/dev/null 2>&1
fi
if [ "$mode" = forward ]; then
  forward_to_preview "$new"
elif [ "$mode" != auto-open ]; then
  printf 'Herdr Preview: opened %s (%s) in %s\n' "$new" "$placement" "$ws"
fi
