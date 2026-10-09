// expect: await, async_fn
async fn fetch() -> u32 {
    1
}

async fn twice() -> u32 {
    fetch().await * 2
}
