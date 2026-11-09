#!/usr/bin/env bash
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "Usage: $0 <new-version>"
  echo "Example: $0 0.2.1"
  exit 1
fi

NEW="$1"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# In-place edit that works with both GNU sed (Linux) and BSD sed (macOS).
sedi() {
  local expr="$1" file="$2"
  sed -i.bak "$expr" "$file" && rm -f "$file.bak"
}

# Update workspace version in Cargo.toml
sedi "s/^version = \".*\"/version = \"${NEW}\"/" "$ROOT/Cargo.toml"

# Update the ticgit-lib version requirement in the CLI crate
sedi "s/^\(ticgit-lib = { version = \"\)[^\"]*\"/\1${NEW}\"/" "$ROOT/crates/ticgit/Cargo.toml"

# Update version strings in docs/index.html
sedi "s/ticgit [0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*/ticgit ${NEW}/g" "$ROOT/docs/index.html"
sedi "s/>[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*</>$NEW</g" "$ROOT/docs/index.html"

echo "Bumped version to ${NEW}"
echo "  Cargo.toml"
echo "  crates/ticgit/Cargo.toml"
echo "  docs/index.html"
echo ""
echo "Run 'cargo check' to verify, then commit."
