// expect: channel
use std::sync::mpsc;

fn pipe() -> (mpsc::Sender<u32>, mpsc::Receiver<u32>) {
    mpsc::channel()
}
