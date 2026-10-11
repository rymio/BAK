#!/bin/sh
# Install bak and bak-server on Windows, through WSL.
#
# bak does not run natively on Windows yet (it relies on Unix file
# permissions and mount detection). It runs fully inside WSL, the Linux
# environment built into Windows 10 and 11, and can back up your Windows
# folders from there (C:\ is /mnt/c).
#
# Run this inside a WSL terminal (Ubuntu). Options are passed on to
# installlinux.sh, e.g.  ./installwin.sh --user
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

say() { printf '%s\n' "$*"; }

in_wsl() {
    [ -n "${WSL_DISTRO_NAME:-}" ] || grep -qi microsoft /proc/version 2>/dev/null
}

case "$(uname -s)" in
    Linux)
        if ! in_wsl; then
            say "This looks like plain Linux, not WSL; using the Linux installer."
        fi
        [ -f "$SCRIPT_DIR/installlinux.sh" ] || { say "error: installlinux.sh must be next to this script" >&2; exit 1; }
        sh "$SCRIPT_DIR/installlinux.sh" "$@"
        if in_wsl; then
            say ""
            say "Windows tips:"
            say "  - Your Windows drives are under /mnt, e.g. C:\\Users\\you\\code is /mnt/c/Users/you/code"
            say "  - Projects inside WSL (~/...) are much faster to back up than ones under /mnt/c"
            say "  - A USB drive shows up as /mnt/<letter> after: sudo mount -t drvfs E: /mnt/e"
        fi
        ;;
    MINGW*|MSYS*|CYGWIN*)
        cat <<'EOF'
bak needs WSL on Windows. It does not run in Git Bash, MSYS2 or Cygwin.

1. Open PowerShell as Administrator and run:
       wsl --install
   then restart Windows when asked. This installs Ubuntu.
2. Open "Ubuntu" from the Start menu and create your Linux user.
3. Copy the bak release (or source) into WSL and run:
       ./installwin.sh
EOF
        exit 1
        ;;
    *)
        say "error: unsupported system $(uname -s); use installmac.sh or installlinux.sh" >&2
        exit 1
        ;;
esac
