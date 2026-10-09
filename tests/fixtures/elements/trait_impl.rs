// expect: trait_impl, trait_def
trait Shape {
    fn sides() -> u32;
}

struct Square;

impl Shape for Square {
    fn sides() -> u32 {
        4
    }
}
