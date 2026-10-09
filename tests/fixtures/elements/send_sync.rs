// expect: send_sync, generic_param, trait_bound
fn share<T: Send + Sync>(value: T) -> T {
    value
}
