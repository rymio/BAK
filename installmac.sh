#!/bin/sh
# Install bak and bak-server on macOS.
#
# Run from an unpacked release archive (binaries next to this script) or from
# a source checkout (builds with cargo).
#
#   ./installmac.sh                 install to /usr/local/bin (asks for sudo if needed)
#   ./installmac.sh --user          install to ~/.local/bin, no sudo
#   ./installmac.sh --prefix DIR    install to DIR/bin
#   ./installmac.sh --client-only   install bak only, not bak-server
#   ./installmac.sh --uninstall     remove installed binaries
set -eu

BINARIES="bak bak-server"
PREFIX="/usr/local"
USER_MODE=0
UNINSTALL=0
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() { sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0; }

while [ $# -gt 0 ]; do
    case "$1" in
        --user) USER_MODE=1; PREFIX="$HOME/.local" ;;
        --prefix) [ $# -ge 2 ] || die "--prefix needs a directory"; PREFIX="$2"; shift ;;
        --client-only) BINARIES="bak" ;;
        --uninstall) UNINSTALL=1 ;;
        -h|--help) usage ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
    shift
done

[ "$(uname -s)" = "Darwin" ] || die "this installer is for macOS; use installlinux.sh on Linux"

BIN_DIR="$PREFIX/bin"

# Run a command, with sudo only when the target directory is not writable.
SUDO=""
need_sudo() {
    dir="$BIN_DIR"
    while [ ! -d "$dir" ]; do dir=$(dirname "$dir"); done
    if [ ! -w "$dir" ] || { [ -d "$BIN_DIR" ] && [ ! -w "$BIN_DIR" ]; }; then
        command -v sudo >/dev/null 2>&1 || die "$BIN_DIR is not writable and sudo is missing; try --user"
        SUDO="sudo"
        say "Installing to $BIN_DIR needs administrator rights; you may be asked for your password."
    fi
}

if [ "$UNINSTALL" -eq 1 ]; then
    need_sudo
    for b in bak bak-server; do
        if [ -e "$BIN_DIR/$b" ]; then
            $SUDO rm -f "$BIN_DIR/$b"
            say "removed $BIN_DIR/$b"
        fi
    done
    say "Backups, repositories and .bak/ project folders were not touched."
    exit 0
fi

# Find binaries: release archive layout first, then a source build.
SRC_DIR=""
for d in "$SCRIPT_DIR" "$SCRIPT_DIR/bin" "$SCRIPT_DIR/target/release"; do
    if [ -x "$d/bak" ]; then SRC_DIR="$d"; break; fi
done

if [ -z "$SRC_DIR" ]; then
    [ -f "$SCRIPT_DIR/Cargo.toml" ] || die "no bak binaries next to this script and no source checkout to build from"
    command -v cargo >/dev/null 2>&1 || die "cargo not found. Install Rust from https://rustup.rs, then run this again"
    say "Building release binaries (this takes a few minutes the first time)..."
    (cd "$SCRIPT_DIR" && cargo build --release --locked -p bak-cli -p bak-server)
    SRC_DIR="$SCRIPT_DIR/target/release"
fi

for b in $BINARIES; do
    [ -x "$SRC_DIR/$b" ] || die "$SRC_DIR/$b is missing"
    file "$SRC_DIR/$b" | grep -q "Mach-O" || die "$SRC_DIR/$b is not a macOS binary (wrong archive?)"
done

need_sudo
$SUDO mkdir -p "$BIN_DIR"
for b in $BINARIES; do
    $SUDO install -m 0755 "$SRC_DIR/$b" "$BIN_DIR/$b"
    # Binaries from a downloaded archive carry a quarantine flag that makes
    # Gatekeeper block them; clear it on the installed copy.
    $SUDO xattr -d com.apple.quarantine "$BIN_DIR/$b" 2>/dev/null || true
    say "installed $BIN_DIR/$b"
done

# Make sure the directory is on PATH.
case ":$PATH:" in
    *":$BIN_DIR:"*) ON_PATH=1 ;;
    *) ON_PATH=0 ;;
esac
if [ "$ON_PATH" -eq 0 ]; then
    RC="$HOME/.zshrc"
    case "${SHELL:-}" in */bash) RC="$HOME/.bash_profile" ;; esac
    LINE="export PATH=\"$BIN_DIR:\$PATH\""
    if ! grep -qsF "$LINE" "$RC"; then
        printf '\n# Added by the bak installer\n%s\n' "$LINE" >> "$RC"
        say "added $BIN_DIR to PATH in $RC"
    fi
    say "Open a new terminal (or run: $LINE) to use bak."
    export PATH="$BIN_DIR:$PATH"
fi

say ""
for b in $BINARIES; do "$BIN_DIR/$b" --version; done
say ""
say "Done. Start with:  cd your-project && bak init"
