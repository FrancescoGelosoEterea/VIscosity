"""Reads a VICTORY_PROFILE file and names its frames from the build's PDB (DbgHelp).

    python prof.py <profile.txt> <victory.exe> [threads=10] [rows=28]

Per busy thread: where its samples lie, by the innermost frame (self) and by every
function on the stack (inclusive). A sample whose innermost frame is a system call
that waits is counted as waiting, under the first of our own functions below it.
"""
import collections
import ctypes
import ctypes.wintypes as wt
import os
import re
import sys

path, exe = sys.argv[1], sys.argv[2]
n_threads = int(sys.argv[3]) if len(sys.argv) > 3 else 10
n_rows = int(sys.argv[4]) if len(sys.argv) > 4 else 28

dbghelp = ctypes.WinDLL("dbghelp.dll")
H = wt.HANDLE(0x5A5A)


class SYMBOL_INFOW(ctypes.Structure):
    _fields_ = [
        ("SizeOfStruct", wt.ULONG), ("TypeIndex", wt.ULONG), ("Reserved", ctypes.c_uint64 * 2),
        ("Index", wt.ULONG), ("Size", wt.ULONG), ("ModBase", ctypes.c_uint64), ("Flags", wt.ULONG),
        ("Value", ctypes.c_uint64), ("Address", ctypes.c_uint64), ("Register", wt.ULONG), ("Scope", wt.ULONG),
        ("Tag", wt.ULONG), ("NameLen", wt.ULONG), ("MaxNameLen", wt.ULONG), ("Name", ctypes.c_wchar * 1024),
    ]


dbghelp.SymSetOptions.argtypes = [wt.DWORD]
dbghelp.SymInitializeW.argtypes = [wt.HANDLE, wt.LPCWSTR, wt.BOOL]
dbghelp.SymLoadModuleExW.argtypes = [wt.HANDLE, wt.HANDLE, wt.LPCWSTR, wt.LPCWSTR, ctypes.c_uint64, wt.DWORD, ctypes.c_void_p, wt.DWORD]
dbghelp.SymLoadModuleExW.restype = ctypes.c_uint64
dbghelp.SymFromAddrW.argtypes = [wt.HANDLE, ctypes.c_uint64, ctypes.POINTER(ctypes.c_uint64), ctypes.POINTER(SYMBOL_INFOW)]
dbghelp.SymSetOptions(0x2)  # undecorated names
if not dbghelp.SymInitializeW(H, os.path.dirname(os.path.abspath(exe)), False):
    sys.exit("SymInitialize failed")

bases = {}
next_base = 0x1_0000_0000
system = os.path.join(os.environ["SystemRoot"], "System32")


def base_of(module):
    global next_base
    key = module.lower()
    if key in bases:
        return bases[key]
    file = exe if key.startswith("victory") and key.endswith(".exe") else os.path.join(system, module)
    got = 0
    if os.path.exists(file):
        got = dbghelp.SymLoadModuleExW(H, None, file, None, next_base, 0, None, 0)
        next_base += 0x1_0000_0000
    bases[key] = got
    return got


names = {}
SHORT = re.compile(r"<[^<>]*>")


def tidy(name):
    for _ in range(6):
        name = SHORT.sub("", name)
    name = re.sub(r"::h[0-9a-f]{16}$", "", name)
    name = name.replace("victory_gpu::", "gpu::").replace("victory_hle::", "hle::").replace("victory_loader::", "loader::")
    return name[-78:]


def name_of(frame):
    if frame in names:
        return names[frame]
    out = frame
    m = re.fullmatch(r"(.+)\+0x([0-9a-f]+)", frame)
    if m:
        module, rva = m.group(1), int(m.group(2), 16)
        base = base_of(module)
        if base:
            sym = SYMBOL_INFOW()
            sym.SizeOfStruct = 88
            sym.MaxNameLen = 1023
            disp = ctypes.c_uint64(0)
            if dbghelp.SymFromAddrW(H, base + rva, ctypes.byref(disp), ctypes.byref(sym)):
                short = module.split(".")[0]
                out = tidy(sym.Name) if short.lower().startswith("victory") else f"{short}!{sym.Name}"
        elif not module.lower().startswith("victory"):
            # A module without names (the title): by its 64 KiB neighbourhood.
            out = f"{module.split('.')[0]}+{rva >> 16:#x}xxxx"
    elif frame.startswith("0x"):
        out = "(no module)"
    names[frame] = out
    return out


WAITS = ("NtWait", "ZwWait", "NtDelayExecution", "NtYieldExecution", "NtRemoveIoCompletion", "NtSignalAndWait",
         "NtAlertThreadByThreadId", "RtlpWaitOnAddress", "NtReleaseKeyedEvent", "NtWaitForKeyedEvent",
         "NtWaitForAlertByThreadId", "NtWaitForWorkViaWorkerFactory")

threads = {}
stacks = collections.defaultdict(list)
rounds = 0
with open(path, encoding="utf-8", errors="replace") as f:
    for line in f:
        part = line.rstrip("\n").split(" ", 4)
        if part[0] == "rounds":
            rounds = int(part[1])
        elif part[0] == "thread":
            more = line.rstrip("\n").split(" ", 5)
            threads[int(more[1])] = (more[2], int(more[3]), int(more[4]), more[5] if len(more) > 5 else "")
        elif part[0] == "stack":
            frames = [x for x in part[4].split(";") if x]
            stacks[int(part[1])].append((int(part[3]), frames))

print(f"{rounds} rounds; {len(threads)} threads, {sum(1 for t in stacks if stacks[t])} sampled")
busy = sorted(stacks, key=lambda t: -threads.get(t, ("", 0, 0, ""))[1])[:n_threads]
for tid in busy:
    start, cpu_ms, samples, name = threads.get(tid, ("", 0, 0, ""))
    total = sum(n for n, _ in stacks[tid])
    self_, incl, waiting = collections.Counter(), collections.Counter(), collections.Counter()
    waits = 0
    for n, frames in stacks[tid]:
        named = [name_of(x) for x in frames]
        leaf = named[0]
        is_wait = re.search(r"!(Nt|Zw)(Wait|Delay|Yield|RemoveIoCompletion|SignalAndWait)", leaf) is not None
        if is_wait:
            waits += n
            ours = next((x for x in named if "!" not in x and "+" not in x and x != "(no module)"), named[min(2, len(named) - 1)])
            waiting[ours] += n
        else:
            self_[leaf] += n
        for x in set(named):
            incl[x] += n
    print(f"\n== thread {tid} '{name}' start {name_of(start)}: {cpu_ms} ms of processor, {total} samples, {100 * waits / max(total, 1):.0f}% waiting")
    run = max(total - waits, 1)
    print("  self (of the samples that run):")
    for x, n in self_.most_common(n_rows):
        print(f"    {100 * n / run:5.1f}%  {x}")
    print("  on the stack (of all samples):")
    for x, n in incl.most_common(n_rows):
        print(f"    {100 * n / max(total, 1):5.1f}%  {x}")
    if waits:
        print("  waiting, under:")
        for x, n in waiting.most_common(8):
            print(f"    {100 * n / max(total, 1):5.1f}%  {x}")
