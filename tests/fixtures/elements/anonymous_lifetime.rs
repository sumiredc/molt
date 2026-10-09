// expect: anonymous_lifetime, shared_ref
fn length(text: &'_ str) -> usize {
    text.len()
}
