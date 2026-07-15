#!/bin/sh
#
# orchestratr installer — downloads the prebuilt `orcr` binary for your platform
# from GitHub Releases and installs it.
#
#   curl -fsSL https://orchestratr.dev/install.sh | sh
#   curl -fsSL https://orchestratr.dev/install.sh | sh -s -- v0.1.0   # pin a version
#
# Env overrides:
#   ORCR_INSTALL_DIR   install location (default: $HOME/.local/bin)
#   GITHUB_TOKEN       for downloading from a private repo (until it's public)
#
set -eu

REPO="hkandala/orchestratr"
BIN="orcr"
INSTALL_DIR="${ORCR_INSTALL_DIR:-$HOME/.local/bin}"

say()  { printf '%s\n' "$*"; }
err()  { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

have curl || err "curl is required"
have tar  || err "tar is required"

# --- detect platform → release asset suffix ---
os="$(uname -s)"; arch="$(uname -m)"
case "$os" in
  Darwin) case "$arch" in
            arm64|aarch64) plat="macos-arm64" ;;
            x86_64)        plat="macos-x64" ;;
            *) err "unsupported macOS arch: $arch" ;;
          esac ;;
  Linux)  case "$arch" in
            x86_64|amd64)  plat="linux-x64" ;;
            *) err "unsupported Linux arch: $arch (only linux-x64 is prebuilt today)" ;;
          esac ;;
  *) err "unsupported OS: $os (macOS and Linux only; Windows is on the roadmap)" ;;
esac

auth=""
[ -n "${GITHUB_TOKEN:-}" ] && auth="-H Authorization:\ Bearer\ ${GITHUB_TOKEN}"

# --- resolve version (arg, else latest via the releases/latest redirect — no API/jq) ---
tag="${1:-}"
if [ -z "$tag" ]; then
  eff="$(curl -fsSLI -o /dev/null -w '%{url_effective}' $auth "https://github.com/$REPO/releases/latest")" \
    || err "could not reach GitHub releases"
  tag="${eff##*/tag/}"
  [ -n "$tag" ] && [ "$tag" != "$eff" ] || err "could not resolve the latest release (is the repo public / GITHUB_TOKEN set?)"
fi
ver="${tag#v}"
asset="${BIN}-${ver}-${plat}.tar.gz"
base="https://github.com/$REPO/releases/download/$tag"

say "orchestratr: installing $BIN $tag ($plat) → $INSTALL_DIR"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL $auth -o "$tmp/$asset" "$base/$asset" \
  || err "download failed: $base/$asset"

# --- verify checksum if present ---
if curl -fsSL $auth -o "$tmp/$asset.sha256" "$base/$asset.sha256" 2>/dev/null; then
  expected="$(awk '{print $1}' "$tmp/$asset.sha256")"
  if have sha256sum; then actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
  else actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"; fi
  [ "$expected" = "$actual" ] || err "checksum mismatch (expected $expected, got $actual)"
  say "checksum ok"
fi

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 0755 "$tmp/$BIN" "$INSTALL_DIR/$BIN" 2>/dev/null || { cp "$tmp/$BIN" "$INSTALL_DIR/$BIN"; chmod 0755 "$INSTALL_DIR/$BIN"; }

say "installed: $INSTALL_DIR/$BIN"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) : ;;
  *) say ""; say "note: $INSTALL_DIR is not on your PATH — add it, e.g.:"; say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc" ;;
esac
say "run: $BIN --help"
