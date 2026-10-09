// expect: associated_type, trait_def, shared_ref
trait Container {
    type Item;

    fn first(&self) -> Self::Item;
}
