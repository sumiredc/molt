// expect: impl_iterator, trait_impl, associated_type, mut_ref, option
struct Countdown(u32);

impl Iterator for Countdown {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        None
    }
}
