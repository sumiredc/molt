// expect: impl_drop, trait_impl, mut_ref
struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {}
}
