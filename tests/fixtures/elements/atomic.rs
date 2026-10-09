// expect: atomic
use std::sync::atomic::{AtomicUsize, Ordering};

fn next_id(counter: AtomicUsize) -> usize {
    counter.fetch_add(1, Ordering::SeqCst)
}
