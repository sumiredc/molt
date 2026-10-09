// expect: lifetime_param, lifetime_annotation, shared_ref
fn longest<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.len() > b.len() { a } else { b }
}
