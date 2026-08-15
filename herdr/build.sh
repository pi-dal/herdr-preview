#!/usr/bin/env bash
# Herdr managed-install build step: compile this checkout and install the Preview-only
# herdr-preview binary through the shared fresh-inode swap helper.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
SOURCE="$TARGET_DIR/release/herdr-preview"
DEST="$ROOT/bin/herdr-preview"

mise exec rust@1.97.1 -- cargo build --release --locked --manifest-path "$ROOT/Cargo.toml"
mkdir -p "$ROOT/bin"
"$ROOT/scripts/swap-binary.sh" "$SOURCE" "$DEST"
echo "Herdr Preview: installed $DEST"

# Runtime actions re-point stable links after Herdr moves its staging checkout into place.
LINK_ROOT="${HERDR_PLUGIN_ROOT:-$ROOT}"
link_binary() {
  local dir="$1"
  if mkdir -p "$dir" 2>/dev/null && { [ -L "$dir/herdr-preview" ] || [ ! -e "$dir/herdr-preview" ]; } &&
    ln -sfn "$LINK_ROOT/bin/herdr-preview" "$dir/herdr-preview" 2>/dev/null; then
    echo "Herdr Preview: linked $dir/herdr-preview"
  else
    echo "Herdr Preview: warning: could not link $dir/herdr-preview" >&2
  fi
}
link_binary "$HOME/.local/state/herdr/plugins/pi-dal.herdr-preview/bin"
if [ -d "$HOME/.local/bin" ]; then
  link_binary "$HOME/.local/bin"
fi
