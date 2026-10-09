// expect: mem_swap, mut_ref
fn exchange(a: &mut u32, b: &mut u32) {
    std::mem::swap(a, b);
}
