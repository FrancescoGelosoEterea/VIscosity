//! A frame of many small draws prepared with one worker up to all of them,
//! as one indirect call, and a culling pass, each measured on this machine.
//!
//!   frameprep [draws] [software]     20000 draws on the fastest card unless said

#[cfg(windows)]
fn main() {
    use victory_frameprep::bench;
    use victory_frameprep::d3d12::Renderer;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let draws = args.iter().find_map(|a| a.parse::<usize>().ok()).unwrap_or(20_000).clamp(1, 2_000_000);
    let software = args.iter().any(|a| a == "software");
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let counts: Vec<usize> = [1, 2, 4, 6, 8, 12, 16, 24, 32].into_iter().filter(|&w| w <= threads).collect();
    let renderer = Renderer::new(threads, software).unwrap_or_else(|e| panic!("{e}"));
    println!("{} | {threads} logical processors | {draws} draws a frame, each its own constants and draw call\n", renderer.adapter_name());
    let scene = bench::scene(draws);
    // "against one": elapsed time with one worker over elapsed time with this
    // many, which is the gain. "work grew": the workers' summed working time
    // against one worker's, which is what running side by side cost (shared
    // caches, two threads a core, the driver's own locks).
    println!("{:<18} {:>7} {:>6} {:>10} {:>11} {:>10} {:>10} {:>9} {:>9} {:>8} {:>10}", "", "workers", "lists", "record ms", "against one", "work grew", "last wake", "imbalance", "submit ms", "GPU ms", "latency ms");
    let rows = bench::recording(&renderer, &scene, &counts, 4, 60).unwrap_or_else(|e| panic!("{e}"));
    let one = rows.first().map_or(1.0, |r| r.record_ms);
    for r in &rows {
        println!(
            "{:<18} {:>7} {:>6} {:>10.3} {:>10.2}x {:>9.2}x {:>7.0} us {:>9.2} {:>9.3} {:>8.3} {:>10.3}",
            r.what, r.workers, r.parts, r.record_ms, one / r.record_ms.max(1e-9), r.speedup * r.record_ms / one.max(1e-9), r.latest_wake_us, r.imbalance, r.submit_ms, r.gpu_ms, r.latency_ms
        );
    }
    println!("\nculling 1,000,000 bounding spheres against a view volume");
    println!("{:>7} {:>10} {:>11} {:>10} {:>10}", "workers", "ms", "against one", "work grew", "last wake");
    let rows = bench::culling(1_000_000, &counts, 40);
    let one = rows.first().map_or(1.0, |r| r.1);
    for (workers, ms, speedup, _, wake) in rows {
        println!("{workers:>7} {ms:>10.3} {:>10.2}x {:>9.2}x {wake:>7.0} us", one / ms.max(1e-9), speedup * ms / one.max(1e-9));
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("frameprep: only the Direct3D 12 backend exists so far, which needs Windows");
}
