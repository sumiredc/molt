// expect: rw_lock
use std::sync::RwLock;

fn settings() -> RwLock<u32> {
    RwLock::new(0)
}
