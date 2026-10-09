// expect: unsafe_block, raw_pointer, deref
fn read(pointer: *const u8) -> u8 {
    unsafe { *pointer }
}
