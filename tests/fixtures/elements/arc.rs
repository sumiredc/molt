// expect: arc
use std::sync::Arc;

fn shared() -> Arc<u32> {
    Arc::new(1)
}
