#!/usr/bin/env python3
"""Record an explicitly supplied, pinned file-association snapshot for future review.

This tool intentionally has no network access and does not generate Rust.  A future source-data
refresh must first supply reviewed upstream bytes, then extend this seam with a parser and tests.
Keeping the provenance boundary explicit avoids falsely representing a partial local map as a
vendored upstream dataset.
"""
from __future__ import annotations

import argparse
import hashlib
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--snapshot-dir", type=Path, required=True)
    parser.add_argument("--commit", required=True, help="reviewed full 40-character source commit SHA")
    args = parser.parse_args()
    if len(args.commit) != 40 or any(c not in "0123456789abcdef" for c in args.commit):
        parser.error("--commit must be a lowercase, full 40-character SHA")
    required = ("icons_by_filename.lua", "icons_by_file_extension.lua", "LICENSE")
    missing = [name for name in required if not (args.snapshot_dir / name).is_file()]
    if missing:
        parser.error("snapshot is missing required files: " + ", ".join(missing))
    print(f"candidate source commit: {args.commit}")
    for name in required:
        data = (args.snapshot_dir / name).read_bytes()
        print(f"{name}: sha256:{hashlib.sha256(data).hexdigest()}")
    print("No files changed. Review bytes and add a tested parser before vendoring this snapshot.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
