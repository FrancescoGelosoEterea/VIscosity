# VIscosity: quick start

VIscosity runs one Xbox Series title on a Windows PC with an NVIDIA card. The title's own program
runs directly on your processor; its graphics code is translated for your card while it runs.
Nothing of the title is in this package: you need your own copy of it.

This page is the short version. The project's full notes are in [README.md](README.md) and
[docs/](docs/).

## What you need

- Windows 10 or 11, 64-bit.
- A processor with AVX2 (any Ryzen, or Intel from 2013 on). Eight cores are used fully.
- An NVIDIA RTX card with 12 GB of memory or more. Frame generation needs an RTX 40 or 50.
- 32 GB of RAM, and a page file of 16 GB or more: the title takes 26 to 29 GB. Close browsers
  and other large programs before you start.
- Your own copy of the title: the folder that holds `game_scarlett_master.exe`.

## Run it

Open PowerShell in this folder (not as administrator). If Windows refuses to run the scripts,
allow them for this window first:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
Get-ChildItem -Recurse | Unblock-File
```

**1. Prepare, once** (a few minutes, uses every core). This translates the title's shaders ahead of
time; without it the first minutes of play stand still again and again.

```powershell
.\scripts\play-viscosity.ps1 -GameDir 'D:\path\to\the\title' -Prepare
```

**2. Play.**

```powershell
.\scripts\play-viscosity.ps1 -GameDir 'D:\path\to\the\title'
```

The release package holds a ready-built `target\diag\victory.exe`, so there is nothing to compile
(from a clone of the repository, build it first: see [Build it yourself](#build-it-yourself)). The
first start takes a minute or two before the menu shows.

**3. After your first session, prepare again.** The first time anything is drawn, your card's
driver has to compile for it, and the picture stops for up to a second. To get that out of the
way, play once with the list of the title's pipelines written down, then let them all be built
(up to an hour, once):

```powershell
.\scripts\play-viscosity.ps1 -GameDir 'D:\path\to\the\title' -More @{VICTORY_PIPELINE_BOOK='1'}
.\scripts\play-viscosity.ps1 -GameDir 'D:\path\to\the\title' -Prepare
```

## Frame generation (RTX 40 and 50)

The layer can show two frames for each one the title renders, with NVIDIA DLSS Frame
Generation. NVIDIA's files are not in this package (their licence does not let us pass them on):

1. Download the Streamline SDK release **2.14.16** from
   <https://github.com/NVIDIA-RTX/Streamline/releases> (`streamline-sdk-v2.14.16.zip`).
2. Copy its `bin\x64` folder to `nvidia-sdk\Streamline\bin\x64` inside this folder (it must hold
   `sl.interposer.dll`).
3. Switch on "Hardware-accelerated GPU scheduling" in Windows' graphics settings.
4. Start with `-FrameGen`:

```powershell
.\scripts\play-viscosity.ps1 -GameDir 'D:\path\to\the\title' -FrameGen
```

Frames are only generated while the game's window has the focus.

## Switches

| Switch | What it does |
|---|---|
| `-GameDir <folder>` | Where the title is. Or set `$env:VICTORY_GAME_DIR` once. |
| `-Prepare` | Does not start the title: fills the shader cache and builds the pipelines written down. |
| `-FrameGen` | DLSS Frame Generation, two frames shown for each rendered (`-FrameGenX 3` to `6` for more). |
| `-Cap 30` | At most that many frames shown a second, evenly spaced (30 unless said; `0`: no cap). |
| `-World 720` | Renders the world at 1280x720 instead of 1920x1080 (a little faster, softer). |
| `-Fullscreen` | Borderless over the whole monitor instead of a window. |
| `-Mouse` | The mouse as the right stick when you play on the keyboard. It holds the cursor in the window. |
| `-HeatHaze` | Keeps the title's heat-haze pass (left out: on NVIDIA it paints black blotches). |
| `-Freezes` | Diagnosis: records what the layer did around every frame of 200 ms or more. Slower. |
| `-More @{NAME='value'}` | Any other setting of the layer. |

The log of a run is `runs\viscosity-<date>-<time>.log`. Saves go to `%LOCALAPPDATA%\victory\data`
(or where `$env:VICTORY_DATA_DIR` says), the caches to `%LOCALAPPDATA%\VIctory\cache-nvidia`.

Xbox pads and HID gamepads (the vendor-0x054C ones too, with rumble) work as they are. Without a pad the keyboard stands in for
one; add `-Mouse` to look around with the mouse.

## What to expect

Measured on an RTX 5070 12 GB with a Ryzen 7 7700 and 32 GB of RAM, world at 1920x1080:

- 14 to 18 frames rendered a second in the open world; with `-FrameGen`, a steady 30 shown.
- Ray tracing is off. Reflections that need it show the sky's colour instead.
- The picture stops for a fraction of a second now and then while the world is streamed in,
  most of all in the first minute after loading.
- Glass and windows are not right yet. Mirrors have not been looked at.
- AMD and Intel cards are not what this tree is tuned for.

## Build it yourself

Only if you change the code. Install [Rust](https://rustup.rs) with the Visual Studio C++ build
tools; the toolchain version is pinned and installs itself.

```powershell
cargo build --locked --profile diag -p victory --features diagnostics
```

Then run `-Prepare` again: a changed translator means a new shader cache.

## What it is made of

- **Rust** (edition 2024, toolchain 1.98.1), one workspace of crates, built for
  `x86_64-pc-windows-msvc`. No C or C++ of our own.
- **Direct3D 12** on the PC side, through the `windows` crate: the title's console Direct3D calls
  are recorded and replayed as PC ones, with mesh shaders and shader model 6.6.
- **A shader translator of our own** (`crates/victory-shader`): the title ships its shaders as
  machine code for the console's AMD graphics chip. The translator reads that code, rebuilds its
  structure, and writes DXIL, the form PC drivers take. The results are kept in a cache on disk.
- **A loader and a stand-in for the console's system** (`crates/victory-loader`,
  `crates/victory-hle`): the title's 64-bit program is mapped into our process and runs as it is;
  the system functions it calls (memory, files, input, sound, saves) are ours.
- **Memory tracking** (`crates/victory-gpu`): on the console the processor and the graphics chip
  share one memory. On a PC they do not, so the layer watches what the title writes and keeps
  copies on the card in step. This is where most of the processor's time goes.
- **NVIDIA Streamline**, loaded at run time if present: DLSS Frame Generation and Reflex.
- **PowerShell** launchers in `scripts\`, and a few Python helpers in `tools\` for reading logs.

Where things are:

| Folder | What is in it |
|---|---|
| `apps\victory` | The command-line program: `run`, `shaders`, `warm`, `translate`, `framegen-check`. |
| `crates\victory-gpu` | Direct3D translation, memory tracking, presenting, frame generation. |
| `crates\victory-shader` | The shader translator. |
| `crates\victory-hle` | The console system functions the title calls. |
| `crates\victory-loader`, `victory-pe` | Loading and patching the title's program. |
| `crates\victory-formats`, `victory-game`, `victory-core`, `victory-unimem` | Texture formats, what is specific to the title, configuration and logging, memory placement. |
| `scripts\` | Launchers. `play-viscosity.ps1` is the one to use. |
| `docs\` | Longer notes: how it is built, what was measured, what is known to be wrong. |

## Licence and credits

GPL-3.0-or-later ([LICENSE](LICENSE)). VIscosity stands on VIctory and its forks: the people and
projects are named in [README.md](README.md).
