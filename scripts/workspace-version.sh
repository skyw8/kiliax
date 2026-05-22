#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

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
' "$repo_root/Cargo.toml"
