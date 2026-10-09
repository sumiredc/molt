// expect: iterator_adapter, iter_method, closure, iterator_consumer
fn doubled(values: Vec<u32>) -> Vec<u32> {
    values.into_iter().map(|v| v * 2).collect()
}
