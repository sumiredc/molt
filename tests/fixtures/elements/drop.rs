// expect: drop
fn release(buffer: Vec<u8>) {
    drop(buffer);
}
