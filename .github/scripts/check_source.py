#!/usr/bin/env python3
"""Check source syntax and repository text without building or running xtcc."""

import ast
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
TEXT_SUFFIXES = {".rs", ".py", ".m", ".h", ".md", ".toml", ".yml", ".yaml"}
TEXT_NAMES = {"Cargo.lock", "Makefile", ".gitignore"}
CJK = re.compile(
    "[\u3000-\u303f\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff"
    "\uff01-\uff60\U00020000-\U000323af]"
)


def main():
    paths = subprocess.check_output(
        ["git", "ls-files", "-z"], cwd=ROOT
    ).decode("utf-8").split("\0")
    errors = []
    checked = 0
    for name in filter(None, paths):
        path = ROOT / name
        if path.suffix not in TEXT_SUFFIXES and path.name not in TEXT_NAMES:
            continue
        if path.is_symlink():
            errors.append(f"{name}: source checks do not follow symbolic links")
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            errors.append(f"{name}: {error}")
            continue
        checked += 1
        if text and not text.endswith("\n"):
            errors.append(f"{name}: missing final newline")
        if name != "README_CN.md":
            for number, line in enumerate(text.splitlines(), 1):
                if CJK.search(line):
                    errors.append(f"{name}:{number}: use English outside README_CN.md")
        if path.suffix == ".py":
            try:
                ast.parse(text, filename=name)
            except SyntaxError as error:
                errors.append(f"{name}:{error.lineno}: {error.msg}")

    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Source checks passed for {checked} tracked text files.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
