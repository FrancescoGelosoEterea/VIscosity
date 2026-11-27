"""For samples of a thread whose stack holds a frame matching <needle>: the frames just below and above it.
   python profin.py <profile> <exe> <thread name> <needle> [...]"""
import collections, re, sys, importlib.util, io, contextlib
spec = importlib.util.spec_from_file_location("prof", __file__.replace("profin.py", "prof.py"))
argv = sys.argv
sys.argv = [argv[0], argv[1], argv[2], "0", "0"]
prof = importlib.util.module_from_spec(spec)
with contextlib.redirect_stdout(io.StringIO()):
    spec.loader.exec_module(prof)
want = argv[3]
for needle in argv[4:]:
    total = hit = 0
    callers, callees = collections.Counter(), collections.Counter()
    for tid, (start, cpu, samples, name) in prof.threads.items():
        if name != want:
            continue
        for n, frames in prof.stacks[tid]:
            named = [prof.name_of(x) for x in frames]
            total += n
            idx = [i for i, x in enumerate(named) if needle in x]
            if not idx:
                continue
            hit += n
            i = idx[-1]  # outermost
            above = [x for x in named[i + 1:] if not x.startswith(("std::", "core::", "alloc::"))][:3]
            callers[" < ".join(above)] += n
            j = idx[0]
            callees[named[j - 1] if j > 0 else "(itself)"] += n
    print(f"\n== '{needle}' on {want}: {100*hit/max(total,1):.1f}% of its samples")
    print(" called from:")
    for c, n in callers.most_common(6):
        print(f"   {100*n/max(total,1):5.1f}%  {c[:230]}")
    print(" spends it in:")
    for c, n in callees.most_common(6):
        print(f"   {100*n/max(total,1):5.1f}%  {c[:120]}")
