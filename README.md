# VIscosity

**An Xbox Series (Scarlett / GDK) runtime for Windows, tuned for NVIDIA GeForce RTX cards.**

VIscosity builds on [cool_fox](https://github.com/DoubleGhosty/cool_fox), the D3D12 runtime and GFX10 → DXIL shader translator of the VIctory family. cool_fox already runs on NVIDIA cards. VIscosity adds the fixes and defaults that make it run fully on them:

- every shader program translates on NVIDIA's 32-lane waves;
- HID gamepads work without extra tools;
- a launcher's defaults were measured on an RTX 5070 Ti.

> **Legal notice:** This repository contains no copyrighted game code, keys, assets, or executables. It does not provide or link to any third-party software, and does not instruct or encourage bypassing console protections. Use only with software you are legally entitled to run. All original code here is licensed under **GPL-3.0-or-later** (see [LICENSE](LICENSE) and [docs/asset-policy.md](docs/asset-policy.md)).

---

## What VIscosity adds to cool_fox

### Shaders: every program translates on 32-lane hosts

The console runs most compute programs in waves of 64 lanes, but NVIDIA cards run waves of 32. VIscosity translates those programs in a **paired form**:
- each console wave runs as one host wave;
- each host thread runs two console lanes, lane *n* and lane *n* + 32;
- cross-lane operations reach all 64 lanes through the host wave's own operations.

Results on the title's shader pack (`VICTORY_HOST_MAX_WAVE=32 victory translate --pipelines`):
- **26,084 of 26,084 programs** translate, and every pipeline is created.
- Before, six programs were refused in play, among them the water volumetrics and the VFX computes.
- The face-relocation compute (`CS_DefragHeap_ComputeRelocations`) takes the paired form first. cool_fox's half-meet form stays as the fallback.

Other shader and dispatch fixes:
- **Compute dispatches past 65,535 groups** are spread across *z*, and the program gets back the group id it asked for.
- A dispatch counted in threads runs the groups worked out from its own count, not the spread counts.
- **BC7:** channel rotation is applied only in modes 4 and 5. Before, other modes swapped alpha with a colour channel.

### Input
- **HID gamepads** (including the vendor-0x054C pads, with rumble) are read directly, even where XInput does not list them. No extra mapping tool is needed. An XInput pad keeps its slot, and a HID pad moves to a free one.
- **Mouse-look** comes from cool_fox: the mouse moves the keyboard's stand-in pad. Turn it off with `VICTORY_MOUSE=0`, or scale it with `VICTORY_MOUSE_SENS`.

### GPU robustness
- Background workers stop when a host fence wait fails, instead of spinning on a removed device (from [fxceless/victory-fork](https://github.com/fxceless/victory-fork) #1366).

### Launcher defaults for RTX (`scripts/play-cool.ps1`)

It starts from cool_fox's `Play-VIctory.cmd` switches ("pareto-residency") and changes these, each one measured:

| Switch | VIscosity | cool_fox launcher | Why |
|---|---|---|---|
| `VICTORY_TEXTURE_GUARD` | `1` | `0` | Textures are re-hashed whole every eighth frame, not at every check. Texture work went from 15–21 ms / ~300 MB to **~5 ms / ~55 MB** per frame. |
| `VICTORY_HAIR` | `1` | off | Hair strands are drawn. All hair programs translate in the paired form, and no lines cross the screen. |
| `VICTORY_TEXTURE_ROOM_PERCENT` | `94` | `90` | More of a 16 GB card goes to textures. |
| `VICTORY_PIPELINE_BOOK` | `1` | off | Every pipeline the title creates is kept on disk, so `victory warm --used` can pre-build it later and stutter less. |
| `VICTORY_TIMER_LOG` | `1` | off | Frame pacing goes into the log every 5 s. |
| `VICTORY_THIN`, `VICTORY_JOIN_PARTS` | `0` | `0` | Kept off on NVIDIA. With `THIN`, some head skin is not drawn. With `JOIN_PARTS`, frames of the opening video repeat. |

No automatic key presses are sent by default. Enter is Cross/A, and the scripted Enter made the character jump on its own during play.

---

## Status

Tested on an RTX 5070 Ti (16 GB), Windows 11, Series X profile, 4K output drawn at 2560×1440:

- About **12–16 fps** with 7,000–9,700 draws per frame. The limit is the CPU's frame preparation, which takes 50–66 ms. The card is mostly waiting.
- The image is clean: no refused shaders, and hair and water are drawn.
- Lowering the output to 1440p gains little, because the CPU is the limit, and it shows artifacts around the player character. 4K output is the default.
- `-AllShaders` (screen-probe rays and the RT resolve without a BVH) drops to ~3.5 fps. Leave it off.

The next steps for performance are porting:
- victory-fork's per-thread-group card hashing;
- its parallel texture checks;
- the mesh-launch path of the kelvaroq patches.

---

## How to run

### Requirements
- Windows 10/11 x64, CPU with AVX2
- A Direct3D 12 GPU with Shader Model 6.6 and mesh shaders. NVIDIA RTX is the target; AMD runs too, as in cool_fox.
- 32 GB of RAM recommended, with a large fixed pagefile (48–64 GB) on 16–32 GB systems
- Rust toolchain (see `rust-toolchain.toml`) and the MSVC C++ Build Tools with the Windows SDK

### Build

```powershell
git clone https://github.com/FrancescoGelosoEterea/victory-nvidia.git viscosity
cd viscosity
cargo build --release --locked -p victory
```

### Launch

```powershell
$env:VICTORY_GAME_DIR = 'D:\path\to\your\title'   # the folder with game_scarlett_master.exe
.\scripts\play-cool.ps1 -Console series-x
```

The first run builds the shader and pipeline cache in `%LOCALAPPDATA%\VIctory\cache-pareto`, and the screen stays black for a few minutes. Later runs start quickly.

| Option | What it does |
|---|---|
| `-Console series-s` / `series-x` | The console profile. The default is `series-s`. `series-x` draws more, and `-Scale` works only with it. |
| `-Output 720p` … `4k` / `title` | The output size (`VICTORY_OUTPUT`). The default, `title`, keeps the title's own 4K. |
| `-Scale 0.5`–`1.0` | The share of the output drawn before the title's upscaler. The default is the title's 2/3. |
| `-RayTracing` | Ray-traced indirect light (`VICTORY_BVH_BUILDER=1`). Interiors are lit, and it runs about a third slower. |
| `-Stats` | Adds a per-frame cost line to the log: preparation, copies, textures, GPU, copy-back, draws. |
| `-Keys` | Presses Enter every 5 s for the first 4 minutes. |
| `-More @{VICTORY_X='1'}` | Any extra switch, on top of the others. |
| `-GameDir <folder>` | Use this instead of `VICTORY_GAME_DIR`. |

Logs go to `runs\<name>.log`. `scripts\shot.ps1` saves a screenshot of the game window to `runs\shots\`.

The original cool_fox launchers (`Play-VIctory.cmd`, `Play-VIctory-4GB.cmd`) are still there and unchanged.

### Fewer stutters

Long hitches mostly come from building a pipeline the first time it is used. After a long session, pre-build the pipelines the title used:

```powershell
target\release\victory.exe warm --used
```

### Checking the shaders offline

```powershell
$env:VICTORY_HOST_MAX_WAVE = '32'   # translate as for a 32-lane (NVIDIA) host
target\release\victory.exe translate --pipelines --list <path to the title's shaders.rpf>
```

Commands, debugging switches and technical notes are in [docs/README-fxceless.md](docs/README-fxceless.md) and [docs/findings.md](docs/findings.md). Every `VICTORY_...` variable is documented where the source reads it.

---

## Architecture (from cool_fox and VIctory)

- **Recorded command lists** (`VICTORY_THIN`): list calls are recorded on the title's threads and translated on worker threads.
- **Write-guard relay** (`VICTORY_RELAY`): buffers the GPU writes are read back by the dwords that changed, on a relay thread.
- **On-card buffer fills** (`VICTORY_CARD_STEPS`): buffer fills that depend on bytes in card memory run on the GPU.
- **Texture copy queue and read-back:** textures upload on their own copy queue, and textures the GPU writes are copied back for the CPU.
- **Pareto residency:** adaptive GPU memory residency limits, so WDDM does not run out of paging room on 16–32 GB systems.
- **Frame record and replay:** per-pass timing and comparison of shader variants (`VICTORY_RECORD`, [docs/replay.md](docs/replay.md)).

---

## Credits

VIscosity keeps the history and authorship of everything it builds on:

- **[cool_fox](https://github.com/DoubleGhosty/cool_fox)** (codec-d / CGBJ, mirrored by DoubleGhosty): the base runtime, pareto residency, half-meet wave32 lowering, mouse-look.
- **[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and contributors: loader, HLE services, D3D12 backend, shader recompiler.
- **[fxceless/victory-fork](https://github.com/fxceless/victory-fork)**: dispatch spread, the BC7 fix, the fence-wait fix and many upstream pull requests.
- **Pilote Production**: the paired wave64 form.
- **marquezini**: HID gamepad input.
- **[avatarmanz/VIctory-7900XTX](https://github.com/avatarmanz/VIctory-7900XTX)**, **[DexNom/VIctory](https://github.com/DexNom/VIctory)**, **[Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD)**, **[FQY55kai/ZJOU-VICTORY](https://github.com/FQY55kai/ZJOU-VICTORY)**, **[myokex/victory-amd](https://github.com/myokex/victory-amd)**, **Geckomacabre**, **cxy0072019-sketch**: see the cool_fox credits.
- **[XCL](https://github.com/ivall/xcl)** (GPL-2.0-only) was studied as a reference. No XCL code is included.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE) and [docs/asset-policy.md](docs/asset-policy.md).
