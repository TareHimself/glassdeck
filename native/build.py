"""Builds every native module with silo and copies each one's bin folder into target/native, the one
folder glassdeck looks in (GLASSDECK_NATIVE, else beside glassdeck.exe).

    uv run python build.py
"""

import json
import shutil
import subprocess
import sys
from pathlib import Path

NATIVE_DIR = Path(__file__).resolve().parent
OUT_DIR = NATIVE_DIR.parent / "target" / "native"


def modules() -> list[Path]:
    return sorted(recipe.parent for recipe in NATIVE_DIR.glob("*/silopkg.py"))


def build(module: Path) -> Path:
    report_dir = NATIVE_DIR / "build-report" / module.name
    result = subprocess.run(["silo", "build", str(module), "--report", str(report_dir)], cwd=NATIVE_DIR, check=False)
    report = json.loads((report_dir / "report.json").read_text())
    if result.returncode != 0 or not report["ok"]:
        sys.exit(f"{module.name} failed to build; see {report_dir}")
    return Path(report["store_path"]) / "bin"


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for module in modules():
        for library in build(module).iterdir():
            shutil.copy2(library, OUT_DIR / library.name)
        print(f"{module.name}: copied into {OUT_DIR}")


if __name__ == "__main__":
    main()
