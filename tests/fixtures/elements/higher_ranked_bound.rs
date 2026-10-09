// expect: higher_ranked_bound, generic_param, where_clause, trait_bound, shared_ref, lifetime_annotation
fn same<T>(a: T, b: T) -> bool
where
    for<'a> &'a T: PartialEq,
{
    &a == &b
}
