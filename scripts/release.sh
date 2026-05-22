#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

workspace_version() {
  awk '
    $0 == "[workspace.package]" {
      in_workspace_package = 1
      next
    }
    /^\[/ {
      in_workspace_package = 0
    }
    in_workspace_package && $1 == "version" && $2 == "=" {
      version = $3
      gsub(/"/, "", version)
      print version
      found = 1
      exit
    }
    END {
      if (!found) {
        exit 1
      }
    }
  ' Cargo.toml
}

check_tag() {
  local tag version
  tag="$1"
  version="$(workspace_version)"

  if [[ "$tag" != "v$version" ]]; then
    echo "release tag $tag does not match Cargo workspace version $version" >&2
    exit 1
  fi

  echo "release tag $tag matches Cargo workspace version $version"
}

if [[ "${1:-}" == "--check-tag" ]]; then
  if [[ -z "${2:-}" ]]; then
    echo "usage: scripts/release.sh --check-tag <tag>" >&2
    exit 1
  fi
  check_tag "$2"
  exit 0
fi

version="${1:-}"
if [[ -z "$version" ]]; then
  echo "usage: scripts/release.sh <version>" >&2
  exit 1
fi

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "version must be a SemVer value such as 0.6.1: $version" >&2
  exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
  echo "working tree is not clean; release from a committed state" >&2
  exit 1
fi

current_version="$(workspace_version)"
if [[ "$version" == "$current_version" ]]; then
  echo "workspace version is already $version" >&2
  exit 1
fi

tag="v$version"
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  echo "tag already exists: $tag" >&2
  exit 1
fi

tmp_manifest="$(mktemp)"
trap 'rm -f "$tmp_manifest"' EXIT
cp -p Cargo.toml "$tmp_manifest"

awk -v version="$version" '
  $0 == "[workspace.package]" {
    in_workspace_package = 1
  }
  /^\[/ && $0 != "[workspace.package]" {
    in_workspace_package = 0
  }
  in_workspace_package && $1 == "version" && $2 == "=" {
    print "version = \"" version "\""
    updated = 1
    next
  }
  {
    print
  }
  END {
    if (!updated) {
      exit 1
    }
  }
' Cargo.toml > "$tmp_manifest"
mv "$tmp_manifest" Cargo.toml

cargo update --workspace
cargo test --workspace

actual_version="$(workspace_version)"
if [[ "$actual_version" != "$version" ]]; then
  echo "workspace version update failed: expected $version, got $actual_version" >&2
  exit 1
fi

check_tag "$tag"
git add Cargo.toml Cargo.lock
git commit -m "release: $tag"
git tag -a "$tag" -m "kiliax $tag"

echo "created release commit and tag $tag"
echo "push them with:"
echo "  git push origin HEAD"
echo "  git push origin $tag"
