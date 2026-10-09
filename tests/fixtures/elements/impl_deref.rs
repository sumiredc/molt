// expect: impl_deref, trait_impl, associated_type, shared_ref
use std::ops::Deref;

struct Meters(u32);

impl Deref for Meters {
    type Target = u32;

    fn deref(&self) -> &u32 {
        &self.0
    }
}
