#!/usr/bin/env sh
# Uninstall branch-graph: remove the symlink created by install.sh.
#
# Usage:
#   ./uninstall.sh
#   BINDIR=/usr/local/bin ./uninstall.sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
SOURCE="$SCRIPT_DIR/branch-graph.js"
BINDIR="${BINDIR:-$HOME/.local/bin}"
TARGET="$BINDIR/branch-graph"

if [ -L "$TARGET" ]; then
  # Only remove if it points at our script (don't clobber an unrelated binary).
  LINK_DEST=$(readlink "$TARGET" || true)
  if [ "$LINK_DEST" = "$SOURCE" ]; then
    rm -f "$TARGET"
    echo "Removed: $TARGET"
  else
    echo "Skipped: $TARGET points to $LINK_DEST (not this project); left untouched." >&2
    exit 1
  fi
elif [ -e "$TARGET" ]; then
  echo "Skipped: $TARGET exists but is not a symlink; left untouched." >&2
  exit 1
else
  echo "Nothing to remove: $TARGET not found."
fi
