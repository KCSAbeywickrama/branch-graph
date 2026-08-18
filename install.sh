#!/usr/bin/env sh
# Install branch-graph: build the release binary and put it on your PATH.
#
# Usage:
#   ./install.sh                 # build, then install to ~/.local/bin (default)
#   BINDIR=/usr/local/bin ./install.sh
#   LINK=1 ./install.sh          # symlink target/release instead of copying, so a
#                                # later `cargo build --release` updates it in place
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
BINDIR="${BINDIR:-$HOME/.local/bin}"
BUILT="$SCRIPT_DIR/target/release/branch-graph"

if ! command -v cargo >/dev/null 2>&1; then
  echo "install: cargo not found on PATH." >&2
  echo "  Install a Rust toolchain (https://rustup.rs), or if you just installed one," >&2
  echo "  start a new shell or run: . \"\$HOME/.cargo/env\"" >&2
  exit 1
fi

echo "Building release binary..."
( cd "$SCRIPT_DIR" && cargo build --release )

if [ ! -x "$BUILT" ]; then
  echo "install: build succeeded but $BUILT is missing" >&2
  exit 1
fi

mkdir -p "$BINDIR"

# Install under both names: `branch-graph` and the short alias `cbg`.
for name in branch-graph cbg; do
  target="$BINDIR/$name"
  if [ -e "$target" ] || [ -L "$target" ]; then
    rm -f "$target"
  fi
  if [ "${LINK:-}" = "1" ]; then
    ln -s "$BUILT" "$target"
    echo "Installed: $target -> $BUILT"
  else
    cp "$BUILT" "$target"
    chmod +x "$target"
    echo "Installed: $target"
  fi
done

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
echo "Try it: branch-graph  or  cbg    (or inside Claude Code: !branch-graph  or !cbg)"
