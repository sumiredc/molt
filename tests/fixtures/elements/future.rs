// expect: future, impl_trait, associated_type, async_block
use std::future::Future;

fn later() -> impl Future<Output = u32> {
    async { 1 }
}
