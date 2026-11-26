#!/bin/sh
# rowser installer: downloads the rowser binary from a GitHub Release, checks
# it against SHA256SUMS and installs it to ~/.local/bin (or $ROWSER_INSTALL_DIR).
#
#   curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jhnhnsn/rowser/releases/latest/download/rowser-installer.sh | sh
#
# The release workflow writes the Release's version into ROWSER_VERSION below.
# No sudo, and nothing outside the home directory.

set -eu

ROWSER_VERSION="__ROWSER_VERSION__"
REPO_URL="https://github.com/jhnhnsn/rowser"

err() {
    echo "rowser-installer: $*" >&2
    exit 1
}

case "$ROWSER_VERSION" in
__*) default_base="$REPO_URL/releases/latest/download" ;;
*) default_base="$REPO_URL/releases/download/v$ROWSER_VERSION" ;;
esac
# Hidden test hook: a local HTTP server that serves the Release assets.
base="${ROWSER_INSTALL_BASE_URL:-$default_base}"

system="$(uname -sm)"
case "$system" in
"Linux x86_64") target=x86_64-unknown-linux-musl ;;
"Linux aarch64" | "Linux arm64") target=aarch64-unknown-linux-musl ;;
"Darwin arm64") target=aarch64-apple-darwin ;;
"Darwin x86_64") target=x86_64-apple-darwin ;;
*) err "unsupported system: $system" ;;
esac

[ -n "${HOME:-}" ] || err "HOME is not set"
install_dir="${ROWSER_INSTALL_DIR:-$HOME/.local/bin}"

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q -O "$2" "$1"; }
else
    err "needs curl or wget"
fi

if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
else
    err "needs sha256sum or shasum"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

asset="rowser-$target.tar.gz"
fetch "$base/$asset" "$tmp/$asset" || err "cannot download $base/$asset"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || err "cannot download $base/SHA256SUMS"

want="$(awk -v name="$asset" '$2 == name || $2 == "*" name {print $1; exit}' "$tmp/SHA256SUMS")"
[ -n "$want" ] || err "SHA256SUMS has no entry for $asset"
got="$(sha256 "$tmp/$asset")"
[ "$want" = "$got" ] || err "$asset does not match SHA256SUMS"

mkdir "$tmp/x"
tar -xzf "$tmp/$asset" -C "$tmp/x" rowser || err "cannot unpack $asset"

mkdir -p "$install_dir"
# Replace an existing binary in one step: temp file in the same folder, then mv.
part="$install_dir/.rowser.part.$$"
cp "$tmp/x/rowser" "$part"
chmod 755 "$part"
mv -f "$part" "$install_dir/rowser"

version="$("$install_dir/rowser" --version 2>/dev/null || echo rowser)"
echo "Installed $version to $install_dir/rowser"

case ":${PATH:-}:" in
*":$install_dir:"*) ;;
*)
    echo
    echo "$install_dir is not on your PATH. Add this line to your shell profile:"
    echo "  export PATH=\"$install_dir:\$PATH\""
    ;;
esac

echo
echo "Next steps:"
echo "  On the Local machine: rowser daemon install"
echo "  For a Remote host:    rowser remote install <host>"
