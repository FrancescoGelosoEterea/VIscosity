//! A frame's items recorded into several command lists at once and run in
//! the items' order.
//!
//! The items (draws, here; anything a list takes) are cut into `parts`
//! stretches that follow one another. Each part is recorded into a command
//! list of its own by whichever worker takes it; the closed lists are then
//! handed to the queue in the parts' order, so the card runs the items in
//! the order they were given, whoever recorded them and whenever.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::sched::{RunStats, WorkerPool};

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("the device refused: {0}")]
    Device(String),
    #[error("the adapter does not offer {0}")]
    Unsupported(&'static str),
}

/// A graphics API's command lists, as [`record_frame`] uses them.
pub trait RecordBackend: Sync {
    /// What is recorded.
    type Item: Sync;
    /// An open list.
    type List;
    /// A closed list, ready to run.
    type Closed: Send;

    /// An open list for part `part`, recorded by worker `worker`: with
    /// everything set that every part needs (targets, pipeline, buffers).
    /// A worker has one list open at a time.
    fn begin(&self, worker: usize, part: usize) -> Result<Self::List, RecordError>;

    fn record(&self, list: &mut Self::List, item: &Self::Item);

    fn end(&self, list: Self::List) -> Result<Self::Closed, RecordError>;

    /// Hands the lists to the queue, to run in this order.
    fn submit(&self, parts: Vec<Self::Closed>) -> Result<(), RecordError>;
}

/// How a frame's recording went.
#[derive(Clone, Debug, Default)]
pub struct FrameRecord {
    pub parts: usize,
    /// The recording on the pool.
    pub run: RunStats,
    /// The call that hands the lists to the queue.
    pub submit: Duration,
}

/// Records `items` into `parts` lists on `pool` and submits them in order.
/// `parts` is brought into `1..=items.len()`; with no items nothing is
/// recorded or submitted.
pub fn record_frame<B: RecordBackend>(pool: &WorkerPool, backend: &B, items: &[B::Item], parts: usize) -> Result<FrameRecord, RecordError> {
    if items.is_empty() {
        return Ok(FrameRecord::default());
    }
    let parts = parts.clamp(1, items.len());
    // Part `p` is the items from `cut(p)` up to `cut(p + 1)`.
    let cut = |p: usize| p * items.len() / parts;
    let closed: Vec<Mutex<Option<Result<B::Closed, RecordError>>>> = (0..parts).map(|_| Mutex::new(None)).collect();
    let run = pool.parallel_for(parts, 1, &|pieces, worker| {
        for part in pieces {
            let recorded = backend.begin(worker, part).and_then(|mut list| {
                for item in &items[cut(part)..cut(part + 1)] {
                    backend.record(&mut list, item);
                }
                backend.end(list)
            });
            *closed[part].lock().unwrap_or_else(|e| e.into_inner()) = Some(recorded);
        }
    });
    let mut lists = Vec::with_capacity(parts);
    for slot in closed {
        lists.push(slot.into_inner().unwrap_or_else(|e| e.into_inner()).expect("every part was recorded")?);
    }
    let started = Instant::now();
    backend.submit(lists)?;
    Ok(FrameRecord { parts, run, submit: started.elapsed() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A backend that writes down what it is given.
    struct Log {
        ran: Mutex<Vec<u32>>,
        open: Vec<AtomicUsize>,
    }

    impl RecordBackend for Log {
        type Item = u32;
        type List = (usize, Vec<u32>);
        type Closed = Vec<u32>;

        fn begin(&self, worker: usize, _part: usize) -> Result<Self::List, RecordError> {
            assert_eq!(self.open[worker].fetch_add(1, Ordering::SeqCst), 0, "a worker has one list open at a time");
            Ok((worker, Vec::new()))
        }

        fn record(&self, list: &mut Self::List, item: &u32) {
            list.1.push(*item);
        }

        fn end(&self, list: Self::List) -> Result<Vec<u32>, RecordError> {
            self.open[list.0].fetch_sub(1, Ordering::SeqCst);
            Ok(list.1)
        }

        fn submit(&self, parts: Vec<Vec<u32>>) -> Result<(), RecordError> {
            self.ran.lock().unwrap().extend(parts.into_iter().flatten());
            Ok(())
        }
    }

    #[test]
    fn items_run_in_their_order_whoever_records_them() {
        let items: Vec<u32> = (0..10_007).collect();
        for workers in [1, 3, 8] {
            let pool = WorkerPool::new(workers);
            for parts in [1, 2, 7, 64, 100_000] {
                let log = Log { ran: Mutex::default(), open: (0..workers).map(|_| AtomicUsize::new(0)).collect() };
                let frame = record_frame(&pool, &log, &items, parts).unwrap();
                assert_eq!(frame.parts, parts.min(items.len()));
                assert_eq!(*log.ran.lock().unwrap(), items, "{workers} workers, {parts} parts");
            }
        }
        let pool = WorkerPool::new(2);
        let log = Log { ran: Mutex::default(), open: (0..2).map(|_| AtomicUsize::new(0)).collect() };
        assert_eq!(record_frame(&pool, &log, &[], 4).unwrap().parts, 0);
    }
}
