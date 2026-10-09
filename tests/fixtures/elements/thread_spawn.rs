// expect: thread_spawn
use std::thread;

fn work() {}

fn start() {
    thread::spawn(work);
}
