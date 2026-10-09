// expect: cell
use std::cell::Cell;

fn counter() -> Cell<u32> {
    Cell::new(0)
}
