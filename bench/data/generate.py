#!/usr/bin/env python3
"""Deterministic benchmark content: N articles with a title, a summary, a
Markdown body of about 600 words, two or three topics and a date. Every
system imports the same file, so they serve the same words.

    python3 data/generate.py 200 > data/articles-200.json
"""
import json
import random
import sys

WORDS = (
    "iron forge anvil hammer tongs quench temper scale slag bloom bar rod "
    "railing gate hinge rivet weld flux coal coke charcoal bellows hearth "
    "smith workshop pattern scroll twist taper upset draw punch drift chisel "
    "file rasp vise stake swage fuller flatter set hardy pritchel bick horn "
    "face edge heel tang ferrule collar leaf finial basket spiral texture "
    "patina rust wax oil linseed beeswax brush wire wheel grinder belt sander "
    "oak ash elm walnut beech handle shaft wedge joint mortise tenon dovetail "
    "plane chisel saw kerf grain board glue clamp square marking gauge"
).split()
TOPICS = ["Forging", "Tools", "Joinery", "Finishes", "Materials", "History",
          "Repairs", "Design", "Safety", "Workshop"]


def sentence(rng, n):
    w = [rng.choice(WORDS) for _ in range(n)]
    s = " ".join(w)
    return s[0].upper() + s[1:] + "."


def article(rng, i):
    title = " ".join(rng.choice(WORDS) for _ in range(rng.randint(3, 6))).capitalize()
    title = f"{title} {i}"
    paras = []
    for p in range(6):
        if p == 2:
            paras.append(f"## {sentence(rng, 4)[:-1]}")
        paras.append(" ".join(sentence(rng, rng.randint(8, 16)) for _ in range(rng.randint(4, 7))))
    return {
        "slug": f"article-{i}",
        "title": title,
        "summary": sentence(rng, 18),
        "body": "\n\n".join(paras),
        "topics": sorted(rng.sample(TOPICS, rng.randint(2, 3))),
        "date": f"2026-{(i % 12) + 1:02d}-{(i % 28) + 1:02d}",
    }


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 200
    rng = random.Random(42)
    json.dump([article(rng, i) for i in range(1, n + 1)], sys.stdout, indent=1)


if __name__ == "__main__":
    main()
