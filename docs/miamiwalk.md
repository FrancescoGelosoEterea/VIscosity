# This tree: what was added on 2026-11-26 and 27, and what it measured

The tree is trustno1's VIctory-NVIDIA with codec-d/cool_fox's `residency.rs` (the copy `upstream` points at),
plus the work below. Everything was measured on one PC: Ryzen 7 7700, RTX 5070 12 GB, 32 GB of memory, a
2560x1440 display, Windows 11, the Series S profile, the `diag` build.

## Start it

```powershell
cargo build --locked --profile diag -p victory --features diagnostics
scripts\play-miamiwalk.ps1 2> "$env:TEMP\miamiwalk.log"
```

`play-miamiwalk.ps1` is `play-nvidia.ps1` with this PC's settings on top; its header says what each is for.
`-World 720` draws the world at 1280x720 as before, `-Fullscreen` covers the monitor. Without the `2>` the
diagnostics build writes its whole log into the terminal.

Once per change of the translator or the driver, with the same settings in the environment:
`victory shaders <the title's shader archive>` (two minutes), then `victory warm` (88,638 pipelines, about half
an hour, goes on where it was stopped). A run then finds its pipelines in the driver's cache: in the runs before,
358 pipelines took more than a second to make during play, one of them ten.

## What was added

| What | Where | Switch | What it did here |
|---|---|---|---|
| The world at 1920x1080 on Series S | `victory-loader/src/hooks.rs` | `VICTORY_RENDER_SCALE` above 2/3; `VICTORY_GRAPHICS_POOL_MB` | The title stopped at a breakpoint a few seconds in: its graphics pool (1,277 MB on S) and its room for a frame's render targets (297 MB) are sized for the console. Both get their Series X sizes (2,199 and 710 MB). 16-18 fps with the card about half busy, 9.0 of 10.8 GB of its memory in use. |
| Card copies of buffers from 64 KiB up | settings only | `VICTORY_CARD_BUFFERS_MB=3072`, `VICTORY_CARD_BUFFERS_MIN_KB=64` | On codec-d/cool_fox's tree with 512 MB of them the card read the rest across the bus: 3.5 fps against 13-16. |
| Dynamic descriptor slots as ranges | `victory-gpu/src/descriptor/host.rs`, `victory-unimem/src/offset.rs` | `VICTORY_BURY_LAG=24` | In a heavy daytime street the pool of 499,999 slots ran out (2,700-3,900 tables a frame refused, each a dropped draw: missing heads, blinking) and the search for a free run took 667 ms a frame under its lock. A best-fit range allocator cut the search to 40-55 ms; lists reset 24 submissions after they ran, not 64, leave 160,000 slots free and nothing refused. The lag is there against device losses: none in runs of five minutes, longer ones not tried. |
| Textures the title fills after their upload | `victory-gpu/src/draw.rs` | `VICTORY_RECHECK_FRAMES`, `VICTORY_RECHECK_VOLATILE`, `VICTORY_RECHECK_FOREVER`, `VICTORY_RECHECK_REFILLS` | The title writes some textures through another view of their pages, which nothing tells of. One caught changing twice is looked at in every frame; all uploaded ones are looked at again in turn for as long as their slot shows them, at most a few uploads a frame. Whether it removes the black patches the user saw was not confirmed. |
| The ray-tracing skip list as a set | `victory-gpu/src/pipeline.rs` | | The 1,034 hashes were compared one by one at every draw: 22 % of the list threads' running time, 0.9 % after. (The frame rate did not change: the submitting thread is the limit.) |
| Changed card copies kept in a list | `victory-gpu/src/shadow.rs` | | `card_dirty` looked at every card copy before each of 38 submissions a frame: 5 % of the submitting thread. Built, unit-tested, run two minutes; its gain is not measured. |
| Window, picture | `victory-gpu/src/host.rs`, `compose.rs`, `fpsbox.rs` | `VICTORY_FULLSCREEN`, `VICTORY_WINDOW_SIZE`, `VICTORY_SHARPEN`, `VICTORY_SMOOTH`, `VICTORY_FPS_COUNTER` | A borderless or sized window, a small frame counter, sharpening (its range is narrow: 0.3 and 0.6 look the same) or edge smoothing (fuzzy) of the game's plane before the interface is laid over it. |
| F9 takes six frames in a row | `victory-gpu/src/capture.rs` | `VICTORY_CAPTURE=<folder>`, `VICTORY_DUMP_TARGETS=1` | For what flickers with a period of a few frames. |
| `victory-unimem` | `crates/victory-unimem` | `unimem info \| bench \| demo` | A pool over the card's memory, memory the CPU maps and system memory with one handle type: hardware detection, a range allocator, placement by use, staging, eviction, migration, counters, a D3D12 backend. Its range allocator is the one the descriptor pool uses; the rest is not wired into the engine. No Vulkan backend. |
| `victory-frameprep` | `crates/victory-frameprep` | `frameprep` | A measured worker pool, command lists recorded on several threads and run in order, persistent descriptor slots, culling, CPU and GPU timings, on D3D12. Not wired into the engine; no Vulkan backend. |

## Where the time goes

No thread waits for the card. The one thread that prepares submissions (`victory-submit`) is busy all the time
at 1080p: 22 % refreshing copies with its workers, 15 % in `stale_under_flights` and (before the list above)
`card_dirty`, 13 % in D3D12 and the driver, 12 % asking Windows what the title wrote, 9 % in the heap. The eight
list threads idle more than half the time, the title's own workers three quarters.

A profile is only as good as the names put on it: read it with the build that made it.

```powershell
scripts\test-run.ps1 -Name p1 -Seconds 240 -Pregrow 24 -Set "VICTORY_PROFILE=150,165,4,$PWD\runs\p1-profile.txt"
python tools\profile\prof.py runs\p1-profile.txt target\diag\victory.exe 12 30
python tools\profile\profwait.py runs\p1-profile.txt target\diag\victory.exe victory-submit
python tools\profile\profin.py runs\p1-profile.txt target\diag\victory.exe victory-submit "Vec::retain"
python tools\runs\sum.py runs\p1\err.log
```

`prof.py` names frames from `victory.pdb` through DbgHelp; `profwait.py` groups a thread's waits by what it waits
in, `profin.py` says who calls a function and what it spends its time in, `profall.py` lists every sampled thread.

## Open

- **20 frames a second on average is not reached**: 16-18 at 1080p, 13-17 at 720p before.
- **Shadows of trees and parked cars** came and went with a period of three frames in one place (the title seems
  to draw its shadow cascades in some frames only; in five of nine captured frames it drew none). Not found yet.
  A later run of the user's, with `VICTORY_CAPTURE` set, had them "mostly fixed"; nothing was changed for it.
- **Artifacts on the textures of passers-by**, reported in that same run; not looked at.
- **A frame now and then with every sunlit surface dark red**, twice in a minute's capture.
- **No ray tracing**: on NVIDIA seven of the title's 64-lane compute programs are refused, the structure builder
  runs all or none, and 1,034 ray-tracing shaders are skipped. The estimate of `ambient.rs` stands in.
- **Memory**: the process takes 22-24 GB and 6 GB more is shared with the card; a 32 GB PC has 3 GB free after
  a few minutes, and one run fell to 2-5 fps then (the title's own threads took 150 ms a frame; cause not found).
