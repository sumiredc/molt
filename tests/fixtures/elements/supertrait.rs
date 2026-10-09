// expect: supertrait, trait_def, shared_ref
trait Named {
    fn name(&self) -> String;
}

trait Greeter: Named {
    fn greet(&self) -> String;
}
