// expect: unsafe_trait, trait_def, trait_impl
unsafe trait Zeroable {}

unsafe impl Zeroable for u32 {}
