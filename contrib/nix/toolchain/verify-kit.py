#!/usr/bin/env python3
"""Verify that a compiler kit manifest covers exactly its contained files."""
import hashlib
from pathlib import Path
import sys


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def verify(root):
    root = Path(root).resolve()
    manifest = root / "SHA256SUMS"
    listed = set()
    for line in manifest.read_text().splitlines():
        expected, name = line.split("  ", 1)
        path = Path(name)
        if path.is_absolute() or ".." in path.parts or name in listed:
            raise ValueError("invalid kit manifest path")
        source = root / path
        if root not in source.resolve().parents or digest(source) != expected:
            raise ValueError(f"kit content mismatch: {name}")
        listed.add(name)
    actual = {"./" + str(p.relative_to(root)) for p in root.rglob("*")
              if (p.is_file() or p.is_symlink()) and p != manifest}
    if actual != listed:
        raise ValueError("kit manifest does not cover exactly the kit files")
    return digest(manifest)


if __name__ == "__main__":
    try:
        print(verify(sys.argv[1]))
    except (ValueError, OSError) as error:
        raise SystemExit(str(error)) from error
