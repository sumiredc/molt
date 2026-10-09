// expect: mutex
use std::sync::Mutex;

fn guarded() -> Mutex<u32> {
    Mutex::new(0)
}
