# Measured on this machine (RTX 3080 Ti 12 GB, Ryzen 7 5700X, 32 GiB)

First run of this tree that reached the world here, after the `residency` merge
(`docs/merge-cool-fox.md`) and with `scripts/play-nvidia.ps1`'s speed set at
Series S, half render scale (540p, then the title's own upscaler).

## The frame

```
14.0 fps, 71.7 ms/frame
  44.7 ms preparing  (copies 18.8 ms / 45.0 MB, textures 0.8 ms / 26.4 MB)
  18.5 ms copying back / 14.5 MB
  3794 draws a frame
  the card about 66% busy
```

Stable for the whole 10-minute run: no device loss, no black screen, menus at
30 fps.

## What the other trees did on the same machine, same prologue

| Build | Resolution | Frame | fps |
|---|---|---|---|
| codec-d/cool_fox | 1440p | 394 ms | 2.5 |
| codec-d/cool_fox | 144p | 361 ms | 2.8 |
| Pavl3000/ztrx | 1440p | 470 ms | 2.1 |
| fxceless fork (this fork's ancestor) | 1440p | ~390 ms | 2.6 |
| **this tree + `residency`** | **540p** | **71.7 ms** | **14.0** |

Resolution alone was measured not to be the lever on this card: 1440p → 144p
(100x fewer pixels) moved 394 ms to 361 ms, with the card still 96% busy. What
moved it 5x is the set this fork carries: the title patched to render at half
scale with its own upscaler, `VICTORY_BVH_MISS`, the 1,034 ray-tracing shaders
skipped, `VICTORY_DYNAMIC_SLOTS=524288` (NVIDIA's 1,000,000-view cap), the
Series S compose plane, and the `residency` admission that keeps the layer from
taking the host's commit away from the title.

## Where the frame goes now

The card is 66% busy (about 47 ms) and preparing costs 44.7 ms, so the two are
balanced: further resolution cuts save little, and the CPU side (copies 18.8 ms,
copy-back 18.5 ms) is next. The reference machine's CPU (9800X3D) is much
faster than this one's (5700X), which is most of the difference between 14 fps
here and the 18-22 fps of the README.
