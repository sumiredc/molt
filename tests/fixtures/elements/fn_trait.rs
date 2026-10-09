// expect: fn_trait, generic_param, trait_bound
fn apply<F: Fn(u32) -> u32>(f: F, x: u32) -> u32 {
    f(x)
}
