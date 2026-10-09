// expect: lazy_lock
use std::sync::LazyLock;

fn build() -> u32 {
    42
}

static ANSWER: LazyLock<u32> = LazyLock::new(build);
