// expect: pin, box
use std::pin::Pin;

fn pinned(value: u32) -> Pin<Box<u32>> {
    Box::pin(value)
}
