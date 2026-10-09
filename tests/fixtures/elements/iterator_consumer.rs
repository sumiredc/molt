// expect: iterator_consumer, iter_method
fn total(values: Vec<u32>) -> u32 {
    values.into_iter().sum()
}
