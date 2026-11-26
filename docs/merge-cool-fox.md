# What was taken from codec-d/cool_fox: `residency`

The two trees are nearly the same code. Compared file by file (crates and apps):

| | |
|---|---|
| identical files | 175 |
| different files | 44 |
| only in codec-d/cool_fox | 1 — `crates/victory-gpu/src/residency.rs` |
| only here | 2 — `crates/victory-loader/src/hooks.rs` (title patches: render scale, own upscaler, watchdog), `crates/victory-shader/src/emit/halves.rs` (faces as integers) |

So this tree already carried everything cool_fox has, except its host-memory
admission ("pareto-residency"), and cool_fox carried nothing of the NVIDIA work
here. The merge is therefore one-way: `residency` in, everything else kept.

## What `residency` is

`crates/victory-gpu/src/residency.rs` reads the machine's free commit
(`GlobalMemoryStatusEx`) and decides, once per second and at each allocation,
whether speculative texture work may grow the host's memory. Nothing else in
this fork looked at the machine's free memory at all (`grep pressured|critical_commit`
found nothing here and 20 sites in cool_fox).

It matters on a 32 GiB host: with the page file at 5 GiB the commit limit is
about 37 GiB, the title's own allocations and the layer's uploads compete for
it, and what gives way is the title — the picture goes black while the layer's
residency warning appears in the log. That was measured here on
`codec-d/cool_fox`'s own build: `host memory pressure: limiting texture
prefetch and idle upload storage`, then a black screen until the run was given
leaner settings.

## What was wired, and where

Four call sites, chosen so the module protects the host without changing what
the layer draws when memory is fine:

| Where | Call | Effect |
|---|---|---|
| `host.rs`, the once-a-second frame sweep | `residency::refresh()` | samples free physical memory and commit, logs the pressure transitions |
| `texture.rs::staging`, when the pool has no piece and a new upload buffer would be committed | `residency::allow_staging_growth(false)` → `note_staging_deferred()`, `return None` | a texture that would need new commit waits for a later frame instead |
| `draw.rs::heap_textures`, at the pass's start | `residency::admit_texture_pass(false)` | the whole texture pass is skipped while there is no room for it |
| (kept for later use) | `allocation_error`, `upload_refused`, `headroom`, `pressured`, `critical_commit`, `prefetch_limit`, `limited_host_ram` | the rest of cool_fox's API, used by its own call sites |

The two `live_span_bytes`/`live_staging_bytes` fields of cool_fox's pressure
log line were dropped: those helpers do not exist in this tree, and the line
keeps the numbers that do (`physical_free_mib`, `available_commit_mib`,
`spare_scratch_mib`, `game_filled_gpu_texture_mib`).

## What was deliberately not taken

- cool_fox's other 44 differing files: they are its AMD measurements and its
  own threading, which this fork has in its own form (avatarmanz's, with the
  NVIDIA changes on top). Taking them would replace the NVIDIA work — the
  render-scale patches, the descriptor-slot pool, the compose pass, the BVH
  miss, the NaN scrub — with the AMD ones.
- `scripts/play-nvidia.ps1`'s settings are unchanged; `residency` needs no
  switch and is on whenever the layer runs.
