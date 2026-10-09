// expect: transmute, unsafe_block
fn bits(value: f32) -> u32 {
    unsafe { std::mem::transmute(value) }
}
