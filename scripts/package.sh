#!/bin/sh
# Build a release archive for this machine's platform:
#   dist/bak-<version>-<os>-<arch>.tar.gz
# containing bak, bak-server, the installers, README and LICENSE.
# Build on each platform you release for (macOS arm64/x86_64, Linux x86_64/arm64).
set -eu
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
case "$OS" in darwin) OS=macos ;; esac
ARCH=$(uname -m)
case "$ARCH" in arm64|aarch64) ARCH=arm64 ;; x86_64|amd64) ARCH=x86_64 ;; esac
NAME="bak-$VERSION-$OS-$ARCH"

cargo build --release --locked -p bak-cli -p bak-server

rm -rf "dist/$NAME"
mkdir -p "dist/$NAME"
cp target/release/bak target/release/bak-server "dist/$NAME/"
cp README.md LICENSE installmac.sh installlinux.sh installwin.sh "dist/$NAME/"
chmod 0755 "dist/$NAME"/*.sh "dist/$NAME/bak" "dist/$NAME/bak-server"
tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
rm -rf "dist/$NAME"
(cd dist && shasum -a 256 "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
echo "dist/$NAME.tar.gz"
cat "dist/$NAME.tar.gz.sha256"
