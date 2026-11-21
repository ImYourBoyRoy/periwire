#!/bin/sh
# PeriWire bootstrap for Linux and macOS: download the release archive, check its SHA-256, unpack it, and let the program set itself up.
#   curl -fsSL https://github.com/ImYourBoyRoy/periwire/releases/latest/download/install.sh | sh
# Options (environment): PERIWIRE_VERSION=0.1.2 (default: the latest release), PERIWIRE_INSTALL_DIR (default ~/.local/share/periwire/app),
# PERIWIRE_NO_SHELL=1 (do not touch shell profiles), PERIWIRE_RELEASE_BASE_URL (a folder with the archive and its .sha256: https, file://, or http on loopback).
# The real work (PATH, shell profiles) is `periwire self-install`; undo it with `periwire self-uninstall`, then delete the install folder.
set -eu

REPO="ImYourBoyRoy/periwire"
say() { printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  *) die "this script is for Linux and macOS; on Windows use install.ps1" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=arm64 ;;
  *) die "no PeriWire download for $(uname -m)" ;;
esac
[ "$os-$arch" = "linux-arm64" ] && die "no PeriWire download for Linux on arm64 yet"

fetch() { # fetch URL DEST
  if have curl; then curl -fsSL "$1" -o "$2"; elif have wget; then wget -q "$1" -O "$2"; else die "needs curl or wget"; fi
}

version="${PERIWIRE_VERSION:-}"
if [ -z "$version" ]; then
  have curl || die "needs curl to find the latest release (or set PERIWIRE_VERSION)"
  url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") || die "could not reach GitHub"
  version="${url##*/v}"
fi
version="${version#v}"
case "$version" in *[!0-9A-Za-z.+-]*) die "odd version: $version" ;; esac

base="${PERIWIRE_RELEASE_BASE_URL:-https://github.com/$REPO/releases/download/v$version}"
base="${base%/}"
case "$base" in
  https://* | file://*) ;;
  http://127.0.0.1* | http://localhost* | http://\[::1\]*) ;;
  *) die "PERIWIRE_RELEASE_BASE_URL must be https://, file://, or http:// on this computer" ;;
esac

asset="periwire-$version-$os-$arch.tar.gz"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/periwire-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $asset"
fetch "$base/$asset" "$tmp/$asset" || die "could not download $base/$asset"
if fetch "$base/$asset.sha256" "$tmp/sum" 2>/dev/null; then :; else fetch "$base/SHA256SUMS.txt" "$tmp/sum" || die "the release has no checksum for $asset"; fi
want=$(awk -v f="$asset" 'NF==1 && length($1)==64 {print $1; exit} $2==f || $2=="*"f {print $1; exit}' "$tmp/sum")
[ -n "$want" ] || die "no checksum for $asset in the release"
if have sha256sum; then got=$(sha256sum "$tmp/$asset" | awk '{print $1}'); else got=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}'); fi
[ "$got" = "$want" ] || die "the download does not match its checksum (expected $want, got $got)"
say "checksum ok"

dest="${PERIWIRE_INSTALL_DIR:-$HOME/.local/share/periwire/app}"
mkdir -p "$tmp/x" "$dest"
tar -xzf "$tmp/$asset" -C "$tmp/x" --strip-components=1 || die "could not unpack the archive"
[ -f "$tmp/x/periwire" ] || die "the archive has no periwire program"
for f in periwire periwire-gui; do
  [ -f "$tmp/x/$f" ] || continue
  cp "$tmp/x/$f" "$dest/$f.new" && chmod 755 "$dest/$f.new" && mv "$dest/$f.new" "$dest/$f"
done
say "installed in $dest"

args="--yes"
[ "${PERIWIRE_NO_SHELL:-}" = "1" ] && args="$args --no-shell"
"$dest/periwire" self-install $args
say "Done. Open a new terminal, then run: periwire doctor"
