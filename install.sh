#!/usr/bin/env sh
# Install branch-graph.
#
# Downloads a prebuilt static binary from GitHub Releases for this platform and
# verifies its SHA-256 before installing. Falls back to building with cargo when
# no prebuilt binary matches, which is why the download and build paths live in
# one script instead of two.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/KCSAbeywickrama/branch-graph/rust/install.sh | sh
#   ./install.sh                        # latest release into ~/.local/bin
#   BINDIR=/usr/local/bin ./install.sh  # choose another bin dir
#   VERSION=v1.0.0 ./install.sh         # pin a specific release
#   FROM_SOURCE=1 ./install.sh          # skip the download, build locally
#
# Gatekeeper note: these binaries are ad-hoc signed, not notarized. macOS only
# quarantines files stamped by the downloading app (browsers, Mail, AirDrop);
# curl and wget do not set that attribute, so a downloaded binary runs as-is.
set -eu

REPO=KCSAbeywickrama/branch-graph
BIN=branch-graph
ALIAS=cbg

BINDIR="${BINDIR:-$HOME/.local/bin}"
VERSION="${VERSION:-latest}"
FROM_SOURCE="${FROM_SOURCE:-0}"

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd -P) || SCRIPT_DIR=.

die() { echo "install: $*" >&2; exit 1; }

# Map uname output onto a release target triple. Returns non-zero for anything
# with no prebuilt asset, which routes the caller to the source build.
detect_target() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Darwin)
      case "$arch" in
        arm64|aarch64) echo aarch64-apple-darwin ;;
        x86_64)        echo x86_64-apple-darwin ;;
        *)             return 1 ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64|amd64)  echo x86_64-unknown-linux-musl ;;
        aarch64|arm64) echo aarch64-unknown-linux-musl ;;
        *)             return 1 ;;
      esac
      ;;
    *) return 1 ;;
  esac
}

# curl and wget are both common enough that requiring one specifically causes
# avoidable failures. -f matters: without it an HTTP error page is saved as
# if it were the tarball.
fetch() {
  url=$1 out=$2
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --proto '=https' --tlsv1.2 "$url" -o "$out"
  elif command -v wget >/dev/null 2>&1; then
    wget -q --https-only "$url" -O "$out"
  else
    die "neither curl nor wget found on PATH."
  fi
}

# GNU coreutils, BSD/perl shasum and openssl all disagree on flags, so the hash
# is computed here and compared as a string rather than via `shasum -c`.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    return 1
  fi
}

# Install one real file plus a relative symlink for the alias, so the two names
# cannot drift and the second copy costs no disk.
place() {
  built=$1
  mkdir -p "$BINDIR"
  install -m 755 "$built" "$BINDIR/$BIN"
  rm -f "$BINDIR/$ALIAS"
  ln -s "$BIN" "$BINDIR/$ALIAS"
  echo "Installed: $BINDIR/$BIN"
  echo "Installed: $BINDIR/$ALIAS -> $BIN"
}

build_from_source() {
  [ -f "$SCRIPT_DIR/Cargo.toml" ] || die "no Cargo.toml next to this script, so there is nothing to build.
  Clone the repo and re-run, or install a prebuilt release:
    git clone https://github.com/$REPO && cd branch-graph && ./install.sh"

  command -v cargo >/dev/null 2>&1 || die "cargo not found on PATH.
  Install a Rust toolchain (https://rustup.rs), or if you just installed one,
  start a new shell or run: . \"\$HOME/.cargo/env\""

  echo "Building release binary..."
  ( cd "$SCRIPT_DIR" && cargo build --release )

  built="$SCRIPT_DIR/target/release/$BIN"
  [ -x "$built" ] || die "build succeeded but $built is missing"

  # Copied, not symlinked: an install should survive `cargo clean`. Use
  # ./install-dev.sh when you want the live symlink into target/release.
  place "$built"
}

install_from_release() {
  target=$1
  asset="$BIN-$target.tar.gz"

  if [ "$VERSION" = latest ]; then
    base="https://github.com/$REPO/releases/latest/download"
  else
    base="https://github.com/$REPO/releases/download/$VERSION"
  fi

  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT INT TERM

  echo "Downloading $asset ($VERSION)..."
  fetch "$base/$asset" "$tmp/$asset" || return 1
  fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || return 1

  expected=$(awk -v f="$asset" '$2 == f || $2 == "*"f {print $1; exit}' "$tmp/SHA256SUMS")
  [ -n "$expected" ] || die "$asset has no entry in SHA256SUMS; refusing to install unverified."

  actual=$(sha256_of "$tmp/$asset") || die "no sha256sum, shasum or openssl available to verify the download."
  [ "$actual" = "$expected" ] || die "checksum mismatch for $asset.
  expected: $expected
  actual:   $actual
  Refusing to install. Report this if it persists."

  echo "Checksum verified."
  tar -xzf "$tmp/$asset" -C "$tmp" --strip-components=1
  [ -x "$tmp/$BIN" ] || die "archive did not contain an executable $BIN"

  place "$tmp/$BIN"
}

main() {
  if [ "$FROM_SOURCE" != 0 ]; then
    build_from_source
  elif ! target=$(detect_target); then
    echo "No prebuilt binary for $(uname -s)/$(uname -m); building from source."
    build_from_source
  elif ! install_from_release "$target"; then
    echo "Download failed (no release for this version, or no network)." >&2
    echo "Falling back to a source build." >&2
    build_from_source
  fi

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
}

main "$@"
