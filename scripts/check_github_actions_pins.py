#!/usr/bin/env python3
"""Check that GitHub Actions are pinned by commit SHA unless explicitly allowed."""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_DIR = ROOT / ".github" / "workflows"
USES_RE = re.compile(r"^\s*-\s+uses:\s+([^#\s]+)|^\s*uses:\s+([^#\s]+)")
PINNED_RE = re.compile(r"^[^@\s]+@[0-9a-f]{40}$")
MAX_WORKFLOW_BYTES = 1024 * 1024

# Toolchain channels are intentional: rust-toolchain.toml owns the compiler
# channel, while this action only installs that requested toolchain/components.
ALLOWED_TAG_REFS = {
    "dtolnay/rust-toolchain@stable",
}


def iter_workflows() -> list[Path]:
    return sorted(
        path
        for suffix in ("*.yml", "*.yaml")
        for path in WORKFLOW_DIR.glob(suffix)
        if path.is_file()
    )


def read_text_limited(path: Path, max_bytes: int = MAX_WORKFLOW_BYTES) -> str:
    if not path.is_file():
        raise ValueError(f"{path}: must be a regular file")
    size = path.stat().st_size
    if size > max_bytes:
        raise ValueError(f"{path}: exceeds maximum size of {max_bytes} bytes")
    with path.open("rb") as handle:
        data = handle.read(max_bytes + 1)
    if len(data) > max_bytes:
        raise ValueError(f"{path}: exceeds maximum size of {max_bytes} bytes")
    return data.decode("utf-8")


def main() -> int:
    errors: list[str] = []
    try:
        for workflow in iter_workflows():
            for line_number, line in enumerate(read_text_limited(workflow).splitlines(), start=1):
                match = USES_RE.match(line)
                if not match:
                    continue
                ref = match.group(1) or match.group(2)
                if ref.startswith("./") or ref in ALLOWED_TAG_REFS or PINNED_RE.fullmatch(ref):
                    continue
                errors.append(f"{workflow.relative_to(ROOT)}:{line_number}: action is not pinned by commit SHA: {ref}")
    except (OSError, ValueError, UnicodeDecodeError) as exc:
        print(f"GitHub Actions pin check error: {exc}", file=sys.stderr)
        return 1

    if errors:
        print("GitHub Actions pin check failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    print("GitHub Actions pin check ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
