#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

artifact="${1:-examples/tenancy/handoff}"
shift || true

exec cargo run -p greentic-operax --bin greentic-operax -- run "$artifact" "$@"
