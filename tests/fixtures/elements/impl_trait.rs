// expect: impl_trait
fn keep(value: impl Copy) -> impl Copy {
    value
}
