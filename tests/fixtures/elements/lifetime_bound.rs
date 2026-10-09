// expect: lifetime_bound, lifetime_param, lifetime_annotation, shared_ref
fn shorten<'a, 'b: 'a>(text: &'b str) -> &'a str {
    text
}
