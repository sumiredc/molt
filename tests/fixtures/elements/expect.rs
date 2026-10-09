// expect: expect
fn number(text: String) -> u32 {
    text.parse().expect("a number")
}
