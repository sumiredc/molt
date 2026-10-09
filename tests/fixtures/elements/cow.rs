// expect: cow, shared_ref
use std::borrow::Cow;

fn label(text: &str) -> Cow<str> {
    Cow::Borrowed(text)
}
