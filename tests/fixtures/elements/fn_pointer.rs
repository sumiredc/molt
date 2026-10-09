// expect: fn_pointer
fn apply(f: fn(u32) -> u32, x: u32) -> u32 {
    f(x)
}
