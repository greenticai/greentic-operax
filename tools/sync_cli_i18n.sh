#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="${ROOT_DIR}/i18n"
DST_DIR="${ROOT_DIR}/crates/operax-cli/i18n"

mkdir -p "$DST_DIR"
find "$DST_DIR" -maxdepth 1 -type f -name '*.json' -delete
cp "$SRC_DIR"/*.json "$DST_DIR"/
