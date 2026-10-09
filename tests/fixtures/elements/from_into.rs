// expect: from_into, trait_impl
struct Meters(u32);

impl From<u32> for Meters {
    fn from(value: u32) -> Self {
        Meters(value)
    }
}

fn meters(n: u32) -> Meters {
    n.into()
}
