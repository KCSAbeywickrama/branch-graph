#!/usr/bin/env sh
# Install branch-graph: symlink the CLI into a bin directory on your PATH.
#
# Usage:
#   ./install.sh                 # install to ~/.local/bin (default)
#   BINDIR=/usr/local/bin ./install.sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
SOURCE="$SCRIPT_DIR/branch-graph.js"
BINDIR="${BINDIR:-$HOME/.local/bin}"
TARGET="$BINDIR/branch-graph"

if [ ! -f "$SOURCE" ]; then
  echo "install: cannot find branch-graph.js next to this script" >&2
  exit 1
fi

chmod +x "$SOURCE"
mkdir -p "$BINDIR"

# Replace any existing link/file at the target.
if [ -e "$TARGET" ] || [ -L "$TARGET" ]; then
  rm -f "$TARGET"
fi
ln -s "$SOURCE" "$TARGET"

echo "Installed: $TARGET -> $SOURCE"

# Warn if BINDIR is not on PATH.
case ":$PATH:" in
  *":$BINDIR:"*) ;;
  *)
    echo
    echo "NOTE: $BINDIR is not on your PATH."
    echo "Add this to your shell profile (e.g. ~/.zshrc):"
    echo "  export PATH=\"$BINDIR:\$PATH\""
    ;;
esac

echo
echo "Try it: branch-graph        (or inside Claude Code: !branch-graph)"
