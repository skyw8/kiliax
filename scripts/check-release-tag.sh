#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$("$repo_root/scripts/workspace-version.sh")"
tag="${1:-${GITHUB_REF_NAME:-}}"

if [[ -z "$tag" ]]; then
  echo "release tag is required" >&2
  exit 1
fi

if [[ "$tag" != v* ]]; then
  echo "release tag must start with v: $tag" >&2
  exit 1
fi

if [[ "${tag#v}" != "$version" ]]; then
  echo "release tag $tag does not match Cargo workspace version $version" >&2
  exit 1
fi

echo "release tag $tag matches Cargo workspace version $version"
