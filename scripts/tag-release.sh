#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

version="$(scripts/workspace-version.sh)"
tag="v$version"

if [[ -n "$(git status --porcelain)" ]]; then
  echo "working tree is not clean; commit the versioned release state first" >&2
  exit 1
fi

if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  echo "tag already exists: $tag" >&2
  exit 1
fi

git tag -a "$tag" -m "kiliax $tag"
echo "created $tag"
echo "push it with: git push origin $tag"
