// expect: thread_scope, lifetime_param, lifetime_annotation, shared_ref
use std::thread::{self, Scope};

fn work<'scope, 'env>(scope: &'scope Scope<'scope, 'env>) {
    let _ = scope;
}

fn run() {
    thread::scope(work);
}
