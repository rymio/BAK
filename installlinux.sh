#!/bin/sh
# Install bak and bak-server on Linux.
#
# Run from an unpacked release archive (binaries next to this script) or from
# a source checkout (builds with cargo).
#
#   ./installlinux.sh                   install to /usr/local/bin (uses sudo if needed)
#   ./installlinux.sh --user            install to ~/.local/bin, no sudo
#   ./installlinux.sh --prefix DIR      install to DIR/bin
#   ./installlinux.sh --client-only     install bak only, not bak-server
#   ./installlinux.sh --server-service HOSTNAME
#                                       also run bak-server as a systemd service on
#                                       port 8000 with TLS, append-only and private
#                                       repositories, storing data in /srv/bak
#   ./installlinux.sh --uninstall       remove installed binaries (and the service)
set -eu

BINARIES="bak bak-server"
PREFIX="/usr/local"
UNINSTALL=0
SERVICE_HOST=""
DATA_DIR="/srv/bak"
SERVICE_USER="bak"
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() { sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0; }

while [ $# -gt 0 ]; do
    case "$1" in
        --user) PREFIX="$HOME/.local" ;;
        --prefix) [ $# -ge 2 ] || die "--prefix needs a directory"; PREFIX="$2"; shift ;;
        --client-only) BINARIES="bak" ;;
        --server-service) [ $# -ge 2 ] || die "--server-service needs the server's host name"; SERVICE_HOST="$2"; shift ;;
        --uninstall) UNINSTALL=1 ;;
        -h|--help) usage ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
    shift
done

[ "$(uname -s)" = "Linux" ] || die "this installer is for Linux; use installmac.sh on macOS"
if [ -n "$SERVICE_HOST" ] && [ "$BINARIES" = "bak" ]; then
    die "--server-service needs bak-server; drop --client-only"
fi

BIN_DIR="$PREFIX/bin"

SUDO=""
need_root() {
    if [ "$(id -u)" -ne 0 ]; then
        command -v sudo >/dev/null 2>&1 || die "root rights needed and sudo is missing; run as root or use --user"
        SUDO="sudo"
    fi
}
need_write() {
    dir="$BIN_DIR"
    while [ ! -d "$dir" ]; do dir=$(dirname "$dir"); done
    if [ ! -w "$dir" ] || { [ -d "$BIN_DIR" ] && [ ! -w "$BIN_DIR" ]; }; then
        need_root
        say "Installing to $BIN_DIR needs root; you may be asked for your password."
    fi
}

if [ "$UNINSTALL" -eq 1 ]; then
    need_write
    if command -v systemctl >/dev/null 2>&1 && [ -f /etc/systemd/system/bak-server.service ]; then
        need_root
        $SUDO systemctl disable --now bak-server || true
        $SUDO rm -f /etc/systemd/system/bak-server.service
        $SUDO systemctl daemon-reload
        say "removed the bak-server service (data in $DATA_DIR was kept)"
    fi
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
    command -v cargo >/dev/null 2>&1 || die "cargo not found. Install Rust from https://rustup.rs (and a C compiler: sudo apt install build-essential cmake), then run this again"
    say "Building release binaries (this takes a few minutes the first time)..."
    (cd "$SCRIPT_DIR" && cargo build --release --locked -p bak-cli -p bak-server)
    SRC_DIR="$SCRIPT_DIR/target/release"
fi

for b in $BINARIES; do
    [ -x "$SRC_DIR/$b" ] || die "$SRC_DIR/$b is missing"
    if command -v file >/dev/null 2>&1; then
        file "$SRC_DIR/$b" | grep -q "ELF" || die "$SRC_DIR/$b is not a Linux binary (wrong archive?)"
    fi
done

need_write
$SUDO mkdir -p "$BIN_DIR"
for b in $BINARIES; do
    $SUDO install -m 0755 "$SRC_DIR/$b" "$BIN_DIR/$b"
    say "installed $BIN_DIR/$b"
done

case ":$PATH:" in
    *":$BIN_DIR:"*) ON_PATH=1 ;;
    *) ON_PATH=0 ;;
esac
if [ "$ON_PATH" -eq 0 ]; then
    RC="$HOME/.profile"
    case "${SHELL:-}" in
        */bash) RC="$HOME/.bashrc" ;;
        */zsh) RC="$HOME/.zshrc" ;;
    esac
    LINE="export PATH=\"$BIN_DIR:\$PATH\""
    if ! grep -qsF "$LINE" "$RC"; then
        printf '\n# Added by the bak installer\n%s\n' "$LINE" >> "$RC"
        say "added $BIN_DIR to PATH in $RC"
    fi
    say "Open a new terminal (or run: $LINE) to use bak."
    export PATH="$BIN_DIR:$PATH"
fi

if [ -n "$SERVICE_HOST" ]; then
    command -v systemctl >/dev/null 2>&1 || die "systemd not found; run bak-server yourself (see README)"
    need_root
    if ! id "$SERVICE_USER" >/dev/null 2>&1; then
        $SUDO useradd --system --home "$DATA_DIR" --shell /usr/sbin/nologin "$SERVICE_USER"
        say "created system user $SERVICE_USER"
    fi
    $SUDO mkdir -p "$DATA_DIR"
    $SUDO chown "$SERVICE_USER": "$DATA_DIR"
    $SUDO chmod 0750 "$DATA_DIR"
    $SUDO tee /etc/systemd/system/bak-server.service >/dev/null <<EOF
[Unit]
Description=bak backup server
After=network-online.target
Wants=network-online.target

[Service]
User=$SERVICE_USER
ExecStart=$BIN_DIR/bak-server serve --path $DATA_DIR --listen 0.0.0.0:8000 --tls --append-only --private-repos --hostname $SERVICE_HOST
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=$DATA_DIR
PrivateTmp=true

[Install]
WantedBy=multi-user.target
EOF
    $SUDO systemctl daemon-reload
    $SUDO systemctl enable --now bak-server
    sleep 2
    say ""
    say "bak-server is running: https://$SERVICE_HOST:8000/_/"
    if $SUDO journalctl -u bak-server --no-pager 2>/dev/null | grep -q 'First start'; then
        say "Administrator account (shown once; keep it safe):"
        $SUDO journalctl -u bak-server --no-pager | grep -A2 'First start' | grep -E 'user:|password:' | sed 's/^.*\]: */  /'
    fi
    say "Clients need the CA certificate: $DATA_DIR/.tls/ca.pem (also downloadable from the dashboard)."
    say "Open TCP port 8000 in your firewall, e.g.: sudo ufw allow 8000/tcp"
fi

say ""
for b in $BINARIES; do "$BIN_DIR/$b" --version; done
say ""
say "Done. Start with:  cd your-project && bak init"
