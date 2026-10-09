// expect: result_combinator
fn number(text: String) -> u32 {
    text.parse().unwrap_or(0)
}
