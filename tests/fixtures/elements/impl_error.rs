// expect: impl_error, trait_impl, derive, shared_ref, mut_ref, result, format_macro
use std::fmt;

#[derive(Debug)]
struct Failure;

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "failure")
    }
}

impl std::error::Error for Failure {}
