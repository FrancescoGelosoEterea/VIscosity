"""Waiting samples of some threads of a VICTORY_PROFILE file, by the chain of our own functions above the wait.
   python profwait.py <profile> <exe> <thread name or id> [...]"""
import collections, re, subprocess, sys, importlib.util, io, contextlib
spec = importlib.util.spec_from_file_location("prof", __file__.replace("profwait.py", "prof.py"))
argv = sys.argv
sys.argv = [argv[0], argv[1], argv[2], "0", "0"]
prof = importlib.util.module_from_spec(spec)
with contextlib.redirect_stdout(io.StringIO()):
    spec.loader.exec_module(prof)
for want in argv[3:]:
    for tid, (start, cpu, samples, name) in prof.threads.items():
        if want not in (name, str(tid)):
            continue
        total = sum(n for n, _ in prof.stacks[tid])
        chains = collections.Counter()
        for n, frames in prof.stacks[tid]:
            named = [prof.name_of(x) for x in frames]
            if not re.search(r"!(Nt|Zw)(Wait|Delay|Yield)", named[0]):
                continue
            ours = [x for x in named if "!" not in x and "+" not in x and not x.startswith(("std::", "core::", "alloc::"))]
            how = next((x.split("::")[-2] + "::" + x.split("::")[-1] for x in named if x.startswith("std::sys::sync")), "other")
            chains[(how, " < ".join(ours[:5]))] += n
        print(f"\n== {name} {tid}: {total} samples")
        for (how, chain), n in chains.most_common(14):
            print(f"  {100 * n / total:5.1f}%  [{how}] {chain}")
