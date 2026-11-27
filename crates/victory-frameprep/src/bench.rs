//! The same frame prepared with one worker up to all of them, measured.
//!
//! Each row is the median of the frames run (the first few left out), so one
//! slow frame does not decide it. What to read: the recording's elapsed time
//! against the worker count, and beside it how much of the workers' time was
//! work (`efficiency`), how late the last one woke and how evenly the parts
//! fell. Where the elapsed time stops falling, more threads only add waking
//! and waiting.

use std::time::Duration;

use crate::cull::{Frustum, cull_spheres};
use crate::d3d12::{Draw, FrameStats, Renderer};
use crate::record::RecordError;
use crate::sched::{RunStats, WorkerPool};

/// A measured way of preparing the frame.
#[derive(Clone, Debug)]
pub struct Row {
    pub what: String,
    pub workers: usize,
    pub parts: usize,
    /// Elapsed time of the recording (or of writing the indirect records).
    pub record_ms: f64,
    /// The workers' summed working time over the elapsed time, and that over
    /// the worker count.
    pub speedup: f64,
    pub efficiency: f64,
    pub latest_wake_us: f64,
    pub imbalance: f64,
    /// Handing the lists to the queue.
    pub submit_ms: f64,
    /// Getting allocators back and reset.
    pub prepare_ms: f64,
    /// The card's time for the frame, and from the first list handed over to
    /// the card being done.
    pub gpu_ms: f64,
    pub latency_ms: f64,
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn row(what: &str, workers: usize, frames: &[FrameStats]) -> Row {
    let of = |f: &dyn Fn(&FrameStats) -> f64| median(frames.iter().map(f).collect());
    Row {
        what: what.to_owned(),
        workers,
        parts: frames.first().map_or(0, |f| f.record.parts),
        record_ms: of(&|f| ms(f.record.run.wall)),
        speedup: of(&|f| f.record.run.speedup()),
        efficiency: of(&|f| f.record.run.efficiency()),
        latest_wake_us: of(&|f| f.record.run.latest_wake().as_secs_f64() * 1e6),
        imbalance: of(&|f| f.record.run.imbalance()),
        submit_ms: of(&|f| ms(f.record.submit)),
        prepare_ms: of(&|f| ms(f.prepare)),
        gpu_ms: of(&|f| ms(f.gpu)),
        latency_ms: of(&|f| ms(f.latency)),
    }
}

/// A scene of `count` small triangles spread over the target.
pub fn scene(count: usize) -> Vec<Draw> {
    (0..count)
        .map(|i| {
            let t = i as f32 * 0.618_034;
            Draw { offset: [(t.fract() * 2.0 - 1.0) * 0.95, ((t * 0.37).fract() * 2.0 - 1.0) * 0.95], scale: 0.01, colour: (i as u32).wrapping_mul(2_654_435_761) | 0x0020_2020 }
        })
        .collect()
}

const WARM: usize = 5;

/// `draws` recorded with each worker count of `counts` (`parts_per_worker`
/// lists a worker, so that a slow worker does not hold a whole share), then
/// once as a single indirect call.
pub fn recording(renderer: &Renderer, draws: &[Draw], counts: &[usize], parts_per_worker: usize, frames: usize) -> Result<Vec<Row>, RecordError> {
    let mut rows = Vec::new();
    for &workers in counts.iter().filter(|&&w| w <= renderer.workers()) {
        let pool = WorkerPool::new(workers);
        let parts = workers * if workers == 1 { 1 } else { parts_per_worker.max(1) };
        let mut stats = Vec::new();
        for frame in 0..frames + WARM {
            let s = renderer.frame(&pool, draws, parts)?;
            if frame >= WARM {
                stats.push(s);
            }
        }
        rows.push(row("recorded draws", workers, &stats));
    }
    let workers = counts.iter().copied().filter(|&w| w <= renderer.workers()).max().unwrap_or(1).min(4);
    let pool = WorkerPool::new(workers);
    let mut stats = Vec::new();
    for frame in 0..frames + WARM {
        let s = renderer.frame_indirect(&pool, draws)?;
        if frame >= WARM {
            stats.push(s);
        }
    }
    rows.push(row("one indirect call", workers, &stats));
    Ok(rows)
}

/// `spheres` bounds tested against a view volume with each worker count:
/// (workers, elapsed milliseconds, speedup, efficiency, latest wake in
/// microseconds), the median of `runs`.
pub fn culling(spheres: usize, counts: &[usize], runs: usize) -> Vec<(usize, f64, f64, f64, f64)> {
    let (near, far) = (0.5f32, 2000.0f32);
    let q = far / (far - near);
    let frustum = Frustum::from_view_projection([[1.0, 0.0, 0.0, 0.0], [0.0, 1.78, 0.0, 0.0], [0.0, 0.0, q, 1.0], [0.0, 0.0, -q * near, 0.0]]);
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 40) as f32 / (1u64 << 24) as f32
    };
    let bounds: Vec<[f32; 4]> = (0..spheres).map(|_| [next() * 4000.0 - 2000.0, next() * 400.0 - 200.0, next() * 4000.0 - 2000.0, 0.5 + next() * 20.0]).collect();
    let mut visible = vec![0u8; spheres];
    counts
        .iter()
        .map(|&workers| {
            let pool = WorkerPool::new(workers);
            let stats: Vec<RunStats> = (0..runs + WARM).map(|_| cull_spheres(&pool, &frustum, &bounds, &mut visible, 4096)).skip(WARM).collect();
            (
                workers,
                median(stats.iter().map(|s| ms(s.wall)).collect()),
                median(stats.iter().map(RunStats::speedup).collect()),
                median(stats.iter().map(RunStats::efficiency).collect()),
                median(stats.iter().map(|s| s.latest_wake().as_secs_f64() * 1e6).collect()),
            )
        })
        .collect()
}
