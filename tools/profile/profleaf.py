"""Callers of a leaf function in the running samples of some threads of a VICTORY_PROFILE file.
   python profleaf.py <profile> <exe> <leaf substring> <thread name prefix>"""
import collections, re, sys, importlib.util, io, contextlib
spec = importlib.util.spec_from_file_location("prof", __file__.replace("profleaf.py", "prof.py"))
argv = sys.argv
sys.argv = [argv[0], argv[1], argv[2], "0", "0"]
prof = importlib.util.module_from_spec(spec)
with contextlib.redirect_stdout(io.StringIO()):
    spec.loader.exec_module(prof)
leaf, prefix = argv[3], argv[4]
total = run = hit = 0
chains = collections.Counter()
firsts = collections.Counter()
for tid, (start, cpu, samples, name) in prof.threads.items():
    if not name.startswith(prefix):
        continue
    for n, frames in prof.stacks[tid]:
        named = [prof.name_of(x) for x in frames]
        total += n
        if re.search(r"!(Nt|Zw)(Wait|Delay|Yield)", named[0]):
            continue
        run += n
        if leaf in named[0]:
            hit += n
            ours = [x for x in named[1:] if not x.startswith(("std::", "core::", "alloc::", "ntdll!", "KERNEL"))]
            chains[" < ".join(ours[:4])] += n
            firsts[ours[0] if ours else "?"] += n
print(f"threads '{prefix}*': {total} samples, {run} running, {hit} in '{leaf}' ({100*hit/max(run,1):.1f}% of running)")
print("by the first caller:")
for c, n in firsts.most_common(10):
    print(f"  {100*n/max(run,1):5.1f}%  {c}")
print("by the chain:")
for c, n in chains.most_common(12):
    print(f"  {100*n/max(run,1):5.1f}%  {c}")
