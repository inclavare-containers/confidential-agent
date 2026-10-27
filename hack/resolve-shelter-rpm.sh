#!/usr/bin/env bash
# Select the recorded package even when older RPMs remain in the checkout.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3.11 - "$root/hack/shelter-rpm.json" <<'PY'
import json
from pathlib import Path
import sys

manifest = Path(sys.argv[1])
try:
    name = json.loads(manifest.read_text())["file"]
    if not isinstance(name, str) or Path(name).name != name or not name.startswith("shelter-") or not name.endswith(".rpm"):
        raise ValueError("invalid package filename in shelter-rpm.json")
    package = manifest.parent / name
    if not package.is_file():
        raise ValueError(f"recorded Shelter RPM does not exist: {package}")
except (OSError, ValueError, KeyError, TypeError) as error:
    sys.exit(f"Cannot resolve bundled Shelter RPM: {error}")
print(package)
PY
