// expect: move_closure, closure
fn size_of(name: String) -> usize {
    let size = move || name.len();
    size()
}
