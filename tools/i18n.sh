#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

MODE="${1:-all}"
AUTH_MODE="${AUTH_MODE:-auto}"
LOCALE="${LOCALE:-en}"
EN_PATH="${EN_PATH:-i18n/en.json}"
LOCALES_PATH="${LOCALES_PATH:-i18n/locales.json}"
BATCH_SIZE="${I18N_BATCH_SIZE:-200}"
TRANSLATOR_BIN="${TRANSLATOR_BIN:-greentic-i18n-translator}"

usage() {
  cat <<'EOF'
Usage: tools/i18n.sh [translate|validate|status|all]

Environment overrides:
  EN_PATH=...                     English source file path (default: i18n/en.json)
  LOCALES_PATH=...                Locale list file path (default: i18n/locales.json)
  AUTH_MODE=auto|api-key|browser  Translator auth mode for translate (default: auto)
  LOCALE=...                      CLI locale used for translator output (default: en)
  I18N_BATCH_SIZE=<int>           Keys per translation request (default: 200)
  TRANSLATOR_BIN=...              Translator binary name or path (default: greentic-i18n-translator)
EOF
}

log() {
  printf '[i18n] %s\n' "$*"
}

fail() {
  printf '[i18n] error: %s\n' "$*" >&2
  exit 1
}

find_translator_bin() {
  if command -v "$TRANSLATOR_BIN" >/dev/null 2>&1; then
    command -v "$TRANSLATOR_BIN"
    return
  fi
  if command -v greentic-i18n-translator >/dev/null 2>&1; then
    command -v greentic-i18n-translator
    return
  fi
  local candidates=()
  if [[ -n "${CARGO_INSTALL_ROOT:-}" ]]; then
    candidates+=("${CARGO_INSTALL_ROOT}/bin/greentic-i18n-translator")
  fi
  if [[ -n "${CARGO_HOME:-}" ]]; then
    candidates+=("${CARGO_HOME}/bin/greentic-i18n-translator")
  fi
  if [[ -n "${HOME:-}" ]]; then
    candidates+=("${HOME}/.cargo/bin/greentic-i18n-translator")
  fi
  if command -v cargo >/dev/null 2>&1; then
    candidates+=("$(dirname "$(command -v cargo)")/greentic-i18n-translator")
  fi
  local candidate
  for candidate in "${candidates[@]}"; do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return
    fi
  done
}

ensure_translator() {
  local resolved
  resolved="$(find_translator_bin || true)"
  if [[ -n "$resolved" ]]; then
    TRANSLATOR_BIN="$resolved"
    return
  fi
  command -v cargo-binstall >/dev/null 2>&1 || return 1
  log "installing greentic-i18n-translator via cargo-binstall"
  cargo binstall -y greentic-i18n-translator || return 1
  resolved="$(find_translator_bin || true)"
  if [[ -n "$resolved" ]]; then
    TRANSLATOR_BIN="$resolved"
  else
    log "greentic-i18n-translator is still unavailable after cargo-binstall; using local checks when possible"
    return 1
  fi
}

require_translator() {
  ensure_translator || fail "${TRANSLATOR_BIN} not found and cargo-binstall could not make it available"
}

require_locale_list() {
  [[ -f "$LOCALES_PATH" ]] || fail "missing locale list: ${LOCALES_PATH}"
}

locale_csv() {
  python3 - "$LOCALES_PATH" <<'PY'
import json
import sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    print(",".join(json.load(f)))
PY
}

translator_supports_batch_size() {
  "$TRANSLATOR_BIN" --help 2>/dev/null | grep -q -- "--batch-size"
}

catalogs_have_missing() {
  python3 - "$LOCALES_PATH" "$EN_PATH" <<'PY'
import json
import sys
from pathlib import Path

locales_path, en_path = Path(sys.argv[1]), Path(sys.argv[2])
locales = json.loads(locales_path.read_text(encoding="utf-8"))
base_dir = en_path.parent
for locale in locales:
    path = en_path if locale == "en" else base_dir / f"{locale}.json"
    if not path.exists():
        raise SystemExit(0)
raise SystemExit(1)
PY
}

run_translate() {
  require_translator
  require_locale_list
  local langs
  langs="$(locale_csv)"
  if translator_supports_batch_size; then
    "$TRANSLATOR_BIN" --locale "$LOCALE" translate --langs "$langs" --en "$EN_PATH" --auth-mode "$AUTH_MODE" --batch-size "$BATCH_SIZE"
  else
    "$TRANSLATOR_BIN" --locale "$LOCALE" translate --langs "$langs" --en "$EN_PATH" --auth-mode "$AUTH_MODE"
  fi
  bash tools/sync_cli_i18n.sh
}

run_local_catalog_check() {
  local label="$1"
  python3 - "$label" "$LOCALES_PATH" "$EN_PATH" <<'PY'
import json
import sys
from pathlib import Path

label, locales_path, en_path = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])
locales = json.loads(locales_path.read_text(encoding="utf-8"))
if not isinstance(locales, list) or not all(isinstance(locale, str) for locale in locales):
    raise SystemExit(f"{locales_path} must contain a JSON list of locale strings")
en_catalog = json.loads(en_path.read_text(encoding="utf-8"))
if not isinstance(en_catalog, dict):
    raise SystemExit(f"{en_path} must contain a JSON object")
expected_keys = set(en_catalog)
base_dir = en_path.parent
print(f"{label}: {len(locales)} language(s)")
for locale in locales:
    path = en_path if locale == "en" else base_dir / f"{locale}.json"
    if not path.exists():
        print(f"{locale}: pending translation ({path} is missing)")
        continue
    catalog = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(catalog, dict):
        raise SystemExit(f"{locale}: catalog must contain a JSON object")
    if locale != "en" and catalog == en_catalog:
        raise SystemExit(f"{locale}: catalog is identical to English; delete it and rerun translation")
    missing = sorted(expected_keys - set(catalog))
    extra = sorted(set(catalog) - expected_keys)
    if missing or extra:
        details = []
        if missing:
            details.append("missing keys: " + ", ".join(missing))
        if extra:
            details.append("extra keys: " + ", ".join(extra))
        raise SystemExit(f"{locale}: {'; '.join(details)}")
    print(f"{locale}: ok")
PY
}

run_validate() {
  require_locale_list
  if catalogs_have_missing; then
    run_local_catalog_check validate
  elif ensure_translator; then
    "$TRANSLATOR_BIN" --locale "$LOCALE" validate --langs "$(locale_csv)" --en "$EN_PATH"
  else
    run_local_catalog_check validate
  fi
}

run_status() {
  require_locale_list
  if catalogs_have_missing; then
    run_local_catalog_check status
  elif ensure_translator; then
    "$TRANSLATOR_BIN" --locale "$LOCALE" status --langs "$(locale_csv)" --en "$EN_PATH"
  else
    run_local_catalog_check status
  fi
}

if [[ "${MODE}" == "-h" || "${MODE}" == "--help" ]]; then
  usage
  exit 0
fi

case "$MODE" in
  translate) run_translate ;;
  validate) run_validate ;;
  status) run_status ;;
  all)
    run_translate
    run_validate
    run_status
    bash tools/sync_cli_i18n.sh
    ;;
  *)
    echo "Unknown mode: $MODE" >&2
    usage
    exit 2
    ;;
esac
