// expect: closure
fn sum_of(a: u32, b: u32) -> u32 {
    let add = |x: u32, y: u32| x + y;
    add(a, b)
}
