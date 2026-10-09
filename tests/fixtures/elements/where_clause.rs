// expect: where_clause, generic_param, trait_bound
fn same<T>(value: T) -> T
where
    T: Copy,
{
    value
}
