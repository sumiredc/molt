// expect: mem_replace, mut_ref
fn swap_in(slot: &mut u32, value: u32) -> u32 {
    std::mem::replace(slot, value)
}
