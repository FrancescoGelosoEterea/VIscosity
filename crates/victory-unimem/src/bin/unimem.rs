//! What the machine has, how fast its kinds of memory are, and a pool at work.
//!
//!   unimem info                 the PC's memory and every adapter
//!   unimem bench [adapter] [MB] bandwidth of each kind of memory (64 MB buffers unless said)
//!   unimem demo [adapter]       a pool with a frame's worth of allocations, and what it moved

#[cfg(windows)]
fn main() {
    use victory_unimem::d3d12::{D3d12, Which};
    use victory_unimem::policy::Usage;
    use victory_unimem::pool::{Config, UnifiedPool};
    use victory_unimem::{Backend, hw};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let which = |at: usize| match args.get(at).map(String::as_str) {
        Some("software") => Which::Software,
        Some(n) if n.parse::<u32>().is_ok() => Which::Index(n.parse().unwrap()),
        _ => Which::Fastest,
    };
    match args.first().map(String::as_str) {
        Some("bench") => {
            let backend = D3d12::open(which(1)).unwrap_or_else(|e| panic!("{e}"));
            let mb = args.get(2).and_then(|v| v.parse::<u64>().ok()).unwrap_or(64).clamp(1, 1024);
            println!("{}\n", backend.adapter());
            for m in victory_unimem::bench::run(&backend, mb << 20) {
                println!("{:>10.0} MB/s  {} ({} MB)", m.mb_per_s, m.what, m.bytes >> 20);
            }
        }
        Some("demo") => {
            let backend = D3d12::open(which(1)).unwrap_or_else(|e| panic!("{e}"));
            println!("{}\n", backend.adapter());
            let caps = backend.caps();
            let pool = UnifiedPool::new(backend, Config::default());
            // A frame of a streaming title, in little: geometry and textures
            // filled once, constants and skinned vertices rewritten every
            // frame, a buffer shaders write, and the CPU's own working data.
            let sets = [("static", Usage::STATIC, 48usize << 20, false), ("dynamic", Usage::DYNAMIC, 16 << 20, true), ("card output", Usage::GPU_OUTPUT, 8 << 20, false), ("CPU side", Usage::CPU_SIDE, 8 << 20, true)];
            let mut live = Vec::new();
            for (name, usage, len, every_frame) in sets {
                let h = pool.allocate(len as u64, 65536, usage).unwrap_or_else(|e| panic!("{name}: {e}"));
                let (kind, why) = pool.placement(h).unwrap();
                println!("{name:>12}: {:>3} MB in {}\n              {why}", len >> 20, kind.name());
                live.push((h, vec![0xA5u8; len], every_frame));
            }
            let started = std::time::Instant::now();
            const FRAMES: u32 = 60;
            for frame in 0..FRAMES {
                for (h, data, every_frame) in &live {
                    if *every_frame || frame == 0 {
                        pool.write(*h, 0, data).unwrap();
                    }
                }
                pool.end_frame().unwrap();
            }
            let per_frame = started.elapsed().as_secs_f64() * 1e3 / f64::from(FRAMES);
            println!("\n{FRAMES} frames, {per_frame:.2} ms each for the writes and copies\n{}", pool.snapshot());
            if !caps.unified && !caps.mapped_vram {
                println!("(this adapter has no card memory the CPU maps: every per-frame write is a staged copy)");
            }
        }
        _ => {
            match hw::host_memory() {
                Some(memory) => println!("{memory}"),
                None => println!("system memory: unknown"),
            }
            for adapter in hw::adapters() {
                println!("{adapter}");
            }
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("unimem: only the Direct3D 12 backend exists so far, which needs Windows");
}
