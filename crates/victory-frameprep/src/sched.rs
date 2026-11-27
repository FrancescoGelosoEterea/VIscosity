//! A worker pool whose runs are measured.
//!
//! `workers` threads in all, the caller's among them: `parallel_for` hands
//! out the numbers `0..count` in pieces of `grain`, each participant taking
//! the next piece when it has done its last (a counter, no queue and no lock
//! on the way to a piece), and returns when every piece is done. What comes
//! back says where the time went: how long each participant worked, how long
//! after the call it took its first piece, and how many numbers it got.

use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// One participant's part in a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkerRun {
    /// Time inside the work itself.
    pub busy: Duration,
    /// From the call to its first piece: how late it woke. `None` if it took
    /// none (the others were done before it was awake).
    pub woke_after: Option<Duration>,
    /// Numbers it was given.
    pub items: usize,
}

/// A run of [`WorkerPool::parallel_for`].
#[derive(Clone, Debug, Default)]
pub struct RunStats {
    pub wall: Duration,
    pub workers: Vec<WorkerRun>,
}

impl RunStats {
    /// The participants' working time, summed.
    pub fn busy(&self) -> Duration {
        self.workers.iter().map(|w| w.busy).sum()
    }

    /// How many times faster than one thread doing all the work measured:
    /// summed working time over elapsed time.
    pub fn speedup(&self) -> f64 {
        self.busy().as_secs_f64() / self.wall.as_secs_f64().max(1e-9)
    }

    /// The share of the participants' time in the run that was work
    /// (1: none of them waited or woke late).
    pub fn efficiency(&self) -> f64 {
        self.speedup() / self.workers.len().max(1) as f64
    }

    /// The longest any participant took to take its first piece.
    pub fn latest_wake(&self) -> Duration {
        self.workers.iter().filter_map(|w| w.woke_after).max().unwrap_or_default()
    }

    /// Participants that got nothing.
    pub fn idle_workers(&self) -> usize {
        self.workers.iter().filter(|w| w.items == 0).count()
    }

    /// The largest share of the numbers one participant got, against an even
    /// share (1: even).
    pub fn imbalance(&self) -> f64 {
        let all: usize = self.workers.iter().map(|w| w.items).sum();
        let most = self.workers.iter().map(|w| w.items).max().unwrap_or(0);
        if all == 0 { 1.0 } else { most as f64 * self.workers.len() as f64 / all as f64 }
    }
}

/// The work of one run, as the threads see it. The pointers are the caller's
/// stack: they hold only until `parallel_for` returns, which it does when
/// every thread has let go of them.
#[derive(Clone, Copy)]
struct Job {
    work: *const (dyn Fn(Range<usize>, usize) + Sync),
    next: *const AtomicUsize,
    runs: *const [Mutex<WorkerRun>],
    count: usize,
    grain: usize,
    posted: Instant,
}

// SAFETY: the pointers are to `Sync` things that outlive the job (see `Job`).
unsafe impl Send for Job {}

struct State {
    job: Option<Job>,
    /// Counts the runs: a thread takes each once.
    round: u64,
    /// Threads that have not finished the round's job.
    at_work: usize,
    closing: bool,
}

struct Shared {
    state: Mutex<State>,
    posted: Condvar,
    finished: Condvar,
}

/// The pool.
pub struct WorkerPool {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
    /// One run at a time.
    running: Mutex<()>,
}

fn take_pieces(job: &Job, index: usize) {
    // SAFETY: see `Job`: the three live until the run's caller returns.
    let (work, next, runs) = unsafe { (&*job.work, &*job.next, &*job.runs) };
    let mut run = WorkerRun::default();
    loop {
        let from = next.fetch_add(job.grain, Ordering::Relaxed);
        if from >= job.count {
            break;
        }
        let to = (from + job.grain).min(job.count);
        let started = Instant::now();
        run.woke_after.get_or_insert(started - job.posted);
        work(from..to, index);
        run.busy += started.elapsed();
        run.items += to - from;
    }
    *runs[index].lock().unwrap_or_else(|e| e.into_inner()) = run;
}

impl WorkerPool {
    /// A pool of `workers` participants (1 at least): `workers - 1` threads
    /// and the thread that calls.
    pub fn new(workers: usize) -> WorkerPool {
        let shared = Arc::new(Shared {
            state: Mutex::new(State { job: None, round: 0, at_work: 0, closing: false }),
            posted: Condvar::new(),
            finished: Condvar::new(),
        });
        let threads = (1..workers.max(1))
            .map(|index| {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name(format!("frameprep-{index}"))
                    .spawn(move || {
                        let mut seen = 0u64;
                        loop {
                            let job = {
                                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                                while !state.closing && state.round == seen {
                                    state = shared.posted.wait(state).unwrap_or_else(|e| e.into_inner());
                                }
                                if state.closing {
                                    return;
                                }
                                seen = state.round;
                                state.job
                            };
                            if let Some(job) = job {
                                take_pieces(&job, index);
                            }
                            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                            state.at_work -= 1;
                            if state.at_work == 0 {
                                shared.finished.notify_all();
                            }
                        }
                    })
                    .expect("a worker thread")
            })
            .collect();
        WorkerPool { shared, threads, running: Mutex::new(()) }
    }

    /// Participants in a run, the caller counted.
    pub fn workers(&self) -> usize {
        self.threads.len() + 1
    }

    /// Calls `work` with every number of `0..count`, in pieces of `grain`
    /// (1 at least), on the pool's participants, and returns when all are
    /// done. `work` gets the piece and the participant's index
    /// (`0..workers()`, 0 the caller): two calls with the same index never
    /// run at once.
    pub fn parallel_for(&self, count: usize, grain: usize, work: &(dyn Fn(Range<usize>, usize) + Sync)) -> RunStats {
        let _one = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let next = AtomicUsize::new(0);
        let runs: Vec<Mutex<WorkerRun>> = (0..self.workers()).map(|_| Mutex::default()).collect();
        let posted = Instant::now();
        // SAFETY: the lifetime of `work` is erased for the threads; this
        // call does not return before each of them has finished with the job
        // (`at_work` back at 0), so none of them sees it afterwards.
        let work: *const (dyn Fn(Range<usize>, usize) + Sync) = unsafe { std::mem::transmute(work) };
        let job = Job { work, next: &next, runs: &runs[..], count, grain: grain.max(1), posted };
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.job = Some(job);
            state.round += 1;
            state.at_work = self.threads.len();
        }
        self.shared.posted.notify_all();
        take_pieces(&job, 0);
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            while state.at_work > 0 {
                state = self.shared.finished.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            state.job = None;
        }
        let wall = posted.elapsed();
        RunStats { wall, workers: runs.into_iter().map(|r| r.into_inner().unwrap_or_else(|e| e.into_inner())).collect() }
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner()).closing = true;
        self.shared.posted.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn every_number_is_done_once_whatever_the_pool() {
        for workers in [1, 2, 5, 16] {
            let pool = WorkerPool::new(workers);
            for (count, grain) in [(0usize, 1usize), (1, 1), (1000, 7), (1000, 5000), (77, 0)] {
                let seen: Vec<AtomicU64> = (0..count).map(|_| AtomicU64::new(0)).collect();
                let stats = pool.parallel_for(count, grain, &|piece, index| {
                    assert!(index < workers);
                    for i in piece {
                        seen[i].fetch_add(1, Ordering::Relaxed);
                    }
                });
                assert!(seen.iter().all(|n| n.load(Ordering::Relaxed) == 1), "{workers} workers, {count} by {grain}");
                assert_eq!(stats.workers.len(), workers);
                assert_eq!(stats.workers.iter().map(|w| w.items).sum::<usize>(), count);
            }
        }
    }

    #[test]
    fn the_same_index_never_runs_twice_at_once() {
        let pool = WorkerPool::new(8);
        let inside: Vec<AtomicUsize> = (0..8).map(|_| AtomicUsize::new(0)).collect();
        for _ in 0..50 {
            pool.parallel_for(4000, 16, &|piece, index| {
                assert_eq!(inside[index].fetch_add(1, Ordering::SeqCst), 0);
                std::hint::black_box(piece.sum::<usize>());
                inside[index].fetch_sub(1, Ordering::SeqCst);
            });
        }
    }

    #[test]
    fn work_that_takes_time_is_spread_and_the_run_says_so() {
        let pool = WorkerPool::new(4);
        let stats = pool.parallel_for(16, 1, &|_, _| std::thread::sleep(Duration::from_millis(5)));
        // Sixteen sleeps of 5 ms on four participants: about 20 ms, not 80.
        assert!(stats.wall < Duration::from_millis(60), "{:?}", stats.wall);
        assert!(stats.speedup() > 2.0, "{}", stats.speedup());
        assert_eq!(stats.idle_workers(), 0);
        assert!(stats.busy() >= Duration::from_millis(75));
    }
}
