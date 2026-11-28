#!/usr/bin/env python3
"""Merge a `--targets` re-run into results.json.

    python3 bench/merge.py results/results-targets.json

Every figure the re-run measured replaces the one in results.json (entry
by entry: per system, per page, per build); every other figure stays as
its own run left it. results.json records which targets came from which
re-run under `reruns`, so a table can always be traced to its file.
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).parent


def merge(base, new):
    for k, v in new.items():
        if isinstance(v, dict) and isinstance(base.get(k), dict):
            merge(base[k], v)
        else:
            base[k] = v


def main(path):
    src = HERE / path if not Path(path).is_absolute() else Path(path)
    rerun = json.loads(src.read_text())
    out = HERE / "results/results.json"
    r = json.loads(out.read_text())
    keep = {k: rerun.pop(k) for k in ("date", "hardware", "targets", "settings", "wall_s")
            if k in rerun}
    merge(r, rerun)
    r.setdefault("reruns", []).append({
        "date": keep.get("date"), "targets": keep.get("targets"),
        "wall_s": keep.get("wall_s"), "file": src.name,
    })
    out.write_text(json.dumps(r, indent=1))
    print(f"merged {src.name} ({keep.get('targets')}) into {out}")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "results/results-targets.json")
