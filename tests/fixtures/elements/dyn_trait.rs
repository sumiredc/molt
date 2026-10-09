// expect: dyn_trait, trait_def, shared_ref
trait Shape {
    fn sides(&self) -> u32;
}

fn count(shape: &dyn Shape) -> u32 {
    shape.sides()
}
