#!/usr/bin/env bash
#
# release.sh — one-command release for orchestratr.
#
# Bumps the version in Cargo.toml + sdk/ts/package.json (and Cargo.lock), commits,
# tags v<version>, and pushes — which triggers .github/workflows/release.yml (build
# platform binaries → GitHub Release, + gated crates.io/npm publish).
#
# Usage:
#   scripts/release.sh patch        # 0.1.0 -> 0.1.1  (bug fixes)
#   scripts/release.sh minor        # 0.1.0 -> 0.2.0  (backward-compatible features)
#   scripts/release.sh major        # 0.1.0 -> 1.0.0  (breaking changes)
#   scripts/release.sh 0.4.2        # set an explicit version
#   scripts/release.sh minor --dry-run   # show what it would do, change nothing
#
set -euo pipefail

die() { echo "error: $*" >&2; exit 1; }
usage() { echo "usage: $0 <patch|minor|major|X.Y.Z> [--dry-run]" >&2; exit 1; }

[ $# -ge 1 ] || usage
bump="$1"; shift || true
dry_run=false
[ "${1:-}" = "--dry-run" ] && dry_run=true

cd "$(git rev-parse --show-toplevel)"

# --- preconditions (safe by default) ---
[ "$(git rev-parse --abbrev-ref HEAD)" = "main" ] || die "not on main (checkout main first)"
git diff --quiet && git diff --cached --quiet || die "working tree not clean (commit or stash first)"
git fetch -q origin
[ "$(git rev-parse @)" = "$(git rev-parse '@{u}')" ] || die "main is not in sync with origin/main (pull/push first)"

# --- current version (source of truth: Cargo.toml) ---
cur="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
[[ "$cur" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "could not parse current version from Cargo.toml (got '$cur')"
IFS=. read -r MA MI PA <<< "$cur"

case "$bump" in
  major) new="$((MA+1)).0.0" ;;
  minor) new="${MA}.$((MI+1)).0" ;;
  patch) new="${MA}.${MI}.$((PA+1))" ;;
  [0-9]*.[0-9]*.[0-9]*) [[ "$bump" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "invalid version '$bump'"; new="$bump" ;;
  *) usage ;;
esac

echo "orchestratr release: $cur -> $new"
git rev-parse "v$new" >/dev/null 2>&1 && die "tag v$new already exists"

if $dry_run; then
  echo "[dry-run] would: bump Cargo.toml + sdk/ts/package.json to $new, cargo build, commit 'chore(release): v$new', tag v$new, push main + tag"
  exit 0
fi

# --- bump manifests ---
sed -i.bak -E "s/^version = \"${cur}\"/version = \"${new}\"/" Cargo.toml && rm -f Cargo.toml.bak
node -e "const f='sdk/ts/package.json',p=require('./'+f);p.version='${new}';require('fs').writeFileSync(f,JSON.stringify(p,null,2)+'\n')"

# --- sync Cargo.lock + sanity build ---
cargo build -q

git add Cargo.toml Cargo.lock sdk/ts/package.json
git commit -q -m "chore(release): v${new}"
git tag -a "v${new}" -m "orchestratr v${new}"
git push -q origin main
git push -q origin "v${new}"

echo "✅ pushed v${new}"
echo "   release will appear at: https://github.com/hkandala/orchestratr/releases/tag/v${new}"
echo "   watch the build:        gh run watch \$(gh run list --workflow=release.yml -L1 --json databaseId --jq '.[0].databaseId')"
