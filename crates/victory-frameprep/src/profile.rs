//! Where a frame's time goes, and how evenly frames arrive.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Cell {
    nanos: AtomicU64,
    calls: AtomicU64,
    longest: AtomicU64,
}

/// Named scopes of CPU time, added to from any thread.
pub struct Scopes {
    names: Vec<&'static str>,
    cells: Vec<Cell>,
}

/// What a scope took since the totals were last taken.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopeTotal {
    pub name: &'static str,
    pub calls: u64,
    pub total: Duration,
    pub longest: Duration,
}

/// Adds its lifetime to a scope when dropped.
pub struct Timed<'a> {
    scopes: &'a Scopes,
    scope: usize,
    started: Instant,
}

impl Drop for Timed<'_> {
    fn drop(&mut self) {
        self.scopes.add(self.scope, self.started.elapsed());
    }
}

impl Scopes {
    /// Scopes by name; a scope is its place in `names`.
    pub fn new(names: &[&'static str]) -> Scopes {
        Scopes { names: names.to_vec(), cells: names.iter().map(|_| Cell::default()).collect() }
    }

    pub fn add(&self, scope: usize, time: Duration) {
        let nanos = time.as_nanos() as u64;
        let cell = &self.cells[scope];
        cell.nanos.fetch_add(nanos, Relaxed);
        cell.calls.fetch_add(1, Relaxed);
        cell.longest.fetch_max(nanos, Relaxed);
    }

    /// Times from now until what it returns is dropped.
    pub fn time(&self, scope: usize) -> Timed<'_> {
        Timed { scopes: self, scope, started: Instant::now() }
    }

    /// The totals since the last call, which start again from nothing.
    pub fn take(&self) -> Vec<ScopeTotal> {
        self.names
            .iter()
            .zip(&self.cells)
            .map(|(&name, cell)| ScopeTotal {
                name,
                calls: cell.calls.swap(0, Relaxed),
                total: Duration::from_nanos(cell.nanos.swap(0, Relaxed)),
                longest: Duration::from_nanos(cell.longest.swap(0, Relaxed)),
            })
            .collect()
    }
}

/// How presented frames were spaced.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pacing {
    pub frames: usize,
    pub mean_ms: f64,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub worst_ms: f64,
    /// The mean distance of an interval from the median: how uneven the
    /// frames are, whatever their rate.
    pub jitter_ms: f64,
}

impl Pacing {
    pub fn fps(&self) -> f64 {
        if self.mean_ms > 0.0 { 1000.0 / self.mean_ms } else { 0.0 }
    }
}

/// The times frames were presented at.
#[derive(Default)]
pub struct FrameClock {
    last: Option<Instant>,
    intervals: Vec<f64>,
}

impl FrameClock {
    /// A frame was presented now.
    pub fn presented(&mut self) {
        self.presented_at(Instant::now());
    }

    pub fn presented_at(&mut self, at: Instant) {
        if let Some(last) = self.last.replace(at) {
            self.intervals.push((at - last).as_secs_f64() * 1e3);
        }
    }

    /// The spacing of the frames since the last call.
    pub fn take(&mut self) -> Pacing {
        let mut sorted = std::mem::take(&mut self.intervals);
        if sorted.is_empty() {
            return Pacing::default();
        }
        sorted.sort_by(f64::total_cmp);
        let at = |share: f64| sorted[((sorted.len() - 1) as f64 * share).round() as usize];
        let median = at(0.5);
        Pacing {
            frames: sorted.len(),
            mean_ms: sorted.iter().sum::<f64>() / sorted.len() as f64,
            median_ms: median,
            p95_ms: at(0.95),
            p99_ms: at(0.99),
            worst_ms: *sorted.last().expect("not empty"),
            jitter_ms: sorted.iter().map(|ms| (ms - median).abs()).sum::<f64>() / sorted.len() as f64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_add_up_and_start_again() {
        let scopes = Scopes::new(&["record", "submit"]);
        scopes.add(0, Duration::from_millis(3));
        scopes.add(0, Duration::from_millis(5));
        {
            let _timed = scopes.time(1);
            std::thread::sleep(Duration::from_millis(2));
        }
        let totals = scopes.take();
        assert_eq!((totals[0].calls, totals[0].total, totals[0].longest), (2, Duration::from_millis(8), Duration::from_millis(5)));
        assert!(totals[1].calls == 1 && totals[1].total >= Duration::from_millis(2));
        assert!(scopes.take().iter().all(|t| t.calls == 0 && t.total.is_zero()));
    }

    #[test]
    fn even_frames_have_no_jitter_and_a_long_one_shows() {
        let start = Instant::now();
        let mut clock = FrameClock::default();
        for frame in 0..=100u64 {
            clock.presented_at(start + Duration::from_millis(frame * 20));
        }
        let even = clock.take();
        assert_eq!(even.frames, 100);
        assert!((even.mean_ms - 20.0).abs() < 1e-6 && even.jitter_ms < 1e-6 && (even.fps() - 50.0).abs() < 1e-6);
        // One frame of 220 ms among ninety-nine of 20, on a clock of its own
        // (the first one's last frame would count as an interval).
        let mut clock = FrameClock::default();
        let mut at = start + Duration::from_secs(10);
        clock.presented_at(at);
        for frame in 0..100 {
            at += Duration::from_millis(if frame == 50 { 220 } else { 20 });
            clock.presented_at(at);
        }
        let uneven = clock.take();
        assert!((uneven.median_ms - 20.0).abs() < 1e-6 && (uneven.worst_ms - 220.0).abs() < 1e-6);
        assert!((uneven.mean_ms - 22.0).abs() < 1e-6 && (uneven.jitter_ms - 2.0).abs() < 1e-6);
    }
}
