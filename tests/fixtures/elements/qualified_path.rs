// expect: qualified_path
fn zero() -> u32 {
    <u32 as Default>::default()
}
