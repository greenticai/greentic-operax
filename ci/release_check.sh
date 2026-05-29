#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

python3 - <<'PY'
import filecmp
import json
import pathlib
import subprocess
import sys

try:
    import tomllib
except ModuleNotFoundError:
    print("Python 3.11+ with tomllib is required for release metadata checks.", file=sys.stderr)
    raise

ROOT = pathlib.Path.cwd()
CLI_MANIFEST = ROOT / "crates" / "operax-cli" / "Cargo.toml"

def fail(message):
    print(f"release check failed: {message}", file=sys.stderr)
    sys.exit(1)

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"], text=True))
packages = {package["name"]: package for package in metadata["packages"]}
cli = packages.get("greentic-operax")
if cli is None:
    fail("missing greentic-operax package")

bin_targets = [target["name"] for target in cli.get("targets", []) if "bin" in target.get("kind", [])]
if bin_targets != ["greentic-operax"]:
    fail(f"expected exactly one binary target named greentic-operax, got {bin_targets!r}")

cli_manifest = tomllib.loads(CLI_MANIFEST.read_text())
binstall = cli_manifest["package"].get("metadata", {}).get("binstall", {})
expected_binstall = {
    "pkg-url": "{ repo }/releases/download/v{ version }/{ name }-v{ version }-{ target }.{ archive-format }",
    "bin-dir": "{ name }-v{ version }-{ target }",
    "pkg-fmt": "tgz",
    "bin": ["greentic-operax"],
}
for key, expected in expected_binstall.items():
    actual = binstall.get(key)
    if actual != expected:
        fail(f"package.metadata.binstall.{key} expected {expected!r}, got {actual!r}")

root_i18n = ROOT / "i18n"
cli_i18n = ROOT / "crates" / "operax-cli" / "i18n"
root_json = sorted(path.name for path in root_i18n.glob("*.json"))
cli_json = sorted(path.name for path in cli_i18n.glob("*.json"))
if root_json != cli_json:
    fail("crates/operax-cli/i18n must contain the same JSON files as root i18n/")
for name in root_json:
    if not filecmp.cmp(root_i18n / name, cli_i18n / name, shallow=False):
        fail(f"CLI i18n catalog is out of sync: {name}")

required_workflows = [
    "ci.yml",
    "customer-pilot-demo.yml",
    "nightly-coverage.yml",
    "perf.yml",
    "release.yml",
    "publish.yml",
    "release-binaries.yml",
]
workflow_dir = ROOT / ".github" / "workflows"
missing = [name for name in required_workflows if not (workflow_dir / name).exists()]
if missing:
    fail("missing workflow(s): " + ", ".join(missing))

release_binaries_text = (workflow_dir / "release-binaries.yml").read_text()
if "uses: greenticai/.github/.github/workflows/release-binaries.yml@main" not in release_binaries_text:
    fail("release-binaries workflow must use the shared Greentic binary release workflow")
if "package: greentic-operax" not in release_binaries_text:
    fail("release-binaries workflow must release package greentic-operax")
if "binary: greentic-operax" not in release_binaries_text:
    fail("release-binaries workflow must release binary greentic-operax")

if not (ROOT / "coverage-policy.json").exists():
    fail("missing coverage-policy.json")

print(f"binstall release asset: greentic-operax-v{cli['version']}-<target>.tgz")
print("release metadata ok")
PY
