"""Every sampled thread of a VICTORY_PROFILE file: how much it waits and where (the three frames above the system call)."""
import collections, re, sys, importlib.util, io, contextlib
spec = importlib.util.spec_from_file_location("prof", __file__.replace("profall.py", "prof.py"))
argv = sys.argv
sys.argv = [argv[0], argv[1], argv[2], "0", "0"]
prof = importlib.util.module_from_spec(spec)
with contextlib.redirect_stdout(io.StringIO()):
    spec.loader.exec_module(prof)
rows = []
for tid, st in prof.stacks.items():
    start, cpu, samples, name = prof.threads[tid]
    total = sum(n for n, _ in st)
    if total < 300:
        continue
    sites = collections.Counter()
    waits = 0
    for n, frames in st:
        named = [prof.name_of(x) for x in frames]
        if re.search(r"!(Nt|Zw)(Wait|Delay|Yield|RemoveIo)", named[0]):
            waits += n
            above = [x for x in named[1:] if not x.startswith(("ntdll!", "KERNELBASE!", "KERNEL32!"))][:3]
            # raw addresses for title frames
            raw = [f for f, x in zip(frames, named) if not x.startswith(("ntdll!", "KERNELBASE!", "KERNEL32!"))][1:4] if False else None
            sites[" < ".join(above)] += n
    rows.append((cpu, tid, name, total, waits, sites))
for cpu, tid, name, total, waits, sites in sorted(rows, reverse=True)[: int(argv[3]) if len(argv) > 3 else 40]:
    print(f"{tid:6} {name[:22]:22} cpu {cpu:5} ms, {total:4} samples, {100 * waits / total:3.0f}% waiting")
    for site, n in sites.most_common(3):
        if n * 100 / total >= 4:
            print(f"         {100 * n / total:5.1f}%  {site[:230]}")
