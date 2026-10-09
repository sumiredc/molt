// expect: raw_pointer, shared_ref
fn address(value: &u32) -> *const u32 {
    value as *const u32
}
