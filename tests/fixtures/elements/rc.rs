// expect: rc
use std::rc::Rc;

fn shared() -> Rc<u32> {
    Rc::new(1)
}
