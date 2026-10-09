// expect: ref_cell
use std::cell::RefCell;

fn log() -> RefCell<Vec<String>> {
    RefCell::new(Vec::new())
}
