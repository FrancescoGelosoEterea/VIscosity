"""Summarises runs from their err.log (python tools/runs/sum.py runs/<name>/err.log ...): the frame-rate lines of
the world (5,000 draws a frame and more), second half. A line is written about every five seconds."""
import re
import statistics as s
import sys

LINE = re.compile(
    r"host: ([0-9.]+) fps, ([0-9.]+) ms/frame \(([0-9.]+) preparing: copies ([0-9.]+) ms ([0-9.]+) MB.*?; "
    r"([0-9.]+) on the GPU; ([0-9.]+) copying back ([0-9.]+) MB\), (\d+) draws/frame, dropped (\d+) .*?"
    r"uploaded so far \((\d+) MB\), (\d+) waiting"
)
LOOK = re.compile(r"at the last look (\d+) MB of the card.s and (\d+) MB")

for path in sys.argv[1:]:
    rows = []
    shared = ""
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            m = LINE.search(line)
            if m and int(m.group(9)) > 5000:
                rows.append([float(x) for x in m.groups()])
            m = LOOK.search(line)
            if m:
                shared = f"card {m.group(1)} MB, shared {m.group(2)} MB"
    name = path.replace("\\", "/").split("/")[-2]
    if not rows:
        print(name, "no world lines")
        continue
    half = rows[len(rows) // 2:]

    def med(i):
        return s.median(r[i] for r in half)

    # Frames over time: each line is about a second, so the mean of the rates is the average rate.
    avg = sum(r[0] for r in half) / len(half)
    print(
        f"{name}: {len(rows)} lines in the world | 2nd half: avg {avg:.1f} fps, median {med(0):.1f}, min {min(r[0] for r in half):.1f}"
        f" | preparing {med(2):.0f} ms, copies {med(3):.0f} ms/{med(4):.0f} MB, GPU {med(5):.0f}, back {med(6):.0f} ms"
        f" | draws {med(8):.0f}, dropped {med(9):.0f}, waiting {med(11):.0f} | {shared}"
    )
