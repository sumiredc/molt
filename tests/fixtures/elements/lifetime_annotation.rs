// expect: lifetime_annotation, lifetime_param, shared_ref
struct Excerpt<'a> {
    part: &'a str,
}
