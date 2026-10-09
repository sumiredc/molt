// expect: async_fn_in_trait, async_fn, trait_def, shared_ref
trait Store {
    async fn load(&self) -> u32;
}
