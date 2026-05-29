#!/usr/bin/env bash
set -euo pipefail

step() {
  printf '\n==> %s\n' "$1"
}

publishable_crates() {
  cargo metadata --no-deps --format-version 1 | python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
members = set(metadata.get("workspace_members", []))
packages = metadata.get("packages", [])
selected = {
    p["name"]: p
    for p in packages
    if (not members or p["id"] in members)
    and p.get("publish") is not False
    and p.get("publish") != []
}
for name in selected:
    print(name)
'
}

step "cargo fmt"
cargo fmt --all -- --check

if [ -x tools/i18n.sh ]; then
  step "i18n"
  tools/i18n.sh validate
  tools/i18n.sh status
  bash tools/sync_cli_i18n.sh
fi

step "release metadata"
bash ci/release_check.sh

step "cargo clippy"
cargo clippy --all-targets --all-features -- -D warnings

step "cargo test"
cargo test --workspace --all-targets --all-features

step "cargo build"
cargo build --all-features

step "cargo doc"
cargo doc --no-deps --all-features

crates="$(publishable_crates)"
step "packaging"
if [ -z "$crates" ]; then
  printf 'No publishable crates detected.\n'
else
  printf '%s\n' "$crates" | while read -r crate; do
    cargo package --no-verify -p "$crate" --allow-dirty
    cargo package -p "$crate" --allow-dirty
    cargo publish -p "$crate" --dry-run --allow-dirty
  done
fi
