//! Ctrl-C while waiting for the model (PRD §8.1). The request is dropped and the caller gets
//! [`ChatError::Interrupted`](super::ChatError::Interrupted); Ctrl-C at any other time ends molt
//! as before.
//!
//! Ctrl-C at a prompt does not come here: rustyline turns the terminal's signals off while it
//! reads a line, so Ctrl-C is a key there. While reading, rustyline also puts its own SIGINT
//! action in place and then restores the one it found, so [`Interrupt::on_sigint`] has to run
//! before the first line is read, or rustyline would restore the default over it.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use signal_hook::consts::SIGINT;
use signal_hook::flag;

/// How often the wait looks at whether Ctrl-C came.
const POLL: Duration = Duration::from_millis(50);

/// Whether Ctrl-C came while waiting for the model.
#[derive(Debug, Clone)]
pub(crate) struct Interrupt {
    /// True unless a wait is going on. SIGINT then does what it does by default: it ends molt.
    idle: Arc<AtomicBool>,
    /// Set by SIGINT during a wait.
    interrupted: Arc<AtomicBool>,
}

impl Interrupt {
    /// An interrupt that nothing raises but [`Interrupt::raise`].
    pub(crate) fn new() -> Self {
        Self {
            idle: Arc::new(AtomicBool::new(true)),
            interrupted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// An interrupt raised by SIGINT (Ctrl-C) during a wait. Outside a wait, SIGINT still ends
    /// the process by its default action, so a shell sees it end by the signal. The handlers stay
    /// for the rest of the process.
    pub(crate) fn on_sigint() -> io::Result<Self> {
        let interrupt = Self::new();
        // Run in the order registered: outside a wait the first ends the process.
        flag::register_conditional_default(SIGINT, Arc::clone(&interrupt.idle))?;
        flag::register(SIGINT, Arc::clone(&interrupt.interrupted))?;
        Ok(interrupt)
    }

    /// Raises the interrupt, as SIGINT does during a wait.
    #[cfg(test)]
    pub(crate) fn raise(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
    }

    /// Runs `work` on its own thread and waits for its result. Returns `None` as soon as the
    /// interrupt is raised during the wait, including right as the result came; `work` is then
    /// left to finish on its own and its result is dropped.
    pub(crate) fn wait<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        let _armed = Armed::new(self);
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            // The receiver is gone when the wait was interrupted.
            let _ = sender.send(work());
        });
        loop {
            if self.interrupted.load(Ordering::SeqCst) {
                return None;
            }
            match receiver.recv_timeout(POLL) {
                Ok(result) if self.interrupted.load(Ordering::SeqCst) => {
                    drop(result);
                    return None;
                }
                Ok(result) => return Some(result),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => panic!("the request thread panicked"),
            }
        }
    }
}

/// A wait going on. It starts by forgetting an interrupt raised before it, so an interrupted
/// wait does not reach the next one. Dropping it ends the wait.
struct Armed<'a>(&'a Interrupt);

impl<'a> Armed<'a> {
    fn new(interrupt: &'a Interrupt) -> Self {
        interrupt.interrupted.store(false, Ordering::SeqCst);
        interrupt.idle.store(false, Ordering::SeqCst);
        Self(interrupt)
    }
}

impl Drop for Armed<'_> {
    fn drop(&mut self) {
        self.0.idle.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn a_wait_returns_the_result_of_the_work() {
        let interrupt = Interrupt::new();
        assert_eq!(interrupt.wait(|| 7), Some(7));
    }

    #[test]
    fn raising_the_interrupt_during_a_wait_returns_none_without_waiting_for_the_work() {
        let interrupt = Interrupt::new();
        let raiser = interrupt.clone();
        let started = Instant::now();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            raiser.raise();
        });

        let result = interrupt.wait(|| {
            thread::sleep(Duration::from_secs(5));
            7
        });

        assert_eq!(result, None);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn an_interrupt_raised_as_the_result_comes_drops_the_result() {
        let interrupt = Interrupt::new();
        let raiser = interrupt.clone();
        assert_eq!(
            interrupt.wait(move || {
                // Let the wait block on the result first, so the result wakes it.
                thread::sleep(Duration::from_millis(120));
                raiser.raise();
                7
            }),
            None
        );
    }

    #[test]
    fn an_interrupted_wait_does_not_interrupt_the_next_one() {
        let interrupt = Interrupt::new();
        let raiser = interrupt.clone();
        interrupt.wait(move || raiser.raise());

        assert_eq!(interrupt.wait(|| 7), Some(7));
    }

    #[test]
    fn an_interrupt_raised_outside_a_wait_does_not_interrupt_the_next_one() {
        let interrupt = Interrupt::new();
        interrupt.raise();

        assert_eq!(interrupt.wait(|| 7), Some(7));
    }

    #[test]
    fn sigint_keeps_its_default_action_outside_a_wait_only() {
        let interrupt = Interrupt::new();
        let during = interrupt.clone();
        assert!(interrupt.idle.load(Ordering::SeqCst));

        let idle_during = interrupt.wait(move || during.idle.load(Ordering::SeqCst));

        assert_eq!(idle_during, Some(false));
        assert!(interrupt.idle.load(Ordering::SeqCst));
    }

    /// Set in the child process of the test below.
    #[cfg(unix)]
    const CHILD: &str = "MOLT_TEST_SIGINT_OUTSIDE_A_WAIT";

    /// A real SIGINT outside a wait, with the handlers registered, ends the process by the signal.
    /// The test runs itself again in a child process, which sends SIGINT to itself.
    #[cfg(unix)]
    #[test]
    fn a_sigint_outside_a_wait_ends_the_process_by_the_signal() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::Command;

        const NAME: &str =
            "llm::interrupt::tests::a_sigint_outside_a_wait_ends_the_process_by_the_signal";
        if std::env::var_os(CHILD).is_some() {
            let _interrupt = Interrupt::on_sigint().unwrap();
            Command::new("kill")
                .args(["-INT", &std::process::id().to_string()])
                .status()
                .unwrap();
            thread::sleep(Duration::from_secs(5));
            return;
        }

        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .unwrap()
            .status;

        assert_eq!(status.signal(), Some(SIGINT), "{status:?}");
    }
}
