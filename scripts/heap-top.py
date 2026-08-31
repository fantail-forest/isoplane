#!/usr/bin/env python3
"""Rank a dhat-heap.json's program points by bytes still LIVE AT EXIT.

`dhat`'s own viewer sorts by total-ever-allocated, which for a compiler is
dominated by scratch that was freed before the run ended. The question here is
"what does a loaded template *retain*", so the sort key is `eb` — bytes still
live when the profiler dumped — and the run that produced the file is expected
to have held every template alive until then.

  scripts/heap-top.py dhat-heap.json [--top N] [--frames K] [--key eb|gb|tb]
"""
import json, sys, collections

def main():
    a = sys.argv[1:]
    path = next((x for x in a if not x.startswith("--")), "dhat-heap.json")
    def opt(k, d):
        return a[a.index(k) + 1] if k in a else d
    top = int(opt("--top", 25))
    frames = int(opt("--frames", 4))
    key = opt("--key", "eb")

    d = json.load(open(path))
    ftbl = d["ftbl"]
    pps = d["pps"]
    total = sum(p.get(key, 0) for p in pps)
    blocks_key = {"eb": "ebk", "gb": "gbk", "tb": "tbk"}[key]
    label = {"eb": "live at exit", "gb": "live at peak", "tb": "allocated ever"}[key]

    print(f"{path}: {len(pps)} program points, {total:,} bytes {label}")
    print()

    # Roll up by the shallowest interesting frame so the same logical
    # allocation site made from three inlined places lands on one line.
    def sig(p):
        fs = [ftbl[i] for i in p["fs"]]
        keep = [f for f in fs if "dhat::" not in f and "alloc::alloc" not in f
                and "__rust_alloc" not in f and "alloc::raw_vec" not in f]
        return tuple(keep[:frames]) or tuple(fs[:frames])

    roll = collections.defaultdict(lambda: [0, 0])
    for p in pps:
        s = sig(p)
        roll[s][0] += p.get(key, 0)
        roll[s][1] += p.get(blocks_key, 0)

    for s, (b, blocks) in sorted(roll.items(), key=lambda kv: -kv[1][0])[:top]:
        pct = 100.0 * b / total if total else 0
        print(f"{b:>14,} B  {pct:5.1f}%  {blocks:>9,} blocks")
        for f in s:
            print(f"                            {f}")
        print()

if __name__ == "__main__":
    main()
