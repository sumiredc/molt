// expect: mem_take, mut_ref
use std::mem;

fn drain_all(items: &mut Vec<u32>) -> Vec<u32> {
    mem::take(items)
}
