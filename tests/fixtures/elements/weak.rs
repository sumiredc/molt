// expect: weak, rc, shared_ref
use std::rc::{Rc, Weak};

fn observe(value: Rc<u32>) -> Weak<u32> {
    Rc::downgrade(&value)
}
